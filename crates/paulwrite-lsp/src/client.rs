//! Client LSP : un thread qui pilote les serveurs de langage (un processus par langage, lancé à la
//! première ouverture d'un document de ce langage) et traduit leurs messages en [`Event`]s.
//!
//! Le thread d'interface n'écrit ni ne lit jamais dans un tuyau : il envoie des [`Request`]s et
//! reçoit des [`Event`]s. Un serveur lent ou bloqué ne peut donc pas figer l'éditeur.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{select, unbounded, Receiver, Sender};
use paulwrite_core::TextChange;
use ropey::Rope;
use serde_json::{json, Value};

use crate::frame::{read_message, write_message};
use crate::uri::{path_key, path_to_uri, uri_to_path};

/// Commande de lancement d'un serveur de langage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    pub command: String,
    pub args: Vec<String>,
}

impl ServerConfig {
    /// Découpe « rust-analyzer --flag » en commande et arguments (séparés par des espaces).
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace().map(str::to_owned);
        let command = parts.next()?;
        Some(Self {
            command,
            args: parts.collect(),
        })
    }
}

/// Gravité d'un diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

/// Un diagnostic (erreur, avertissement…) : plage `(ligne, colonne UTF-16)` et message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub severity: Severity,
    pub message: String,
}

/// Un élément de complétion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    /// Texte à insérer (le libellé si le serveur n'en donne pas d'autre).
    pub insert_text: String,
    pub detail: String,
}

/// Un emplacement dans un fichier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub col16: usize,
}

/// Demandes de l'application.
#[derive(Debug)]
pub enum Request {
    /// Un document s'ouvre (le serveur du langage démarre si besoin). `root` : dossier du projet.
    Open {
        path: PathBuf,
        language: String,
        root: Option<PathBuf>,
        version: i32,
        text: Rope,
    },
    /// Le document a changé : `changes` (dans l'ordre) pour une synchronisation incrémentale,
    /// `text` (copie O(1)) pour un serveur qui ne sait que recevoir le texte entier.
    Change {
        path: PathBuf,
        version: i32,
        changes: Vec<TextChange>,
        text: Rope,
    },
    Save {
        path: PathBuf,
    },
    Close {
        path: PathBuf,
    },
    /// Complétion à la position `(line, col16)` ; la réponse porte le même `id`.
    Completion {
        id: u64,
        path: PathBuf,
        line: usize,
        col16: usize,
    },
    Hover {
        id: u64,
        path: PathBuf,
        line: usize,
        col16: usize,
    },
    Definition {
        id: u64,
        path: PathBuf,
        line: usize,
        col16: usize,
    },
    /// Arrête les serveurs et le thread.
    Shutdown,
}

/// Messages renvoyés à l'application.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Diagnostics {
        path: PathBuf,
        items: Vec<Diagnostic>,
    },
    Completion {
        id: u64,
        items: Vec<CompletionItem>,
    },
    Hover {
        id: u64,
        text: Option<String>,
    },
    Definition {
        id: u64,
        locations: Vec<Location>,
    },
    /// La demande `id` ne peut pas aboutir (pas de serveur pour ce langage, serveur arrêté…).
    Unavailable {
        id: u64,
        reason: String,
    },
    ServerStarted {
        language: String,
    },
    /// Le serveur n'a pas pu être lancé (introuvable…) ou a échoué à l'initialisation.
    ServerError {
        language: String,
        message: String,
    },
    ServerExited {
        language: String,
    },
}

/// Ce qu'on attend en réponse à une requête envoyée au serveur.
enum Pending {
    Initialize,
    Completion(u64),
    Hover(u64),
    Definition(u64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sync {
    None,
    Full,
    Incremental,
}

enum State {
    Starting,
    Ready,
    Dead,
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    state: State,
    sync: Sync,
    next_id: i64,
    pending: HashMap<i64, Pending>,
    /// Messages à envoyer dès que le serveur est prêt (ouvertures et changements, dans l'ordre).
    queued: Vec<Value>,
}

enum FromServer {
    Message { language: String, value: Value },
    Closed { language: String },
}

struct DocInfo {
    language: String,
    uri: String,
    path: PathBuf,
}

struct Client {
    configs: HashMap<String, ServerConfig>,
    servers: HashMap<String, Server>,
    /// Documents ouverts, par clé de chemin (voir [`path_key`]).
    docs: HashMap<String, DocInfo>,
    from_servers: Sender<FromServer>,
    notify: Box<dyn Fn(Event) + Send>,
    shutting_down: bool,
}

fn position(line: usize, col16: usize) -> Value {
    json!({"line": line, "character": col16})
}

fn range(start: (usize, usize), end: (usize, usize)) -> Value {
    json!({"start": position(start.0, start.1), "end": position(end.0, end.1)})
}

fn as_pos(v: &Value) -> Option<(usize, usize)> {
    Some((
        usize::try_from(v.get("line")?.as_u64()?).ok()?,
        usize::try_from(v.get("character")?.as_u64()?).ok()?,
    ))
}

fn as_range(v: &Value) -> Option<((usize, usize), (usize, usize))> {
    Some((as_pos(v.get("start")?)?, as_pos(v.get("end")?)?))
}

/// Diagnostics d'une notification `publishDiagnostics`.
fn parse_diagnostics(params: &Value) -> Vec<Diagnostic> {
    params
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    let (start, end) = as_range(d.get("range")?)?;
                    let severity = match d.get("severity").and_then(Value::as_u64) {
                        Some(2) => Severity::Warning,
                        Some(3) => Severity::Information,
                        Some(4) => Severity::Hint,
                        _ => Severity::Error,
                    };
                    Some(Diagnostic {
                        start,
                        end,
                        severity,
                        message: d.get("message")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_completion(result: &Value) -> Vec<CompletionItem> {
    let items = match result {
        Value::Array(a) => a.as_slice(),
        Value::Object(_) => result
            .get("items")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice),
        _ => &[],
    };
    items
        .iter()
        .filter_map(|i| {
            let label = i.get("label")?.as_str()?.to_owned();
            let insert_text = i
                .pointer("/textEdit/newText")
                .or_else(|| i.get("insertText"))
                .and_then(Value::as_str)
                .map_or_else(|| label.clone(), str::to_owned);
            Some(CompletionItem {
                insert_text,
                detail: i
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                label,
            })
        })
        .collect()
}

/// Texte d'un survol : chaîne, `{kind, value}`, `{language, value}` ou tableau de ces formes.
fn parse_hover(result: &Value) -> Option<String> {
    fn flatten(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| flatten(x, out)),
            Value::Object(_) => {
                if let Some(s) = v.get("value").and_then(Value::as_str) {
                    out.push(s.to_owned());
                }
            }
            _ => {}
        }
    }
    let mut parts = Vec::new();
    flatten(result.get("contents")?, &mut parts);
    let text = parts.join("\n\n");
    (!text.trim().is_empty()).then_some(text)
}

fn parse_locations(result: &Value) -> Vec<Location> {
    let list: Vec<&Value> = match result {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![result],
        _ => Vec::new(),
    };
    list.into_iter()
        .filter_map(|l| {
            // Location {uri, range} ou LocationLink {targetUri, targetSelectionRange}
            let uri = l.get("uri").or_else(|| l.get("targetUri"))?.as_str()?;
            let r = l
                .get("range")
                .or_else(|| l.get("targetSelectionRange"))
                .or_else(|| l.get("targetRange"))?;
            let (start, _) = as_range(r)?;
            Some(Location {
                path: uri_to_path(uri)?,
                line: start.0,
                col16: start.1,
            })
        })
        .collect()
}

impl Client {
    fn emit(&self, event: Event) {
        (self.notify)(event);
    }

    /// Envoie `value` au serveur de `language` (en file d'attente s'il n'est pas encore prêt).
    fn send(&mut self, language: &str, value: Value) {
        let Some(server) = self.servers.get_mut(language) else {
            return;
        };
        match server.state {
            State::Dead => {}
            State::Starting => server.queued.push(value),
            State::Ready => {
                if write_message(&mut server.stdin, &value).is_err() {
                    server.state = State::Dead;
                }
            }
        }
    }

    fn start_server(&mut self, language: &str, root: Option<&Path>) {
        if self.servers.contains_key(language) {
            return;
        }
        let Some(cfg) = self.configs.get(language).cloned() else {
            return;
        };
        let mut command = Command::new(&cfg.command);
        command
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW : pas de fenêtre de console
        }
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.emit(Event::ServerError {
                    language: language.to_owned(),
                    message: format!("{} : {e}", cfg.command),
                });
                return;
            }
        };
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            self.emit(Event::ServerError {
                language: language.to_owned(),
                message: "tuyaux du serveur indisponibles".into(),
            });
            return;
        };
        // Lecture de la sortie du serveur sur un thread : les messages reviennent par un canal.
        let tx = self.from_servers.clone();
        let lang = language.to_owned();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(value)) = read_message(&mut reader) {
                if tx
                    .send(FromServer::Message {
                        language: lang.clone(),
                        value,
                    })
                    .is_err()
                {
                    return;
                }
            }
            let _ = tx.send(FromServer::Closed { language: lang });
        });
        // La sortie d'erreur doit être vidée, sinon un serveur bavard finit par se bloquer.
        thread::spawn(move || {
            let mut sink = [0u8; 4096];
            while stderr.read(&mut sink).is_ok_and(|n| n > 0) {}
        });

        let root_uri = root.map(path_to_uri);
        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "clientInfo": {"name": "PaulWrite"},
                "rootUri": root_uri,
                "capabilities": {
                    "general": {"positionEncodings": ["utf-16"]},
                    "textDocument": {
                        "synchronization": {"didSave": true},
                        "publishDiagnostics": {},
                        "completion": {"completionItem": {"snippetSupport": false}},
                        "hover": {"contentFormat": ["plaintext", "markdown"]},
                        "definition": {"linkSupport": true}
                    },
                    "workspace": {"workspaceFolders": false}
                }
            }
        });
        let mut server = Server {
            child,
            stdin,
            state: State::Starting,
            sync: Sync::Full,
            next_id: 2,
            pending: HashMap::new(),
            queued: Vec::new(),
        };
        server.pending.insert(1, Pending::Initialize);
        if write_message(&mut server.stdin, &init).is_err() {
            server.state = State::Dead;
        }
        self.servers.insert(language.to_owned(), server);
    }

    /// Envoie une requête (avec réponse attendue) au serveur de `language`.
    fn request(&mut self, language: &str, method: &str, params: Value, pending: Pending) {
        let Some(server) = self.servers.get_mut(language) else {
            return;
        };
        let id = server.next_id;
        server.next_id += 1;
        server.pending.insert(id, pending);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send(language, msg);
    }

    fn notify_server(&mut self, language: &str, method: &str, params: Value) {
        self.send(
            language,
            json!({"jsonrpc": "2.0", "method": method, "params": params}),
        );
    }

    fn doc(&self, path: &Path) -> Option<(&str, &str)> {
        self.docs
            .get(&path_key(path))
            .map(|d| (d.language.as_str(), d.uri.as_str()))
    }

    fn handle_request(&mut self, req: Request) -> bool {
        match req {
            Request::Open {
                path,
                language,
                root,
                version,
                text,
            } => {
                let root = root.or_else(|| path.parent().map(Path::to_path_buf));
                self.start_server(&language, root.as_deref());
                let uri = path_to_uri(&path);
                self.docs.insert(
                    path_key(&path),
                    DocInfo {
                        language: language.clone(),
                        uri: uri.clone(),
                        path,
                    },
                );
                self.notify_server(
                    &language,
                    "textDocument/didOpen",
                    json!({"textDocument": {
                        "uri": uri, "languageId": language, "version": version,
                        "text": text.to_string()
                    }}),
                );
            }
            Request::Change {
                path,
                version,
                changes,
                text,
            } => {
                let Some((language, uri)) =
                    self.doc(&path).map(|(l, u)| (l.to_owned(), u.to_owned()))
                else {
                    return true;
                };
                let sync = self.servers.get(&language).map_or(Sync::None, |s| s.sync);
                let content_changes: Vec<Value> = match sync {
                    Sync::None => return true,
                    Sync::Incremental => changes
                        .iter()
                        .map(|c| json!({"range": range(c.start, c.end), "text": c.text}))
                        .collect(),
                    Sync::Full => vec![json!({"text": text.to_string()})],
                };
                self.notify_server(
                    &language,
                    "textDocument/didChange",
                    json!({"textDocument": {"uri": uri, "version": version},
                           "contentChanges": content_changes}),
                );
            }
            Request::Save { path } => {
                if let Some((language, uri)) =
                    self.doc(&path).map(|(l, u)| (l.to_owned(), u.to_owned()))
                {
                    self.notify_server(
                        &language,
                        "textDocument/didSave",
                        json!({"textDocument": {"uri": uri}}),
                    );
                }
            }
            Request::Close { path } => {
                if let Some(doc) = self.docs.remove(&path_key(&path)) {
                    self.notify_server(
                        &doc.language,
                        "textDocument/didClose",
                        json!({"textDocument": {"uri": doc.uri}}),
                    );
                }
            }
            Request::Completion {
                id,
                path,
                line,
                col16,
            } => {
                self.query(
                    id,
                    &path,
                    "textDocument/completion",
                    line,
                    col16,
                    Pending::Completion(id),
                );
            }
            Request::Hover {
                id,
                path,
                line,
                col16,
            } => {
                self.query(
                    id,
                    &path,
                    "textDocument/hover",
                    line,
                    col16,
                    Pending::Hover(id),
                );
            }
            Request::Definition {
                id,
                path,
                line,
                col16,
            } => {
                self.query(
                    id,
                    &path,
                    "textDocument/definition",
                    line,
                    col16,
                    Pending::Definition(id),
                );
            }
            Request::Shutdown => return false,
        }
        true
    }

    fn query(
        &mut self,
        id: u64,
        path: &Path,
        method: &str,
        line: usize,
        col16: usize,
        pending: Pending,
    ) {
        let Some((language, uri)) = self.doc(path).map(|(l, u)| (l.to_owned(), u.to_owned()))
        else {
            self.emit(Event::Unavailable {
                id,
                reason: "document non ouvert dans un serveur de langage".into(),
            });
            return;
        };
        let alive = self
            .servers
            .get(&language)
            .is_some_and(|s| !matches!(s.state, State::Dead));
        if !alive {
            self.emit(Event::Unavailable {
                id,
                reason: format!("aucun serveur de langage actif pour « {language} »"),
            });
            return;
        }
        self.request(
            &language,
            method,
            json!({"textDocument": {"uri": uri}, "position": position(line, col16)}),
            pending,
        );
    }

    fn handle_server(&mut self, msg: FromServer) {
        match msg {
            FromServer::Closed { language } => {
                let Some(server) = self.servers.get_mut(&language) else {
                    return;
                };
                server.state = State::Dead;
                let pending: Vec<Pending> = server.pending.drain().map(|(_, p)| p).collect();
                for p in pending {
                    let id = match p {
                        Pending::Completion(id) | Pending::Hover(id) | Pending::Definition(id) => {
                            id
                        }
                        Pending::Initialize => continue,
                    };
                    self.emit(Event::Unavailable {
                        id,
                        reason: "le serveur de langage s'est arrêté".into(),
                    });
                }
                if !self.shutting_down {
                    self.emit(Event::ServerExited { language });
                }
            }
            FromServer::Message { language, value } => self.handle_message(&language, &value),
        }
    }

    fn handle_message(&mut self, language: &str, value: &Value) {
        let method = value.get("method").and_then(Value::as_str);
        let id = value.get("id");
        match (method, id) {
            // Requête du serveur : on répond (sinon certains serveurs attendent indéfiniment).
            (Some(method), Some(id)) => {
                let result = match method {
                    "workspace/configuration" => {
                        let n = value
                            .pointer("/params/items")
                            .and_then(Value::as_array)
                            .map_or(0, Vec::len);
                        Value::Array(vec![Value::Null; n])
                    }
                    _ => Value::Null,
                };
                let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
                if let Some(server) = self.servers.get_mut(language) {
                    if write_message(&mut server.stdin, &reply).is_err() {
                        server.state = State::Dead;
                    }
                }
            }
            (Some("textDocument/publishDiagnostics"), None) => {
                let Some(params) = value.get("params") else {
                    return;
                };
                let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                    return;
                };
                let Some(path) = uri_to_path(uri) else {
                    return;
                };
                // Seulement pour les documents ouverts, sous le chemin tel que l'application le connaît.
                if let Some(doc) = self.docs.get(&path_key(&path)) {
                    let path = doc.path.clone();
                    self.emit(Event::Diagnostics {
                        path,
                        items: parse_diagnostics(params),
                    });
                }
            }
            (Some(_), None) => {} // autres notifications (journaux, progression…)
            (None, Some(id)) => self.handle_response(language, id, value),
            (None, None) => {}
        }
    }

    fn handle_response(&mut self, language: &str, id: &Value, value: &Value) {
        let Some(id) = id.as_i64() else {
            return;
        };
        let Some(pending) = self
            .servers
            .get_mut(language)
            .and_then(|s| s.pending.remove(&id))
        else {
            return;
        };
        let result = value.get("result").unwrap_or(&Value::Null);
        let error = value.get("error");
        match pending {
            Pending::Initialize => {
                if let Some(e) = error {
                    let message = e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("échec de l'initialisation")
                        .to_owned();
                    if let Some(s) = self.servers.get_mut(language) {
                        s.state = State::Dead;
                    }
                    self.emit(Event::ServerError {
                        language: language.to_owned(),
                        message,
                    });
                    return;
                }
                // textDocumentSync : un nombre (0, 1, 2) ou un objet {change: n}
                let sync_kind = result
                    .pointer("/capabilities/textDocumentSync")
                    .and_then(|s| {
                        s.as_u64()
                            .or_else(|| s.get("change").and_then(Value::as_u64))
                    })
                    .unwrap_or(1);
                let queued = {
                    let Some(server) = self.servers.get_mut(language) else {
                        return;
                    };
                    server.sync = match sync_kind {
                        0 => Sync::None,
                        2 => Sync::Incremental,
                        _ => Sync::Full,
                    };
                    server.state = State::Ready;
                    std::mem::take(&mut server.queued)
                };
                self.notify_server(language, "initialized", json!({}));
                for msg in queued {
                    self.send(language, msg);
                }
                self.emit(Event::ServerStarted {
                    language: language.to_owned(),
                });
            }
            Pending::Completion(rid) => self.emit(match error {
                Some(_) => Event::Completion {
                    id: rid,
                    items: Vec::new(),
                },
                None => Event::Completion {
                    id: rid,
                    items: parse_completion(result),
                },
            }),
            Pending::Hover(rid) => self.emit(Event::Hover {
                id: rid,
                text: if error.is_some() {
                    None
                } else {
                    parse_hover(result)
                },
            }),
            Pending::Definition(rid) => self.emit(Event::Definition {
                id: rid,
                locations: if error.is_some() {
                    Vec::new()
                } else {
                    parse_locations(result)
                },
            }),
        }
    }

    /// Arrête proprement les serveurs (`shutdown`, `exit`), puis tue ceux qui traînent.
    fn shutdown(&mut self) {
        self.shutting_down = true;
        for server in self.servers.values_mut() {
            if matches!(server.state, State::Ready) {
                let id = server.next_id;
                server.next_id += 1;
                let _ = write_message(
                    &mut server.stdin,
                    &json!({"jsonrpc": "2.0", "id": id, "method": "shutdown"}),
                );
                let _ = write_message(
                    &mut server.stdin,
                    &json!({"jsonrpc": "2.0", "method": "exit"}),
                );
            }
        }
        let deadline = Instant::now() + Duration::from_millis(1500);
        for server in self.servers.values_mut() {
            while Instant::now() < deadline {
                if matches!(server.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            if matches!(server.child.try_wait(), Ok(None)) {
                let _ = server.child.kill();
            }
            let _ = server.child.wait();
        }
    }
}

/// Fusionne les `Change` consécutifs d'un même document : un seul `didChange` pour plusieurs
/// frappes arrivées pendant que le thread travaillait (les listes de modifications se suivent).
fn coalesce(requests: Vec<Request>) -> Vec<Request> {
    let mut out: Vec<Request> = Vec::with_capacity(requests.len());
    for req in requests {
        if let Request::Change {
            path,
            version,
            changes,
            text,
        } = &req
        {
            if let Some(Request::Change {
                path: last_path,
                version: last_version,
                changes: last_changes,
                text: last_text,
            }) = out.last_mut()
            {
                if path_key(last_path) == path_key(path) {
                    last_changes.extend(changes.iter().cloned());
                    *last_version = *version;
                    *last_text = text.clone();
                    continue;
                }
            }
        }
        out.push(req);
    }
    out
}

/// Démarre le thread du client LSP. `servers` : langage -> commande du serveur. `notify` est
/// appelé depuis le thread du client pour chaque [`Event`].
pub fn spawn(
    servers: HashMap<String, ServerConfig>,
    notify: impl Fn(Event) + Send + 'static,
) -> Sender<Request> {
    let (tx, rx): (Sender<Request>, Receiver<Request>) = unbounded();
    let (from_servers, rx_servers) = unbounded::<FromServer>();
    thread::Builder::new()
        .name("paulwrite-lsp".into())
        .spawn(move || {
            let mut client = Client {
                configs: servers,
                servers: HashMap::new(),
                docs: HashMap::new(),
                from_servers,
                notify: Box::new(notify),
                shutting_down: false,
            };
            'run: loop {
                select! {
                    recv(rx) -> msg => {
                        let Ok(first) = msg else { break 'run };
                        let mut batch = vec![first];
                        batch.extend(rx.try_iter());
                        for req in coalesce(batch) {
                            if !client.handle_request(req) {
                                break 'run;
                            }
                        }
                    }
                    recv(rx_servers) -> msg => {
                        if let Ok(m) = msg {
                            client.handle_server(m);
                        }
                    }
                }
            }
            client.shutdown();
        })
        .expect("thread LSP");
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commande_du_serveur() {
        let c = ServerConfig::parse("rust-analyzer --log-file x.log").unwrap();
        assert_eq!(c.command, "rust-analyzer");
        assert_eq!(c.args, ["--log-file", "x.log"]);
        assert_eq!(ServerConfig::parse("   "), None);
    }

    #[test]
    fn analyse_des_reponses() {
        // complétion : tableau ou {items}, insertText / textEdit / libellé
        let a = json!([{"label": "alpha"}, {"label": "b", "insertText": "beta()", "detail": "fn"},
                       {"label": "c", "textEdit": {"newText": "gamma"}}, {"sans": "label"}]);
        let items = parse_completion(&a);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].insert_text, "alpha");
        assert_eq!(
            (items[1].insert_text.as_str(), items[1].detail.as_str()),
            ("beta()", "fn")
        );
        assert_eq!(items[2].insert_text, "gamma");
        assert_eq!(
            parse_completion(&json!({"items": [{"label": "x"}]})).len(),
            1
        );
        assert!(parse_completion(&Value::Null).is_empty());

        // survol : plusieurs formes
        assert_eq!(parse_hover(&json!({"contents": "texte"})).unwrap(), "texte");
        assert_eq!(
            parse_hover(&json!({"contents": {"kind": "markdown", "value": "**gras**"}})).unwrap(),
            "**gras**"
        );
        assert_eq!(
            parse_hover(&json!({"contents": [{"language": "rust", "value": "fn f()"}, "doc"]}))
                .unwrap(),
            "fn f()\n\ndoc"
        );
        assert_eq!(parse_hover(&json!({"contents": "  "})), None);
        assert_eq!(parse_hover(&Value::Null), None);

        // emplacements : Location, tableau, LocationLink
        let loc = json!({"uri": "file:///C:/p/a.rs", "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 9}}});
        assert_eq!(parse_locations(&loc).len(), 1);
        assert_eq!(parse_locations(&loc)[0].line, 3);
        let link = json!([{"targetUri": "file:///C:/p/b.rs", "targetSelectionRange": {"start": {"line": 7, "character": 1}, "end": {"line": 7, "character": 2}}}]);
        assert_eq!(parse_locations(&link)[0].col16, 1);
        assert!(parse_locations(&Value::Null).is_empty());
    }

    #[test]
    fn analyse_des_diagnostics() {
        let p = json!({"uri": "x", "diagnostics": [
            {"range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 5}}, "severity": 2, "message": "attention"},
            {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "message": "sans gravité"},
            {"message": "sans plage"}
        ]});
        let d = parse_diagnostics(&p);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[1].severity, Severity::Error, "gravité absente : erreur");
        assert!(Severity::Error < Severity::Warning);
    }

    #[test]
    fn fusion_des_changements_d_un_meme_document() {
        let ch = |p: &str, v: i32, t: &str| Request::Change {
            path: PathBuf::from(p),
            version: v,
            changes: vec![TextChange {
                start: (0, 0),
                end: (0, 0),
                text: t.into(),
            }],
            text: Rope::from_str(t),
        };
        let merged = coalesce(vec![
            ch("C:/a.rs", 2, "x"),
            ch("c:\\A.rs", 3, "y"),
            ch("C:/b.rs", 1, "z"),
            ch("C:/a.rs", 4, "w"),
        ]);
        assert_eq!(merged.len(), 3);
        match &merged[0] {
            Request::Change {
                version,
                changes,
                text,
                ..
            } => {
                assert_eq!(*version, 3);
                assert_eq!(changes.len(), 2);
                assert_eq!(text.to_string(), "y");
            }
            other => panic!("{other:?}"),
        }
    }
}
