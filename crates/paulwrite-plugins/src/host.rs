//! Machine virtuelle Lua, API `paul.*` et boucle du thread de plugins.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};
use mlua::{
    Error as LuaError, Function, HookTriggers, Lua, LuaOptions, MultiValue, StdLib, Table, Value,
    VmState,
};
use ropey::Rope;

/// Durée maximale d'un appel de plugin (chargement, commande ou événement). Au-delà, l'appel est
/// interrompu avec une erreur : une boucle infinie ne bloque pas les autres plugins.
pub const CALL_BUDGET: Duration = Duration::from_secs(2);
/// Mémoire maximale de la machine virtuelle Lua.
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// Taille maximale du texte renvoyé par `paul.text` / `paul.selection`.
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// Ce qu'un plugin demande à l'éditeur de faire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Insérer ce texte à chaque curseur (remplace la sélection).
    Insert(String),
    /// Afficher un message bref.
    Message(String),
    /// Lancer une commande intégrée par son nom (`edit.undo`, `file.save`…).
    Run(String),
    /// Aller à cette ligne (à partir de 1).
    GotoLine(usize),
}

/// Une commande déclarée par un plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    /// Nom stable (`mon_plugin.bonjour`).
    pub name: String,
    /// Intitulé affiché dans la palette.
    pub title: String,
}

/// Instantané de l'éditeur remis au plugin pendant un appel. Lecture seule.
#[derive(Clone, Debug, Default)]
pub struct EditorContext {
    pub path: Option<String>,
    /// Copie O(1) du texte (ropey partage sa structure).
    pub rope: Rope,
    /// Texte sélectionné (tronqué à 4 Mo).
    pub selection: String,
    /// Position du curseur principal : ligne (à partir de 1) et colonne (à partir de 1).
    pub line: usize,
    pub column: usize,
}

/// Messages envoyés au thread de plugins.
#[derive(Debug)]
pub enum Request {
    /// Exécuter la commande `command` ; la réponse porte le même `id`.
    Invoke {
        id: u64,
        command: String,
        ctx: EditorContext,
    },
    /// Un événement de l'éditeur (`open`, `save`, `close`).
    Event { name: String, ctx: EditorContext },
    /// Recharger tous les plugins depuis le disque.
    Reload,
}

/// Bilan d'un chargement de plugins.
#[derive(Clone, Debug, Default)]
pub struct PluginReport {
    /// Nombre de fichiers de plugin chargés sans erreur.
    pub loaded: usize,
    /// Commandes déclarées par l'ensemble des plugins.
    pub commands: Vec<CommandInfo>,
    /// Erreurs de chargement (une ligne par erreur, avec le nom du plugin).
    pub errors: Vec<String>,
    /// Durée du chargement (création de la machine virtuelle comprise).
    pub load_time: Duration,
}

/// Messages renvoyés par le thread de plugins.
#[derive(Debug)]
pub enum Response {
    /// Plugins (re)chargés.
    Loaded(PluginReport),
    /// Fin d'une commande (`id` de la requête) ou d'un événement (`id` = 0).
    Done {
        id: u64,
        actions: Vec<Action>,
        error: Option<String>,
    },
}

/// État partagé entre l'API Lua et la boucle (tout sur le thread de plugins).
#[derive(Default)]
struct Shared {
    commands: RefCell<Vec<(CommandInfo, Function)>>,
    events: RefCell<HashMap<String, Vec<Function>>>,
    actions: RefCell<Vec<Action>>,
    ctx: RefCell<Option<EditorContext>>,
    deadline: Cell<Option<Instant>>,
    /// Plugin en cours de chargement (pour attribuer les commandes et les erreurs).
    current_plugin: RefCell<String>,
    reserved: HashSet<String>,
}

fn truncate_utf8(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Crée la machine virtuelle : bibliothèques sûres, `os` réduit à l'heure, API `paul`.
/// Renvoie la machine et la table de base (visible de tous les plugins).
fn build_vm(shared: &Rc<Shared>) -> Result<(Lua, Table), LuaError> {
    let lua = Lua::new_with(
        StdLib::COROUTINE
            | StdLib::TABLE
            | StdLib::STRING
            | StdLib::UTF8
            | StdLib::MATH
            | StdLib::OS,
        LuaOptions::default(),
    )?;
    lua.set_memory_limit(MEMORY_LIMIT)?;

    // Limite de temps : la fonction de surveillance vérifie l'échéance toutes les 10 000
    // instructions de la machine virtuelle.
    let sh = shared.clone();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(10_000),
        move |_, _| match sh.deadline.get() {
            Some(d) if Instant::now() > d => Err(LuaError::runtime(format!(
                "délai dépassé ({} s) : appel interrompu",
                CALL_BUDGET.as_secs()
            ))),
            _ => Ok(VmState::Continue),
        },
    )?;

    let globals = lua.globals();
    // `os` : seulement l'heure. Pas d'`execute`, `exit`, `remove`, `getenv`…
    let full_os: Table = globals.get("os")?;
    let os = lua.create_table()?;
    for key in ["time", "clock", "date", "difftime"] {
        os.set(key, full_os.get::<Value>(key)?)?;
    }
    // Table de base : tout ce qu'un plugin voit.
    let base = lua.create_table()?;
    for key in [
        "assert",
        "error",
        "ipairs",
        "next",
        "pairs",
        "pcall",
        "select",
        "tonumber",
        "tostring",
        "type",
        "xpcall",
        "rawequal",
        "rawget",
        "rawset",
        "rawlen",
        "setmetatable",
        "getmetatable",
        "coroutine",
        "table",
        "string",
        "utf8",
        "math",
    ] {
        base.set(key, globals.get::<Value>(key)?)?;
    }
    base.set("os", os)?;
    // Retire tout du global : les plugins n'y accèdent que par leur environnement.
    let keys: Vec<String> = globals
        .pairs::<String, Value>()
        .filter_map(|p| p.ok().map(|(k, _)| k))
        .collect();
    for k in keys {
        globals.set(k, Value::Nil)?;
    }

    let paul = lua.create_table()?;

    // paul.command(nom, titre, fonction)
    let sh = shared.clone();
    paul.set(
        "command",
        lua.create_function(move |_, (name, title, func): (String, String, Function)| {
            if sh.reserved.contains(&name) {
                return Err(LuaError::runtime(format!(
                    "« {name} » est le nom d'une commande intégrée"
                )));
            }
            let mut cmds = sh.commands.borrow_mut();
            if cmds.iter().any(|(c, _)| c.name == name) {
                return Err(LuaError::runtime(format!(
                    "commande « {name} » déjà déclarée"
                )));
            }
            cmds.push((CommandInfo { name, title }, func));
            Ok(())
        })?,
    )?;

    // paul.on(evenement, fonction)
    let sh = shared.clone();
    paul.set(
        "on",
        lua.create_function(move |_, (event, func): (String, Function)| {
            if !matches!(event.as_str(), "open" | "save" | "close") {
                return Err(LuaError::runtime(format!(
                    "événement inconnu « {event} » (open, save ou close)"
                )));
            }
            sh.events.borrow_mut().entry(event).or_default().push(func);
            Ok(())
        })?,
    )?;

    // Actions
    let push = |lua: &Lua,
                shared: &Rc<Shared>,
                name: &str,
                make: fn(&Lua, MultiValue) -> Result<Action, LuaError>| {
        let sh = shared.clone();
        lua.create_function(move |lua, args: MultiValue| {
            sh.actions.borrow_mut().push(make(lua, args)?);
            Ok(())
        })
        .map(|f| (name.to_owned(), f))
    };
    let text_arg = |args: &MultiValue| -> Result<String, LuaError> {
        match args.iter().next() {
            Some(Value::String(s)) => Ok(s.to_str()?.to_string()),
            Some(Value::Integer(i)) => Ok(i.to_string()),
            Some(Value::Number(n)) => Ok(n.to_string()),
            _ => Err(LuaError::runtime("un texte est attendu")),
        }
    };
    let _ = text_arg;
    for (name, f) in [
        push(&lua, shared, "insert", |_, a| match a.iter().next() {
            Some(Value::String(s)) => Ok(Action::Insert(s.to_str()?.to_string())),
            _ => Err(LuaError::runtime("paul.insert attend un texte")),
        })?,
        push(&lua, shared, "message", |_, a| match a.iter().next() {
            Some(Value::String(s)) => Ok(Action::Message(s.to_str()?.to_string())),
            Some(Value::Integer(i)) => Ok(Action::Message(i.to_string())),
            Some(Value::Number(n)) => Ok(Action::Message(n.to_string())),
            _ => Err(LuaError::runtime("paul.message attend un texte")),
        })?,
        push(&lua, shared, "run", |_, a| match a.iter().next() {
            Some(Value::String(s)) => Ok(Action::Run(s.to_str()?.to_string())),
            _ => Err(LuaError::runtime("paul.run attend le nom d'une commande")),
        })?,
        push(&lua, shared, "goto_line", |_, a| match a.iter().next() {
            Some(Value::Integer(i)) if *i >= 1 => Ok(Action::GotoLine(*i as usize)),
            Some(Value::Number(n)) if *n >= 1.0 => Ok(Action::GotoLine(*n as usize)),
            _ => Err(LuaError::runtime(
                "paul.goto_line attend un numéro de ligne >= 1",
            )),
        })?,
    ] {
        paul.set(name, f)?;
    }

    // Lecture de l'éditeur (valide seulement pendant un appel)
    macro_rules! reader {
        ($name:literal, $sh:ident, |$ctx:ident| $body:expr) => {{
            let $sh = shared.clone();
            paul.set(
                $name,
                lua.create_function(move |_, ()| {
                    let guard = $sh.ctx.borrow();
                    let $ctx = guard.as_ref().ok_or_else(|| {
                        LuaError::runtime("pas d'éditeur disponible dans ce contexte")
                    })?;
                    Ok($body)
                })?,
            )?;
        }};
    }
    reader!("path", sh, |c| c.path.clone());
    reader!("selection", sh, |c| c.selection.clone());
    reader!("line_count", sh, |c| c.rope.len_lines() as i64);
    reader!("cursor", sh, |c| (c.line as i64, c.column as i64));

    // paul.line(n) : texte de la ligne n (à partir de 1), sans fin de ligne ; nil hors du document
    let sh = shared.clone();
    paul.set(
        "line",
        lua.create_function(move |_, n: i64| {
            let guard = sh.ctx.borrow();
            let c = guard
                .as_ref()
                .ok_or_else(|| LuaError::runtime("pas d'éditeur disponible dans ce contexte"))?;
            if n < 1 || n as usize > c.rope.len_lines() {
                return Ok(None);
            }
            let line = c.rope.line(n as usize - 1).to_string();
            Ok(Some(line.trim_end_matches(['\n', '\r']).to_owned()))
        })?,
    )?;
    // paul.text(premiere, derniere) : lignes premiere..derniere jointes par des sauts de ligne
    let sh = shared.clone();
    paul.set(
        "text",
        lua.create_function(move |_, (first, last): (Option<i64>, Option<i64>)| {
            let guard = sh.ctx.borrow();
            let c = guard
                .as_ref()
                .ok_or_else(|| LuaError::runtime("pas d'éditeur disponible dans ce contexte"))?;
            let total = c.rope.len_lines() as i64;
            let (a, b) = (first.unwrap_or(1).max(1), last.unwrap_or(total).min(total));
            if a > b {
                return Ok(String::new());
            }
            let start = c.rope.line_to_char(a as usize - 1);
            let end = if b >= total {
                c.rope.len_chars()
            } else {
                c.rope.line_to_char(b as usize)
            };
            Ok(truncate_utf8(
                c.rope.slice(start..end).to_string(),
                MAX_TEXT_BYTES,
            ))
        })?,
    )?;

    // paul.log(...) : écrit sur la sortie d'erreur (visible en lançant paulwrite depuis un terminal)
    paul.set(
        "log",
        lua.create_function(|_, args: MultiValue| {
            let line: Vec<String> = args
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.to_string_lossy().to_string(),
                    other => format!("{other:?}"),
                })
                .collect();
            eprintln!("[plugin] {}", line.join(" "));
            Ok(())
        })?,
    )?;
    base.set("paul", paul)?;
    Ok((lua, base))
}

/// Exécute `f` avec une échéance de [`CALL_BUDGET`].
fn with_budget<R>(shared: &Shared, f: impl FnOnce() -> R) -> R {
    shared.deadline.set(Some(Instant::now() + CALL_BUDGET));
    let r = f();
    shared.deadline.set(None);
    r
}

/// Fichiers de plugin d'un dossier : `*.lua` et `*/init.lua`, triés par nom.
fn plugin_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                let init = path.join("init.lua");
                init.is_file().then_some((name, init))
            } else if path.extension().is_some_and(|x| x == "lua") {
                Some((name.trim_end_matches(".lua").to_owned(), path))
            } else {
                None
            }
        })
        .collect();
    files.sort();
    files
}

/// Une machine virtuelle avec ses plugins chargés.
struct Vm {
    lua: Lua,
    shared: Rc<Shared>,
}

fn load_all(dir: &Path, reserved: &HashSet<String>) -> (Option<Vm>, PluginReport) {
    let started = Instant::now();
    let mut report = PluginReport::default();
    let shared = Rc::new(Shared {
        reserved: reserved.clone(),
        ..Shared::default()
    });
    let (lua, base) = match build_vm(&shared) {
        Ok(x) => x,
        Err(e) => {
            report.errors.push(format!("machine virtuelle Lua : {e}"));
            report.load_time = started.elapsed();
            return (None, report);
        }
    };
    for (name, path) in plugin_files(dir) {
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                report
                    .errors
                    .push(format!("{name} : lecture impossible ({e})"));
                continue;
            }
        };
        *shared.current_plugin.borrow_mut() = name.clone();
        // Environnement propre au plugin : ses variables globales ne sortent pas de là.
        let result = (|| -> Result<(), LuaError> {
            let env = lua.create_table()?;
            let mt = lua.create_table()?;
            mt.set("__index", base.clone())?;
            env.set_metatable(Some(mt))?;
            with_budget(&shared, || {
                lua.load(&src)
                    .set_name(format!("@{name}"))
                    .set_environment(env)
                    .exec()
            })
        })();
        match result {
            Ok(()) => report.loaded += 1,
            Err(e) => report.errors.push(format!("{name} : {e}")),
        }
    }
    report.commands = shared
        .commands
        .borrow()
        .iter()
        .map(|(c, _)| c.clone())
        .collect();
    report.load_time = started.elapsed();
    (Some(Vm { lua, shared }), report)
}

/// Appelle `func` avec le contexte `ctx` et renvoie les actions produites.
fn call(
    vm: &Vm,
    func: &Function,
    args: impl mlua::IntoLuaMulti,
    ctx: EditorContext,
) -> (Vec<Action>, Option<String>) {
    *vm.shared.ctx.borrow_mut() = Some(ctx);
    vm.shared.actions.borrow_mut().clear();
    let result = with_budget(&vm.shared, || func.call::<()>(args));
    *vm.shared.ctx.borrow_mut() = None;
    let actions = std::mem::take(&mut *vm.shared.actions.borrow_mut());
    (actions, result.err().map(|e| e.to_string()))
}

/// Démarre le thread de plugins : charge les plugins de `dir` (lecture des fichiers comprise, hors
/// du thread d'interface), répond [`Response::Loaded`], puis traite les [`Request`]s.
/// `reserved` : noms de commandes intégrées, interdits aux plugins. `notify` est appelé depuis le
/// thread de plugins pour chaque réponse.
pub fn spawn(
    dir: PathBuf,
    reserved: Vec<String>,
    notify: impl Fn(Response) + Send + 'static,
) -> Sender<Request> {
    let (tx, rx) = unbounded::<Request>();
    let reserved: HashSet<String> = reserved.into_iter().collect();
    thread::Builder::new()
        .name("paulwrite-plugins".into())
        .spawn(move || {
            let (mut vm, report) = load_all(&dir, &reserved);
            notify(Response::Loaded(report));
            for req in rx {
                match req {
                    Request::Reload => {
                        drop(vm.take());
                        let (new_vm, report) = load_all(&dir, &reserved);
                        vm = new_vm;
                        notify(Response::Loaded(report));
                    }
                    Request::Invoke { id, command, ctx } => {
                        let Some(vm) = vm.as_ref() else {
                            notify(Response::Done {
                                id,
                                actions: Vec::new(),
                                error: Some("plugins indisponibles".into()),
                            });
                            continue;
                        };
                        let func = vm
                            .shared
                            .commands
                            .borrow()
                            .iter()
                            .find(|(c, _)| c.name == command)
                            .map(|(_, f)| f.clone());
                        let (actions, error) = match func {
                            Some(f) => call(vm, &f, (), ctx),
                            None => (Vec::new(), Some(format!("commande inconnue : {command}"))),
                        };
                        notify(Response::Done { id, actions, error });
                    }
                    Request::Event { name, ctx } => {
                        let Some(vm) = vm.as_ref() else {
                            continue;
                        };
                        let handlers: Vec<Function> = vm
                            .shared
                            .events
                            .borrow()
                            .get(&name)
                            .cloned()
                            .unwrap_or_default();
                        if handlers.is_empty() {
                            continue;
                        }
                        let mut all_actions = Vec::new();
                        let mut errors = Vec::new();
                        for h in handlers {
                            let ev = vm.lua.create_table().ok();
                            if let Some(ev) = &ev {
                                let _ = ev.set("name", name.clone());
                                let _ = ev.set("path", ctx.path.clone());
                            }
                            let (actions, error) = call(vm, &h, ev, ctx.clone());
                            all_actions.extend(actions);
                            errors.extend(error);
                        }
                        notify(Response::Done {
                            id: 0,
                            actions: all_actions,
                            error: (!errors.is_empty()).then(|| errors.join("\n")),
                        });
                    }
                }
            }
        })
        .expect("thread de plugins");
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Mutex;

    struct Harness {
        tx: Sender<Request>,
        rx: mpsc::Receiver<Response>,
        _dir: tempfile::TempDir,
    }

    fn start(files: &[(&str, &str)]) -> (Harness, PluginReport) {
        let dir = tempfile::tempdir().unwrap();
        for (name, src) in files {
            let p = dir.path().join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, src).unwrap();
        }
        let (otx, rx) = mpsc::channel();
        let otx = Mutex::new(otx);
        let tx = spawn(
            dir.path().to_path_buf(),
            vec!["edit.undo".to_owned(), "file.save".to_owned()],
            move |r| {
                let _ = otx.lock().unwrap().send(r);
            },
        );
        let report = match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Response::Loaded(r) => r,
            other => panic!("réponse inattendue : {other:?}"),
        };
        (Harness { tx, rx, _dir: dir }, report)
    }

    fn ctx(text: &str) -> EditorContext {
        EditorContext {
            path: Some("dossier/notes.txt".into()),
            rope: Rope::from_str(text),
            selection: "sél".into(),
            line: 2,
            column: 3,
        }
    }

    fn invoke(h: &Harness, command: &str, text: &str) -> (Vec<Action>, Option<String>) {
        h.tx.send(Request::Invoke {
            id: 7,
            command: command.into(),
            ctx: ctx(text),
        })
        .unwrap();
        match h.rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Response::Done { id, actions, error } => {
                assert_eq!(id, 7);
                (actions, error)
            }
            other => panic!("réponse inattendue : {other:?}"),
        }
    }

    #[test]
    fn commande_lecture_de_l_editeur_et_actions() {
        let (h, report) = start(&[(
            "bonjour.lua",
            r#"
            paul.command("bonjour.dire", "Bonjour : dire", function()
                local l, c = paul.cursor()
                paul.message("ligne " .. l .. " col " .. c .. " de " .. paul.line_count())
                paul.insert(paul.line(2) .. "|" .. paul.selection() .. "|" .. paul.path())
                paul.run("edit.undo")
                paul.goto_line(3)
            end)
            paul.command("bonjour.texte", "Bonjour : texte", function()
                paul.insert(paul.text(2, 3))
                paul.insert(tostring(paul.line(99)))
            end)
            "#,
        )]);
        assert_eq!(report.loaded, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.commands.len(), 2);
        assert_eq!(report.commands[0].title, "Bonjour : dire");

        let (actions, error) = invoke(&h, "bonjour.dire", "un\ndeux\ntrois\n");
        assert_eq!(error, None);
        assert_eq!(
            actions,
            vec![
                Action::Message("ligne 2 col 3 de 4".into()),
                Action::Insert("deux|sél|dossier/notes.txt".into()),
                Action::Run("edit.undo".into()),
                Action::GotoLine(3),
            ]
        );
        let (actions, _) = invoke(&h, "bonjour.texte", "un\ndeux\ntrois");
        assert_eq!(
            actions,
            vec![
                Action::Insert("deux\ntrois".into()),
                Action::Insert("nil".into())
            ]
        );
    }

    #[test]
    fn un_plugin_en_erreur_n_empeche_pas_les_autres() {
        let (h, report) = start(&[
            ("a_casse.lua", "ceci n'est pas du lua ("),
            ("b_plante.lua", "error('boum')"),
            (
                "c_ok.lua",
                r#"paul.command("c.ok", "C", function() paul.message("ok") end)"#,
            ),
        ]);
        assert_eq!(report.loaded, 1);
        assert_eq!(report.errors.len(), 2);
        assert!(report.errors.iter().any(|e| e.starts_with("a_casse")));
        assert!(report.errors.iter().any(|e| e.contains("boum")));
        let (actions, error) = invoke(&h, "c.ok", "");
        assert_eq!((actions, error), (vec![Action::Message("ok".into())], None));
        // commande inconnue : erreur, pas de panique
        let (_, error) = invoke(&h, "n.existe.pas", "");
        assert!(error.unwrap().contains("inconnue"));
    }

    #[test]
    fn erreur_dans_une_commande_est_renvoyee() {
        let (h, _) = start(&[(
            "p.lua",
            r#"paul.command("p.erreur", "E", function() paul.insert("avant"); error("raté") end)"#,
        )]);
        let (actions, error) = invoke(&h, "p.erreur", "");
        assert!(error.unwrap().contains("raté"));
        // les actions déjà demandées avant l'erreur sont conservées
        assert_eq!(actions, vec![Action::Insert("avant".into())]);
    }

    #[test]
    fn bac_a_sable() {
        let (h, report) = start(&[(
            "s.lua",
            r#"
            paul.command("s.verif", "V", function()
                local interdits = {
                    io = io, debug = debug, package = package, require = require,
                    dofile = dofile, loadfile = loadfile, load = load, collectgarbage = collectgarbage,
                    os_execute = os.execute, os_exit = os.exit, os_remove = os.remove,
                    os_getenv = os.getenv, os_rename = os.rename,
                }
                local trouves = {}
                for k, v in pairs(interdits) do if v ~= nil then trouves[#trouves + 1] = k end end
                table.sort(trouves)
                paul.message(table.concat(trouves, ","))
                paul.message(type(os.time()) .. type(os.clock()) .. type(string.upper) .. type(math.floor))
            end)
            "#,
        )]);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let (actions, error) = invoke(&h, "s.verif", "");
        assert_eq!(error, None);
        assert_eq!(
            actions,
            vec![
                Action::Message(String::new()),
                Action::Message("numbernumberfunctionfunction".into())
            ]
        );
    }

    #[test]
    fn les_plugins_ont_chacun_leurs_globales() {
        let (h, _) = start(&[
            (
                "a.lua",
                r#"x = "a"; paul.command("a.x", "A", function() paul.message(x) end)"#,
            ),
            (
                "b.lua",
                r#"x = "b"; paul.command("b.x", "B", function() paul.message(x) end)"#,
            ),
        ]);
        assert_eq!(invoke(&h, "a.x", "").0, vec![Action::Message("a".into())]);
        assert_eq!(invoke(&h, "b.x", "").0, vec![Action::Message("b".into())]);
    }

    #[test]
    fn noms_reserves_et_doublons() {
        let (_, report) = start(&[
            (
                "a.lua",
                r#"paul.command("edit.undo", "Pirate", function() end)"#,
            ),
            (
                "b.lua",
                r#"
                paul.command("b.un", "Un", function() end)
                paul.command("b.un", "Deux", function() end)
                "#,
            ),
        ]);
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        assert!(report.errors[0].contains("intégrée"));
        assert!(report.errors[1].contains("déjà déclarée"));
    }

    #[test]
    fn evenements() {
        let (h, _) = start(&[(
            "e.lua",
            r#"
            paul.on("save", function(ev) paul.message("sauvé " .. ev.path .. " " .. ev.name) end)
            paul.on("open", function(ev) paul.insert("ouvert") end)
            "#,
        )]);
        h.tx.send(Request::Event {
            name: "save".into(),
            ctx: ctx(""),
        })
        .unwrap();
        match h.rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Response::Done { id, actions, error } => {
                assert_eq!(id, 0);
                assert_eq!(error, None);
                assert_eq!(
                    actions,
                    vec![Action::Message("sauvé dossier/notes.txt save".into())]
                );
            }
            other => panic!("{other:?}"),
        }
        // événement sans abonné : aucune réponse
        h.tx.send(Request::Event {
            name: "close".into(),
            ctx: ctx(""),
        })
        .unwrap();
        assert!(h.rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn evenement_inconnu_refuse_au_chargement() {
        let (_, report) = start(&[("e.lua", r#"paul.on("change", function() end)"#)]);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("événement inconnu"));
    }

    #[test]
    fn boucle_infinie_interrompue() {
        let (h, report) = start(&[
            ("a_boucle_au_chargement.lua", "while true do end"),
            (
                "b.lua",
                r#"paul.command("b.boucle", "B", function() while true do end end)
                   paul.command("b.vite", "V", function() paul.message("vite") end)"#,
            ),
        ]);
        assert!(
            report.errors.iter().any(|e| e.contains("délai dépassé")),
            "{:?}",
            report.errors
        );
        assert!(report.load_time >= CALL_BUDGET);
        let started = Instant::now();
        let (_, error) = invoke(&h, "b.boucle", "");
        assert!(error.unwrap().contains("délai dépassé"));
        assert!(started.elapsed() < CALL_BUDGET + Duration::from_secs(2));
        // la machine virtuelle reste utilisable après l'interruption
        assert_eq!(
            invoke(&h, "b.vite", "").0,
            vec![Action::Message("vite".into())]
        );
    }

    #[test]
    fn memoire_plafonnee() {
        let (h, _) = start(&[(
            "m.lua",
            r#"paul.command("m.gros", "G", function()
                local t = {}
                for i = 1, 1e9 do t[i] = string.rep("x", 1000) .. i end
            end)"#,
        )]);
        let (_, error) = invoke(&h, "m.gros", "");
        let e = error.expect("doit échouer");
        assert!(
            e.contains("memory") || e.contains("mémoire") || e.contains("délai"),
            "{e}"
        );
    }

    #[test]
    fn rechargement_et_plugins_en_dossier() {
        let (h, report) = start(&[(
            "dossier_plugin/init.lua",
            r#"paul.command("d.un", "D", function() end)"#,
        )]);
        assert_eq!(report.commands.len(), 1);
        std::fs::write(
            h._dir.path().join("nouveau.lua"),
            r#"paul.command("n.deux", "N", function() paul.message("recharge") end)"#,
        )
        .unwrap();
        h.tx.send(Request::Reload).unwrap();
        let report = match h.rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Response::Loaded(r) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(report.loaded, 2);
        assert_eq!(report.commands.len(), 2);
        assert_eq!(
            invoke(&h, "n.deux", "").0,
            vec![Action::Message("recharge".into())]
        );
    }

    #[test]
    fn dossier_absent_ou_vide() {
        let (otx, rx) = mpsc::channel();
        let otx = Mutex::new(otx);
        let _tx = spawn(PathBuf::from("Z:\\n'existe\\pas"), Vec::new(), move |r| {
            let _ = otx.lock().unwrap().send(r);
        });
        match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Response::Loaded(r) => {
                assert_eq!((r.loaded, r.commands.len(), r.errors.len()), (0, 0, 0));
                assert!(r.load_time < Duration::from_secs(1));
            }
            other => panic!("{other:?}"),
        }
    }
}
