//! Application winit : fenêtre, onglets, événements clavier/souris/IME/DPI, workers (fichiers et
//! coloration syntaxique), et modes de mesure (`--bench-*`) de la latence et du démarrage.
//!
//! Le thread UI ne fait ni E/S ni calcul lourd : fichiers et boîtes de dialogue passent par le
//! worker ([`crate::worker`]), l'analyse syntaxique par son thread dédié. Le rendu est à la
//! demande : une image n'est dessinée que si un événement l'a demandée (aucune boucle
//! permanente ; seul le clignotement du curseur réveille la boucle deux fois par seconde).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use paulwrite_lsp as lsp;
use paulwrite_plugins as plugins;
use paulwrite_render::{
    platform, since_process_start, Colors, Overlay, OverlayRow, OverlayRowKind, SoftRenderer,
    TabInfo,
};
use paulwrite_search::{self as search, FuzzyFinder, IndexOptions, SearchOptions, TextQuery};
use paulwrite_syntax::{self as syntax, Language};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Cursor, CursorIcon, Window, WindowId};

use crate::command::{Command, Effect, LspAction};
use crate::config::Config;
use crate::editor::{Preedit, Squiggle};
use crate::keymap;
use crate::metrics::Samples;
use crate::palette::{parse_goto, Item, Outcome, Palette, PaletteKey, PaletteMode, Target};
use crate::tab::{Tab, SYNTAX_MARGIN_LINES};
use crate::worker::{self, ConfirmFor, Done, Job, TabId};

const BLINK: Duration = Duration::from_millis(530);

/// Éditeur de l'onglet actif (macro plutôt que méthode : emprunt disjoint des autres champs).
macro_rules! ed {
    ($s:expr) => {
        $s.tabs[$s.active].editor
    };
}

/// Événements envoyés au thread UI par d'autres threads.
#[derive(Debug)]
pub enum UserEvent {
    /// Résultat du worker de fichiers.
    Done(Done),
    /// Plages colorées calculées par le thread de coloration.
    Syntax(syntax::Response),
    /// Frappe simulée (modes `--bench-typing` et `--bench-syntax`), horodatée à l'émission.
    BenchKey(Instant),
    /// Fin de la série de frappes simulées.
    BenchDone,
    /// Le fuzzy finder a de nouveaux résultats (envoyé depuis un thread de nucleo).
    PaletteWake,
    /// Le parcours du projet pour Goto Anything est terminé.
    IndexDone,
    /// Résultats de la recherche dans le projet (génération `generation`).
    Search {
        generation: u64,
        event: search::Event,
    },
    /// Frappe simulée dans le panneau (`None` : efface la saisie).
    BenchPalette { at: Instant, ch: Option<char> },
    /// Réponse du thread de plugins Lua.
    Plugin(plugins::Response),
    /// Événement d'un serveur de langage (diagnostics, résultat de complétion…).
    Lsp(lsp::Event),
}

/// Mode de mesure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchKind {
    /// Quitte après la première image : mesure le démarrage à froid.
    Startup,
    /// `n` frappes simulées, latence événement → image copiée vers la fenêtre.
    Typing(usize),
    /// `n` images de défilement enchaînées (3 lignes par image).
    Scroll(usize),
    /// Ouvre le fichier donné et quitte dès qu'il est affiché.
    Open,
    /// `n` frappes dans un fichier Rust de 1 Mo avec coloration : latence frappe → couleurs.
    Syntax(usize),
    /// `n` frappes dans Goto Anything sur le dossier donné : latence frappe → résultats affichés.
    Palette(usize),
    /// Recherche dans le projet (dossier donné) du texte passé en argument.
    PaletteSearch,
    /// `n` appels de la commande de plugin passée en argument (dossier : `PAULWRITE_PLUGINS_DIR`).
    Plugin(usize),
    /// `n` frappes dans un fichier Rust avec un serveur de langage actif (commande donnée par
    /// `PAULWRITE_LSP_SERVER`) : latence de frappe et aller-retour d'une requête.
    Lsp(usize),
}

/// Options de la ligne de commande.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Fichiers à ouvrir (un onglet chacun).
    pub paths: Vec<PathBuf>,
    pub bench: Option<BenchKind>,
    /// Fichier où ajouter le rapport de mesure (en plus de la sortie standard).
    pub bench_out: Option<PathBuf>,
    /// Argument d'un mode de mesure (texte recherché par `--bench-palette-search`).
    pub bench_arg: Option<String>,
}

impl Options {
    /// Analyse les arguments (sans le nom du programme).
    ///
    /// # Errors
    /// Option inconnue ou valeur invalide.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut o = Options::default();
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let mut count = |name: &str| -> Result<usize, String> {
                it.next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| format!("{name} attend un nombre"))
            };
            match a.as_str() {
                "--bench-startup" => o.bench = Some(BenchKind::Startup),
                "--bench-open" => o.bench = Some(BenchKind::Open),
                "--bench-typing" => o.bench = Some(BenchKind::Typing(count("--bench-typing")?)),
                "--bench-syntax" => o.bench = Some(BenchKind::Syntax(count("--bench-syntax")?)),
                "--bench-scroll" => o.bench = Some(BenchKind::Scroll(count("--bench-scroll")?)),
                "--bench-palette" => o.bench = Some(BenchKind::Palette(count("--bench-palette")?)),
                "--bench-lsp" => o.bench = Some(BenchKind::Lsp(count("--bench-lsp")?)),
                "--bench-plugin" => {
                    o.bench = Some(BenchKind::Plugin(count("--bench-plugin")?));
                    o.bench_arg = Some(
                        it.next()
                            .ok_or("--bench-plugin attend un nom de commande")?,
                    );
                }
                "--bench-palette-search" => {
                    o.bench = Some(BenchKind::PaletteSearch);
                    o.bench_arg = Some(it.next().ok_or("--bench-palette-search attend un texte")?);
                }
                "--bench-out" => {
                    o.bench_out = Some(PathBuf::from(
                        it.next().ok_or("--bench-out attend un chemin")?,
                    ));
                }
                s if s.starts_with("--") => return Err(format!("option inconnue : {s}")),
                _ => o.paths.push(PathBuf::from(a)),
            }
        }
        Ok(o)
    }
}

/// Texte synthétique pour les mesures (déterministe, `lines` lignes de pseudo-code).
fn synthetic_text(lines: usize) -> String {
    let words = [
        "let", "fn", "value", "buffer", "cursor", "select", "compute", "render", "line", "struct",
        "impl", "return", "match", "undo", "redo", "paul", "write", "{", "}", "(", ")", ";", "=",
        "+", "//", "self", "mut", "pub",
    ];
    let mut s = 0x2545_F491_4F6C_DD1D_u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut out = String::new();
    for _ in 0..lines {
        let start = out.len();
        out.push_str(&" ".repeat((next() % 4) as usize * 4));
        let target = 30 + (next() % 80) as usize;
        while out.len() - start < target {
            out.push_str(words[(next() % words.len() as u64) as usize]);
            out.push(' ');
        }
        out.push('\n');
    }
    out
}

/// Code Rust synthétique valide (environ 50 octets par ligne) pour mesurer la coloration.
fn synthetic_rust(lines: usize) -> String {
    let mut out = String::from("use std::collections::HashMap;\n\n");
    // chaque fonction fait 10 lignes
    for i in 0..lines.div_ceil(10) {
        out.push_str(&format!(
            "/// Fonction numéro {i}.\nfn compute_{i}(a: i32, b: &str) -> Option<usize> {{\n    let mut map: HashMap<String, usize> = HashMap::new();\n    // calcul {i}\n    if a > {n} {{\n        map.insert(format!(\"clé-{{}}\", b), {n});\n    }}\n    map.get(b).copied()\n}}\n\n",
            n = i * 7 % 1000
        ));
    }
    out
}

/// Ce qu'il reste à faire une fois l'enregistrement d'un onglet terminé.
#[derive(Debug)]
enum AfterSave {
    /// Fermer cet onglet.
    CloseTab(TabId),
    /// Reprendre la fermeture de l'application (onglet suivant à confirmer).
    ContinueQuit,
}

/// Panneau flottant ouvert : sa saisie et ses résultats, plus ce qui les alimente.
struct PaletteState {
    palette: Palette,
    /// Dossier du projet dans lequel on cherche.
    root: PathBuf,
    /// Goto Anything et palette de commandes : classement des candidats par nucleo.
    finder: Option<FuzzyFinder>,
    /// Palette de commandes : (commande, intitulé, raccourci) dans l'ordre d'injection.
    commands: Vec<(Target, String, String)>,
    /// Palette de commandes : texte classé -> indice dans `commands`.
    candidate_index: HashMap<String, usize>,
    /// Annule le parcours ou la recherche en cours quand le panneau se ferme ou que la saisie change.
    cancel: Arc<AtomicBool>,
    /// La saisie vient de changer : le prochain jeu de résultats remet la sélection en tête.
    reset: bool,
    /// Parcours du projet en cours (Goto Anything).
    indexing: bool,
    /// Génération de la recherche en cours : les messages d'une recherche abandonnée sont ignorés.
    generation: u64,
    search_matches: usize,
}

/// Ligne de la liste pour une correspondance de la recherche dans le projet.
fn match_item(m: search::Match) -> Item {
    let path = m.path.replace('\\', "/");
    let prefix = format!("{path}:{}  ", m.line);
    let trimmed_start = m.text.len() - m.text.trim_start().len();
    let text = m.text.trim();
    let base = prefix.chars().count() as u32;
    let mut highlights = Vec::new();
    for &(a, b) in &m.ranges {
        if a < trimmed_start || b > trimmed_start + text.len() {
            continue;
        }
        let before = m.text[trimmed_start..a].chars().count() as u32;
        let len = m.text[a..b].chars().count() as u32;
        highlights.extend(base + before..base + before + len);
    }
    Item {
        label: format!("{prefix}{text}"),
        detail: String::new(),
        highlights,
        target: Target::Match { path, line: m.line },
    }
}

/// Demande interactive faite à un serveur de langage et pas encore revenue.
struct LspPending {
    id: u64,
    /// Complétion : nombre de caractères d'identifiant avant le curseur au moment de la demande.
    prefix: usize,
}

struct App {
    opts: Options,
    config: Config,
    jobs: Sender<Job>,
    syntax_tx: Sender<syntax::Request>,
    proxy: EventLoopProxy<UserEvent>,
    t0: Instant,
    window: Option<Arc<Window>>,
    hwnd: isize,
    renderer: Option<SoftRenderer>,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: TabId,
    /// La liste des onglets (libellés, modifié, actif) doit être renvoyée au moteur de rendu.
    tabs_dirty: bool,
    mods: ModifiersState,
    /// La prochaine présentation doit copier toute l'image (fenêtre affichée, redimensionnée…).
    full_present: bool,
    // souris
    mouse: PhysicalPosition<f64>,
    dragging: bool,
    wheel_lines: f32,
    // clignotement
    next_blink: Instant,
    // titre, enregistrement, fermeture
    title: String,
    after_save: Option<AfterSave>,
    quit_queue: Vec<TabId>,
    /// Une boîte « Enregistrer ? » est ouverte (sur le worker) : on n'en ouvre pas une deuxième.
    confirming: bool,
    /// Dossier du projet (dossier ouvert, glissé ou passé en argument).
    project_root: Option<PathBuf>,
    /// Un réveil du fuzzy finder est déjà en route vers la boucle d'événements.
    wake_pending: Arc<AtomicBool>,
    // plugins Lua (thread dédié, démarré après la première image)
    plugins_tx: Option<Sender<plugins::Request>>,
    plugins_started: bool,
    plugin_commands: Vec<plugins::CommandInfo>,
    plugin_next_id: u64,
    plugin_reload_requested: bool,
    /// Message bref affiché en haut de la fenêtre jusqu'à l'échéance.
    toast: Option<(String, Instant)>,
    // mesures des plugins
    plugin_load: Option<(usize, usize, Duration)>,
    plugin_latency: Samples,
    plugin_sent_at: Option<Instant>,
    plugin_bench_left: usize,
    // serveurs de langage (thread dédié, démarré à la demande)
    lsp_tx: Option<Sender<lsp::Request>>,
    lsp_next_id: u64,
    lsp_pending: Option<LspPending>,
    lsp_errors_shown: std::collections::HashSet<String>,
    lsp_latency: Samples,
    lsp_req_at: Option<Instant>,
    bench_keys: usize,

    palette: Option<PaletteState>,
    /// Le contenu du panneau a changé : à renvoyer au moteur de rendu.
    overlay_dirty: bool,
    // mesures du panneau
    palette_query_at: Option<Instant>,
    palette_settled: bool,
    palette_latency: Samples,
    /// Frappe -> classement terminé (avant l'affichage) : sépare nucleo du dessin.
    palette_settle_latency: Samples,
    index_started: Option<Instant>,
    index_time: Option<Duration>,
    search_started: Option<Instant>,
    search_first: Option<Duration>,
    search_done: Option<Duration>,
    search_count: usize,
    // IME
    ime_area: Option<(i32, i32)>,
    // mesures
    input_at: Option<Instant>,
    latency: Samples,
    scroll_frames: Samples,
    color_latency: Samples,
    worker_time: Samples,
    last_edit_at: Option<Instant>,
    t_visible: Duration,
    t_first_present: Option<Duration>,
    file_loaded: bool,
    t_file_shown: Option<Duration>,
    bench_started: bool,
}

impl App {
    fn new(
        opts: Options,
        jobs: Sender<Job>,
        syntax_tx: Sender<syntax::Request>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        Self {
            opts,
            config: Config::default(),
            jobs,
            syntax_tx,
            proxy,
            t0: Instant::now(),
            window: None,
            hwnd: 0,
            renderer: None,
            tabs: vec![Tab::new(0)],
            active: 0,
            next_tab_id: 1,
            tabs_dirty: true,
            mods: ModifiersState::empty(),
            full_present: true,
            mouse: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            wheel_lines: 0.0,
            next_blink: Instant::now() + BLINK,
            title: String::new(),
            after_save: None,
            quit_queue: Vec::new(),
            confirming: false,
            project_root: None,
            wake_pending: Arc::new(AtomicBool::new(false)),
            plugins_tx: None,
            plugins_started: false,
            plugin_commands: Vec::new(),
            plugin_next_id: 0,
            plugin_reload_requested: false,
            toast: None,
            plugin_load: None,
            plugin_latency: Samples::default(),
            plugin_sent_at: None,
            plugin_bench_left: 0,
            lsp_tx: None,
            lsp_next_id: 0,
            lsp_pending: None,
            lsp_errors_shown: std::collections::HashSet::new(),
            lsp_latency: Samples::default(),
            lsp_req_at: None,
            bench_keys: 0,
            palette: None,
            overlay_dirty: false,
            palette_query_at: None,
            palette_settled: false,
            palette_latency: Samples::default(),
            palette_settle_latency: Samples::default(),
            index_started: None,
            index_time: None,
            search_started: None,
            search_first: None,
            search_done: None,
            search_count: 0,
            ime_area: None,
            input_at: None,
            latency: Samples::default(),
            scroll_frames: Samples::default(),
            color_latency: Samples::default(),
            worker_time: Samples::default(),
            last_edit_at: None,
            t_visible: Duration::ZERO,
            t_first_present: None,
            file_loaded: false,
            t_file_shown: None,
            bench_started: false,
        }
    }

    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn is_bench(&self) -> bool {
        self.opts.bench.is_some()
    }

    fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    // --- Onglets -----------------------------------------------------------------------------

    fn rows_now(&self) -> Option<usize> {
        self.renderer
            .as_ref()
            .map(|r| r.metrics().rows_for(r.size().1))
    }

    /// Crée un onglet vide à la fin de la liste et renvoie son indice.
    fn push_tab(&mut self) -> usize {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab::new(id));
        self.tabs.len() - 1
    }

    /// Rend actif l'onglet `index` : tout l'écran est redessiné.
    fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        if let Some(rows) = self.rows_now() {
            ed!(self).set_viewport_rows(rows);
        }
        self.full_present = true;
        self.tabs_dirty = true;
        self.ime_area = None;
        self.request_redraw();
    }

    /// Envoie la liste des onglets au moteur de rendu si elle a changé.
    fn refresh_tabs(&mut self) {
        // L'état « modifié » de l'onglet actif change à la frappe : on le surveille.
        let m = ed!(self).is_modified();
        if self.tabs[self.active].shown_modified != m {
            self.tabs[self.active].shown_modified = m;
            self.tabs_dirty = true;
        }
        if !self.tabs_dirty {
            return;
        }
        self.tabs_dirty = false;
        let labels: Vec<String> = self.tabs.iter().map(Tab::label).collect();
        let infos: Vec<TabInfo<'_>> = self
            .tabs
            .iter()
            .zip(&labels)
            .enumerate()
            .map(|(i, (t, label))| TabInfo {
                label,
                modified: t.shown_modified,
                active: i == self.active,
            })
            .collect();
        if let Some(r) = self.renderer.as_mut() {
            r.set_tabs(&infos);
            let rows = r.metrics().rows_for(r.size().1);
            ed!(self).set_viewport_rows(rows);
        }
    }

    /// Ouvre un fichier : active son onglet s'il est déjà ouvert, sinon l'ouvre dans l'onglet
    /// courant s'il est vierge, sinon dans un nouvel onglet. La lecture se fait sur le worker.
    fn open_path(&mut self, path: PathBuf) {
        self.open_path_at(path, None);
    }

    /// Comme [`open_path`](Self::open_path), puis place le curseur à la ligne `line` (à partir de
    /// 1) : tout de suite si le fichier est déjà ouvert, sinon quand il sera chargé.
    fn open_path_at(&mut self, path: PathBuf, line: Option<usize>) {
        // `absolute` ne touche pas au disque : seulement le dossier courant.
        let path = std::path::absolute(&path).unwrap_or(path);
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.editor.path() == Some(path.as_path()))
        {
            self.activate(i);
            if let Some(l) = line {
                ed!(self).goto(l, 0);
                self.request_redraw();
            }
            return;
        }
        let index = if self.tabs[self.active].is_pristine() {
            self.active
        } else {
            self.push_tab()
        };
        let tab = &mut self.tabs[index];
        tab.editor.set_path(path.clone());
        tab.loading = Some((0, 0));
        tab.pending_goto = line;
        let id = tab.id;
        self.jobs.send(Job::Open { tab: id, path }).ok();
        self.activate(index);
    }

    /// Choisit le langage d'après le chemin et démarre (ou arrête) la coloration de l'onglet.
    fn attach_syntax(&mut self, index: usize, forced: Option<Language>) {
        let config = self.config.clone();
        let tx = self.syntax_tx.clone();
        let tab = &mut self.tabs[index];
        let language = forced.or_else(|| tab.editor.path().and_then(Language::from_path));
        let len = tab.editor.document().buffer().len_bytes();
        let was_on = tab.syntax_on;
        tab.language = language;
        tab.requested = None;
        match language {
            Some(language) if config.highlights(len) => {
                tab.editor.enable_syntax(true);
                tab.syntax_on = true;
                let rope = tab.editor.document().buffer().rope().clone();
                tx.send(syntax::Request::Open {
                    tab: tab.id,
                    language,
                    rope,
                    revision: tab.editor.revision(),
                })
                .ok();
                let range = tab.editor.view_bytes(SYNTAX_MARGIN_LINES);
                tx.send(syntax::Request::Viewport {
                    tab: tab.id,
                    range: range.clone(),
                })
                .ok();
                tab.requested = Some(range);
            }
            _ => {
                tab.editor.enable_syntax(false);
                tab.syntax_on = false;
                if was_on {
                    tx.send(syntax::Request::Close { tab: tab.id }).ok();
                }
            }
        }
    }

    /// Transmet au thread de coloration les modifications de l'onglet actif et, si besoin, la
    /// nouvelle fenêtre visible. Appelé une fois par image : les frappes d'une même image sont
    /// regroupées.
    fn sync_syntax(&mut self) {
        let tab = &mut self.tabs[self.active];
        if !tab.syntax_on {
            return;
        }
        let edits = tab.editor.take_edits();
        if !edits.is_empty() {
            let range = tab.editor.view_bytes(SYNTAX_MARGIN_LINES);
            self.syntax_tx
                .send(syntax::Request::Edit {
                    tab: tab.id,
                    rope: tab.editor.document().buffer().rope().clone(),
                    revision: tab.editor.revision(),
                    edits,
                    viewport: range.clone(),
                })
                .ok();
            tab.requested = Some(range);
        } else if !tab.editor.spans_cover_view() {
            let near = tab.editor.view_bytes(30);
            let covered = tab
                .requested
                .as_ref()
                .is_some_and(|r| r.start <= near.start && near.end <= r.end);
            if !covered {
                let range = tab.editor.view_bytes(SYNTAX_MARGIN_LINES);
                self.syntax_tx
                    .send(syntax::Request::Viewport {
                        tab: tab.id,
                        range: range.clone(),
                    })
                    .ok();
                tab.requested = Some(range);
            }
        }
    }

    /// Ferme l'onglet sans rien demander. Il en reste toujours au moins un.
    fn close_now(&mut self, index: usize) {
        self.plugin_event("close", Some(index));
        self.lsp_close_tab(index);
        let tab = self.tabs.remove(index);
        if tab.syntax_on {
            self.syntax_tx
                .send(syntax::Request::Close { tab: tab.id })
                .ok();
        }
        if self.tabs.is_empty() {
            self.push_tab();
        }
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.activate(self.active);
    }

    /// Ferme l'onglet `index`, après confirmation s'il contient des modifications.
    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs[index].editor.is_modified() && !self.is_bench() {
            if self.confirming {
                return;
            }
            self.activate(index);
            self.confirm(ConfirmFor::CloseTab(self.tabs[index].id), index);
        } else {
            self.close_now(index);
        }
    }

    /// Demande « Enregistrer les modifications ? » sur le worker ; la réponse arrive en
    /// [`Done::SaveConfirmed`].
    fn confirm(&mut self, for_: ConfirmFor, index: usize) {
        self.confirming = true;
        self.jobs
            .send(Job::ConfirmSave {
                owner: self.hwnd,
                name: self.tabs[index].editor.display_name(),
                for_,
            })
            .ok();
    }

    /// Affiche une erreur sur le worker (jamais de boîte modale dans la boucle d'événements).
    fn show_error(&self, text: String) {
        self.jobs
            .send(Job::ShowError {
                owner: self.hwnd,
                text,
            })
            .ok();
    }

    // --- Fichiers ----------------------------------------------------------------------------

    fn start_save(&mut self, index: usize) {
        let tab = &self.tabs[index];
        match tab.editor.path() {
            None => {
                self.jobs
                    .send(Job::PickSave {
                        tab: tab.id,
                        owner: self.hwnd,
                        suggested: tab.editor.display_name(),
                    })
                    .ok();
            }
            Some(path) => {
                let doc = tab.editor.document();
                self.jobs
                    .send(Job::Save {
                        tab: tab.id,
                        path: path.to_path_buf(),
                        rope: doc.buffer().rope().clone(),
                        encoding: doc.encoding(),
                        revision: doc.buffer().revision(),
                    })
                    .ok();
            }
        }
    }

    /// Ferme l'application en demandant, onglet par onglet, d'enregistrer les modifications.
    fn request_quit(&mut self, el: &ActiveEventLoop) {
        if self.is_bench() {
            el.exit();
            return;
        }
        if self.confirming {
            return; // une question est déjà posée
        }
        self.quit_queue = self
            .tabs
            .iter()
            .filter(|t| t.editor.is_modified())
            .map(|t| t.id)
            .rev()
            .collect();
        self.continue_quit(el);
    }

    fn continue_quit(&mut self, el: &ActiveEventLoop) {
        while let Some(id) = self.quit_queue.pop() {
            let Some(index) = self.tab_index(id) else {
                continue;
            };
            if !self.tabs[index].editor.is_modified() {
                continue;
            }
            self.activate(index);
            // La suite (enregistrer, ignorer, annuler) se poursuit à la réception de la réponse.
            self.confirm(ConfirmFor::Quit(id), index);
            return;
        }
        el.exit();
    }

    // --- Serveurs de langage -----------------------------------------------------------------

    /// Serveurs configurés, par identifiant de langage LSP.
    fn lsp_servers(&self) -> HashMap<String, lsp::ServerConfig> {
        Language::ALL
            .iter()
            .filter_map(|l| {
                let cfg = lsp::ServerConfig::parse(&self.config.lsp_server(l.lsp_id())?)?;
                Some((l.lsp_id().to_owned(), cfg))
            })
            .collect()
    }

    /// Démarre le client LSP (un thread ; les serveurs ne démarrent qu'à la première ouverture
    /// d'un document de leur langage) si les serveurs de langage sont activés.
    fn ensure_lsp(&mut self) -> bool {
        if !self.config.lsp {
            return false;
        }
        if self.lsp_tx.is_none() {
            let proxy = self.proxy.clone();
            self.lsp_tx = Some(lsp::spawn(self.lsp_servers(), move |e| {
                let _ = proxy.send_event(UserEvent::Lsp(e));
            }));
        }
        true
    }

    /// Ouvre le document de l'onglet `index` dans le serveur de son langage, s'il y en a un.
    fn lsp_open_tab(&mut self, index: usize) {
        if !self.config.lsp {
            return;
        }
        let tab = &self.tabs[index];
        let (Some(path), Some(language)) = (tab.editor.path().map(PathBuf::from), tab.language)
        else {
            return;
        };
        if self.config.lsp_server(language.lsp_id()).is_none() || !self.ensure_lsp() {
            return;
        }
        let root = self.project_root.clone();
        let tab = &mut self.tabs[index];
        tab.editor.enable_lsp(true);
        tab.lsp_path = Some(path.clone());
        tab.lsp_version = 1;
        let text = tab.editor.document().buffer().rope().clone();
        if let Some(tx) = &self.lsp_tx {
            tx.send(lsp::Request::Open {
                path,
                language: language.lsp_id().to_owned(),
                root,
                version: 1,
                text,
            })
            .ok();
        }
    }

    /// Referme le document de l'onglet `index` dans son serveur de langage.
    fn lsp_close_tab(&mut self, index: usize) {
        let tab = &mut self.tabs[index];
        if let Some(path) = tab.lsp_path.take() {
            tab.editor.enable_lsp(false);
            tab.diagnostics.clear();
            if let Some(tx) = &self.lsp_tx {
                tx.send(lsp::Request::Close { path }).ok();
            }
        }
    }

    /// Prévient le serveur de langage que le document a été enregistré.
    fn lsp_save(&self, index: usize) {
        if let (Some(tx), Some(path)) = (&self.lsp_tx, &self.tabs[index].lsp_path) {
            tx.send(lsp::Request::Save { path: path.clone() }).ok();
        }
    }

    /// Transmet au serveur de langage les modifications de l'onglet actif (une fois par image :
    /// les frappes d'une même image sont regroupées).
    fn sync_lsp(&mut self) {
        let tab = &mut self.tabs[self.active];
        let Some(path) = tab.lsp_path.clone() else {
            return;
        };
        let changes = tab.editor.take_lsp_changes();
        if changes.is_empty() {
            return;
        }
        tab.lsp_version += 1;
        let request = lsp::Request::Change {
            path,
            version: tab.lsp_version,
            changes,
            text: tab.editor.document().buffer().rope().clone(),
        };
        if let Some(tx) = &self.lsp_tx {
            tx.send(request).ok();
        }
    }

    /// Commande de langage : complétion, définition, survol ou liste des diagnostics.
    fn lsp_request(&mut self, action: LspAction) {
        if action == LspAction::Diagnostics {
            self.open_diagnostics();
            return;
        }
        if !self.config.lsp {
            self.toast("Serveurs de langage désactivés : ajoutez « lsp = true » à config.toml");
            return;
        }
        let tab = &self.tabs[self.active];
        let Some(path) = tab.lsp_path.clone() else {
            self.toast("Pas de serveur de langage pour ce document");
            return;
        };
        let (line, col16) = tab.editor.caret_lsp_position();
        let prefix = tab.editor.word_prefix_chars();
        self.lsp_next_id += 1;
        let id = self.lsp_next_id;
        let request = match action {
            LspAction::Completion => lsp::Request::Completion {
                id,
                path,
                line,
                col16,
            },
            LspAction::Definition => lsp::Request::Definition {
                id,
                path,
                line,
                col16,
            },
            LspAction::Hover | LspAction::Diagnostics => lsp::Request::Hover {
                id,
                path,
                line,
                col16,
            },
        };
        self.lsp_pending = Some(LspPending { id, prefix });
        if let Some(tx) = &self.lsp_tx {
            tx.send(request).ok();
        }
    }

    fn take_lsp_pending(&mut self, id: u64) -> Option<LspPending> {
        if self.lsp_pending.as_ref().is_some_and(|p| p.id == id) {
            self.lsp_pending.take()
        } else {
            None
        }
    }

    /// Ouvre un panneau en lecture seule (diagnostics, définitions, survol).
    fn open_list_palette(&mut self, mode: PaletteMode, items: Vec<Item>, note: String) {
        self.close_palette();
        let mut st = PaletteState {
            palette: Palette::new(mode),
            root: self.palette_root(),
            finder: None,
            commands: Vec::new(),
            candidate_index: HashMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            reset: true,
            indexing: false,
            generation: 0,
            search_matches: 0,
        };
        st.palette.set_items(items, true);
        st.palette.set_note(note);
        self.palette = Some(st);
        self.overlay_dirty = true;
        self.request_redraw();
    }

    /// Ouvre la liste de complétion (la saisie filtre les éléments).
    fn open_completion(&mut self, items: &[lsp::CompletionItem], prefix: usize) {
        self.close_palette();
        let finder = FuzzyFinder::new(self.wake_callback());
        let injector = finder.injector();
        let mut st = PaletteState {
            palette: Palette::new(PaletteMode::Completion),
            root: self.palette_root(),
            finder: None,
            commands: Vec::new(),
            candidate_index: HashMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            reset: true,
            indexing: false,
            generation: 0,
            search_matches: 0,
        };
        for (i, c) in items.iter().take(5000).enumerate() {
            // Un séparateur invisible + l'indice rend chaque candidat unique (deux éléments
            // peuvent porter le même libellé).
            let candidate = format!("{}\u{1}{i}", c.label);
            st.candidate_index
                .insert(candidate.clone(), st.commands.len());
            st.commands.push((
                Target::Completion(c.insert_text.clone(), prefix),
                c.label.clone(),
                c.detail.clone(),
            ));
            FuzzyFinder::add(&injector, candidate);
        }
        st.finder = Some(finder);
        self.palette = Some(st);
        let settled = self.refresh_palette_items(true);
        self.note_settled(settled);
        self.overlay_dirty = true;
        self.request_redraw();
    }

    /// Liste des diagnostics du document courant.
    fn open_diagnostics(&mut self) {
        let mut diags = self.tabs[self.active].diagnostics.clone();
        diags.sort_by_key(|d| (d.start, d.severity));
        let items: Vec<Item> = diags
            .iter()
            .map(|d| Item {
                label: format!(
                    "{}:{}  {}",
                    d.start.0 + 1,
                    d.start.1 + 1,
                    d.message.lines().next().unwrap_or("")
                ),
                detail: match d.severity {
                    lsp::Severity::Error => "erreur",
                    lsp::Severity::Warning => "avertissement",
                    lsp::Severity::Information => "info",
                    lsp::Severity::Hint => "suggestion",
                }
                .to_owned(),
                highlights: Vec::new(),
                target: Target::Line(d.start.0 + 1),
            })
            .collect();
        let note = if items.is_empty() {
            "Aucun diagnostic".to_owned()
        } else {
            format!("{} diagnostic(s)", items.len())
        };
        self.open_list_palette(PaletteMode::Diagnostics, items, note);
    }

    /// Événement d'un serveur de langage.
    fn on_lsp_event(&mut self, event: lsp::Event) {
        match event {
            lsp::Event::Diagnostics { path, items } => {
                let key = lsp::path_key(&path);
                let Some(i) = self.tabs.iter().position(|t| {
                    t.lsp_path.as_deref().map(lsp::path_key).as_deref() == Some(&key)
                }) else {
                    return;
                };
                let squiggles = items
                    .iter()
                    .map(|d| Squiggle {
                        start: d.start,
                        end: d.end,
                        severity: match d.severity {
                            lsp::Severity::Error => 0,
                            lsp::Severity::Warning => 1,
                            _ => 2,
                        },
                    })
                    .collect();
                self.tabs[i].editor.set_squiggles(squiggles);
                self.tabs[i].diagnostics = items;
                if i == self.active {
                    self.request_redraw();
                }
            }
            lsp::Event::Completion { id, items } => {
                let Some(p) = self.take_lsp_pending(id) else {
                    return;
                };
                if items.is_empty() {
                    self.toast("Aucune complétion");
                } else {
                    self.open_completion(&items, p.prefix);
                }
            }
            lsp::Event::Hover { id, text } => {
                if matches!(self.opts.bench, Some(BenchKind::Lsp(_))) {
                    if let Some(t) = self.lsp_req_at.take() {
                        self.lsp_latency.push(t.elapsed());
                    }
                    return;
                }
                if self.take_lsp_pending(id).is_none() {
                    return;
                }
                match text {
                    None => self.toast("Rien à afficher sous le curseur"),
                    Some(text) => {
                        let items: Vec<Item> = text
                            .lines()
                            .filter(|l| !l.trim().is_empty())
                            .take(60)
                            .map(|l| Item {
                                label: l.trim_end().to_owned(),
                                detail: String::new(),
                                highlights: Vec::new(),
                                target: Target::Nothing,
                            })
                            .collect();
                        self.open_list_palette(PaletteMode::Hover, items, String::new());
                    }
                }
            }
            lsp::Event::Definition { id, locations } => {
                if self.take_lsp_pending(id).is_none() {
                    return;
                }
                match locations.as_slice() {
                    [] => self.toast("Définition introuvable"),
                    [one] => self.open_path_at(one.path.clone(), Some(one.line + 1)),
                    many => {
                        let root = self.palette_root();
                        let items: Vec<Item> = many
                            .iter()
                            .map(|l| {
                                let shown = l.path.strip_prefix(&root).unwrap_or(&l.path);
                                Item {
                                    label: format!("{}:{}", shown.display(), l.line + 1),
                                    detail: String::new(),
                                    highlights: Vec::new(),
                                    target: Target::Match {
                                        path: l.path.to_string_lossy().into_owned(),
                                        line: l.line as u64 + 1,
                                    },
                                }
                            })
                            .collect();
                        let note = format!("{} définitions", items.len());
                        self.open_list_palette(PaletteMode::Locations, items, note);
                    }
                }
            }
            lsp::Event::Unavailable { id, reason } => {
                if self.take_lsp_pending(id).is_some() {
                    self.toast(reason);
                }
            }
            lsp::Event::ServerStarted { language } => {
                self.toast(format!("Serveur de langage « {language} » prêt"));
                if let Some(BenchKind::Lsp(n)) = self.opts.bench {
                    self.start_bench_typing(n);
                }
            }
            lsp::Event::ServerError { language, message } => {
                // une seule boîte de message par langage (sinon une par document ouvert)
                if self.lsp_errors_shown.insert(language.clone()) {
                    self.show_error(format!(
                        "Serveur de langage « {language} » indisponible :\n{message}\n\nInstallez-le ou changez `lsp_{language}` dans la configuration."
                    ));
                }
            }
            lsp::Event::ServerExited { language } => {
                self.toast(format!("Le serveur de langage « {language} » s'est arrêté"));
            }
        }
    }

    // --- Plugins Lua -------------------------------------------------------------------------

    /// Démarre le thread de plugins (qui lit le dossier et charge les scripts lui-même). Appelé
    /// après la première image : les plugins ne comptent pas dans le temps de démarrage.
    fn start_plugins(&mut self) {
        self.plugins_started = true;
        let dir = std::env::var_os("PAULWRITE_PLUGINS_DIR")
            .map(PathBuf::from)
            .or_else(Config::plugins_dir);
        let Some(dir) = dir else {
            return;
        };
        let proxy = self.proxy.clone();
        let reserved = Command::all().iter().map(|c| c.name().to_owned()).collect();
        self.plugins_tx = Some(plugins::spawn(dir, reserved, move |r| {
            let _ = proxy.send_event(UserEvent::Plugin(r));
        }));
    }

    /// Instantané de l'onglet `index` pour les plugins (copie O(1) du texte).
    fn plugin_context(&self, index: usize) -> plugins::EditorContext {
        let e = &self.tabs[index].editor;
        let (line, column) = e.caret_line_col();
        plugins::EditorContext {
            path: e.path().map(|p| p.display().to_string()),
            rope: e.document().buffer().rope().clone(),
            selection: e.selection_string(1 << 20),
            line,
            column,
        }
    }

    fn invoke_plugin(&mut self, command: String) {
        if self.plugins_tx.is_none() {
            self.toast("Plugins non chargés");
            return;
        }
        self.plugin_next_id += 1;
        let request = plugins::Request::Invoke {
            id: self.plugin_next_id,
            command,
            ctx: self.plugin_context(self.active),
        };
        if let Some(tx) = &self.plugins_tx {
            tx.send(request).ok();
        }
    }

    /// Événement de l'éditeur pour les plugins (`open`, `save`, `close`).
    fn plugin_event(&self, name: &str, index: Option<usize>) {
        if let (Some(tx), Some(i)) = (&self.plugins_tx, index) {
            tx.send(plugins::Request::Event {
                name: name.to_owned(),
                ctx: self.plugin_context(i),
            })
            .ok();
        }
    }

    fn apply_plugin_actions(&mut self, actions: Vec<plugins::Action>, el: &ActiveEventLoop) {
        for action in actions {
            match action {
                plugins::Action::Insert(text) => {
                    let now = self.now_ms();
                    ed!(self).insert_text(&text, now);
                }
                plugins::Action::Message(m) => self.toast(m),
                plugins::Action::Run(name) => match Command::from_name(&name) {
                    Some(cmd) => self.execute(cmd, el),
                    None => self.toast(format!("Commande inconnue : {name}")),
                },
                plugins::Action::GotoLine(n) => ed!(self).goto(n, 0),
            }
        }
        self.request_redraw();
    }

    /// Affiche un message bref (3 s) en haut de la fenêtre.
    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now() + Duration::from_secs(3)));
        self.overlay_dirty = true;
        self.request_redraw();
    }

    /// Mode `--bench-plugin` : prochain appel de la commande, ou fin de la mesure.
    fn bench_plugin_next(&mut self, el: &ActiveEventLoop) {
        if self.plugin_bench_left == 0 {
            self.finish_bench(el);
            return;
        }
        self.plugin_bench_left -= 1;
        self.plugin_sent_at = Some(Instant::now());
        let command = self.opts.bench_arg.clone().unwrap_or_default();
        self.invoke_plugin(command);
    }

    // --- Panneau flottant --------------------------------------------------------------------

    /// Dossier du projet : celui qu'on a ouvert, sinon le dossier du fichier courant, sinon le
    /// dossier de travail.
    fn palette_root(&self) -> PathBuf {
        self.project_root
            .clone()
            .or_else(|| {
                self.tabs[self.active]
                    .editor
                    .path()
                    .and_then(|p| p.parent().map(PathBuf::from))
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Fonction appelée par les threads de nucleo quand de nouveaux résultats sont prêts. nucleo ne
    /// regroupe pas ses notifications (une par candidat ajouté, soit 100 000 pour un gros
    /// projet) : on n'envoie qu'un seul réveil à la fois, le suivant n'est permis qu'une fois le
    /// premier traité (voir `UserEvent::PaletteWake`).
    fn wake_callback(&self) -> impl Fn() + Send + Sync + 'static {
        let proxy = self.proxy.clone();
        let pending = self.wake_pending.clone();
        move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = proxy.send_event(UserEvent::PaletteWake);
            }
        }
    }

    /// Ouvre le panneau dans ce mode (remplace celui qui était ouvert).
    fn open_palette(&mut self, mode: PaletteMode) {
        self.close_palette();
        let root = self.palette_root();
        let mut st = PaletteState {
            palette: Palette::new(mode),
            root: root.clone(),
            finder: None,
            commands: Vec::new(),
            candidate_index: HashMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            reset: true,
            indexing: false,
            generation: 0,
            search_matches: 0,
        };
        match mode {
            PaletteMode::Files => {
                let finder = FuzzyFinder::new(self.wake_callback());
                let injector = finder.injector();
                let cancel = st.cancel.clone();
                let proxy = self.proxy.clone();
                self.index_started = Some(Instant::now());
                st.indexing = true;
                // Le parcours du projet et l'alimentation du classement se font hors du thread UI.
                std::thread::spawn(move || {
                    let (tx, rx) = crossbeam_channel::unbounded();
                    std::thread::scope(|s| {
                        s.spawn(|| {
                            search::list_files(&root, &IndexOptions::default(), &cancel, tx);
                        });
                        for batch in rx.iter() {
                            for path in batch {
                                FuzzyFinder::add(&injector, path);
                            }
                        }
                    });
                    let _ = proxy.send_event(UserEvent::IndexDone);
                });
                st.finder = Some(finder);
            }
            PaletteMode::Commands => {
                let finder = FuzzyFinder::new(self.wake_callback());
                let injector = finder.injector();
                for cmd in Command::all() {
                    let title = cmd.title();
                    let candidate = format!("{title} {}", cmd.name());
                    let shortcut = keymap::shortcut_label(cmd).unwrap_or_default();
                    let detail = if shortcut.is_empty() {
                        cmd.name().to_owned()
                    } else {
                        shortcut
                    };
                    st.candidate_index
                        .insert(candidate.clone(), st.commands.len());
                    st.commands
                        .push((Target::Command(cmd), title.to_owned(), detail));
                    FuzzyFinder::add(&injector, candidate);
                }
                // Commandes déclarées par les plugins Lua.
                for c in &self.plugin_commands {
                    let title = format!("Plugin : {}", c.title);
                    let candidate = format!("{title} {}", c.name);
                    st.candidate_index
                        .insert(candidate.clone(), st.commands.len());
                    st.commands
                        .push((Target::Plugin(c.name.clone()), title, c.name.clone()));
                    FuzzyFinder::add(&injector, candidate);
                }
                st.finder = Some(finder);
            }
            // Ces panneaux sont ouverts par les réponses du serveur de langage, pas par un raccourci.
            PaletteMode::GotoLine
            | PaletteMode::Completion
            | PaletteMode::Diagnostics
            | PaletteMode::Locations
            | PaletteMode::Hover => {}
            PaletteMode::Search => {
                st.palette.set_note(format!(
                    "Dans {}",
                    root.file_name().map_or_else(
                        || root.display().to_string(),
                        |n| n.to_string_lossy().into_owned()
                    )
                ));
            }
        }
        self.palette = Some(st);
        let settled = self.refresh_palette_items(true);
        self.note_settled(settled);
        self.overlay_dirty = true;
        self.request_redraw();
    }

    /// Ferme le panneau et arrête ce qui tournait pour lui.
    fn close_palette(&mut self) {
        if let Some(st) = self.palette.take() {
            st.cancel.store(true, Ordering::Relaxed);
            self.overlay_dirty = true;
            self.request_redraw();
        }
    }

    /// Un jeu de résultats à jour vient d'être calculé pour la dernière saisie.
    fn note_settled(&mut self, settled: bool) {
        if let (true, Some(at), false) = (settled, self.palette_query_at, self.palette_settled) {
            self.palette_settle_latency.push(at.elapsed());
            self.palette_settled = true;
        }
    }

    /// Recalcule la liste affichée. `force_reset` : la sélection repart du premier résultat.
    /// Renvoie `true` quand les résultats correspondent à la saisie courante (classement terminé).
    fn refresh_palette_items(&mut self, force_reset: bool) -> bool {
        let total_lines = self.tabs[self.active].editor.line_count();
        let Some(st) = self.palette.as_mut() else {
            return true;
        };
        let mode = st.palette.mode();
        let settled = match mode {
            PaletteMode::Files | PaletteMode::Commands | PaletteMode::Completion => {
                let Some(finder) = st.finder.as_mut() else {
                    return true;
                };
                let status = finder.tick(0);
                let reset = force_reset || st.reset;
                if status.changed || reset {
                    let top = finder.top(200);
                    // La mise en évidence des caractères trouvés est calculée plus tard, pour les
                    // seules lignes affichées (`refresh_overlay`).
                    let items: Vec<Item> = top
                        .into_iter()
                        .filter_map(|cand| {
                            if mode == PaletteMode::Files {
                                Some(Item {
                                    target: Target::File(cand.clone()),
                                    label: cand,
                                    detail: String::new(),
                                    highlights: Vec::new(),
                                })
                            } else {
                                let &i = st.candidate_index.get(&cand)?;
                                let (target, title, detail) = &st.commands[i];
                                Some(Item {
                                    label: title.clone(),
                                    detail: detail.clone(),
                                    highlights: Vec::new(),
                                    target: target.clone(),
                                })
                            }
                        })
                        .collect();
                    st.palette.set_items_lazy(items, reset);
                    st.reset = false;
                }
                if mode == PaletteMode::Files {
                    let (total, matched) = (finder.item_count(), finder.matched_count());
                    st.palette.set_note(if st.indexing {
                        format!("{total} fichiers indexés…")
                    } else {
                        format!("{matched} sur {total} fichiers")
                    });
                }
                !status.running
            }
            PaletteMode::GotoLine => {
                let items = match parse_goto(st.palette.query()) {
                    Some(n) => {
                        let n = n.min(total_lines);
                        vec![Item {
                            label: format!("Aller à la ligne {n}"),
                            detail: format!("sur {total_lines}"),
                            highlights: Vec::new(),
                            target: Target::Line(n),
                        }]
                    }
                    None => Vec::new(),
                };
                st.palette.set_note(if items.is_empty() {
                    format!("Numéro de ligne, de 1 à {total_lines}")
                } else {
                    String::new()
                });
                st.palette.set_items(items, true);
                true
            }
            PaletteMode::Search
            | PaletteMode::Diagnostics
            | PaletteMode::Locations
            | PaletteMode::Hover => true,
        };
        self.overlay_dirty = true;
        settled
    }

    /// La saisie du panneau a changé : relancer le classement ou la recherche.
    fn palette_query_changed(&mut self) {
        let Some(st) = self.palette.as_mut() else {
            return;
        };
        let query = st.palette.query().to_owned();
        st.reset = true;
        match st.palette.mode() {
            PaletteMode::Files | PaletteMode::Commands | PaletteMode::Completion => {
                if let Some(finder) = st.finder.as_mut() {
                    finder.set_query(&query);
                    // Laisse 1 ms au classement pour publier un résultat tout de suite (cas
                    // courant) ; sinon il arrivera par `PaletteWake`.
                    finder.tick(1);
                }
                let settled = self.refresh_palette_items(true);
                self.note_settled(settled);
            }
            PaletteMode::GotoLine => {
                let settled = self.refresh_palette_items(true);
                self.note_settled(settled);
            }
            PaletteMode::Search => self.start_search(query),
            PaletteMode::Diagnostics | PaletteMode::Locations | PaletteMode::Hover => {}
        }
        self.overlay_dirty = true;
    }

    /// Lance (en annulant la précédente) la recherche de `query` dans le projet, sur des threads.
    fn start_search(&mut self, query: String) {
        let proxy = self.proxy.clone();
        let Some(st) = self.palette.as_mut() else {
            return;
        };
        st.cancel.store(true, Ordering::Relaxed);
        st.cancel = Arc::new(AtomicBool::new(false));
        st.generation += 1;
        st.search_matches = 0;
        st.palette.set_items(Vec::new(), true);
        if query.chars().count() < 2 {
            st.palette
                .set_note("Saisir au moins 2 caractères".to_owned());
            return;
        }
        st.palette.set_note("Recherche…".to_owned());
        let (generation, root, cancel) = (st.generation, st.root.clone(), st.cancel.clone());
        // Minuscules seulement : insensible à la casse ; une majuscule la rend sensible.
        let case_sensitive = query.chars().any(char::is_uppercase);
        std::thread::spawn(move || {
            let (tx, rx) = crossbeam_channel::unbounded();
            std::thread::scope(|s| {
                s.spawn(|| {
                    for event in rx.iter() {
                        let _ = proxy.send_event(UserEvent::Search { generation, event });
                    }
                });
                let _ = search::search_text(
                    &root,
                    &TextQuery {
                        pattern: query,
                        regex: false,
                        case_sensitive,
                    },
                    &SearchOptions {
                        max_results: 2000,
                        ..SearchOptions::default()
                    },
                    &cancel,
                    tx,
                );
            });
        });
    }

    /// Résultats de la recherche dans le projet.
    fn on_search_event(&mut self, generation: u64, event: search::Event) {
        let Some(st) = self.palette.as_mut() else {
            return;
        };
        if st.generation != generation || st.palette.mode() != PaletteMode::Search {
            return; // recherche abandonnée
        }
        match event {
            search::Event::Matches(ms) => {
                let items: Vec<Item> = ms.into_iter().map(match_item).collect();
                st.search_matches += items.len();
                st.palette.append_items(items);
                st.palette
                    .set_note(format!("{} résultats…", st.search_matches));
            }
            search::Event::Done(sum) => {
                st.palette.set_note(format!(
                    "{} résultats dans {} fichiers ({:.0} ms){}",
                    sum.matches,
                    sum.files,
                    sum.elapsed.as_secs_f64() * 1000.0,
                    if sum.truncated {
                        " — liste tronquée"
                    } else {
                        ""
                    }
                ));
                self.search_count = sum.matches;
                if let Some(t) = self.search_started {
                    self.search_done = Some(t.elapsed());
                }
            }
        }
        self.overlay_dirty = true;
    }

    /// Valide le résultat sélectionné : ferme le panneau puis agit.
    fn accept_palette(&mut self, el: &ActiveEventLoop) {
        let Some(st) = self.palette.as_ref() else {
            return;
        };
        let Some(item) = st.palette.selected().cloned() else {
            return;
        };
        let root = st.root.clone();
        self.close_palette();
        match item.target {
            Target::File(p) => self.open_path_at(root.join(p), None),
            Target::Command(c) => self.execute(c, el),
            Target::Line(n) => {
                ed!(self).goto(n, 0);
                self.request_redraw();
            }
            Target::Match { path, line } => {
                self.open_path_at(root.join(path), Some(line as usize));
            }
            Target::Plugin(name) => self.invoke_plugin(name),
            Target::Completion(text, prefix) => {
                let now = self.now_ms();
                ed!(self).complete(prefix, &text, now);
                self.request_redraw();
            }
            Target::Nothing => {}
        }
    }

    /// Transmet une touche au panneau et en tire les conséquences.
    fn palette_apply(&mut self, key: &PaletteKey, el: &ActiveEventLoop) {
        let Some(st) = self.palette.as_mut() else {
            return;
        };
        // Diagnostics, définitions, survol : la liste est fixe, la saisie ne sert à rien.
        if st.palette.mode().is_static()
            && matches!(
                key,
                PaletteKey::Text(_) | PaletteKey::Backspace | PaletteKey::Delete
            )
        {
            return;
        }
        match st.palette.handle(key) {
            Outcome::QueryChanged => self.palette_query_changed(),
            Outcome::Accept => self.accept_palette(el),
            Outcome::Close => self.close_palette(),
            Outcome::Redraw => self.overlay_dirty = true,
            Outcome::None => {}
        }
        self.request_redraw();
    }

    /// Clavier pendant que le panneau est ouvert : tout lui est réservé.
    fn palette_keyboard(&mut self, event: &KeyEvent, el: &ActiveEventLoop) {
        let at = Instant::now();
        let plain = event.key_without_modifiers();
        // Un raccourci de panneau bascule vers ce mode, ou ferme le panneau s'il est déjà ouvert.
        if let Some(cmd) = keymap::command_for(&plain, self.mods) {
            let mode = match cmd {
                Command::GotoFile => Some(PaletteMode::Files),
                Command::CommandPalette => Some(PaletteMode::Commands),
                Command::GotoLine => Some(PaletteMode::GotoLine),
                Command::SearchProject => Some(PaletteMode::Search),
                _ => None,
            };
            if let Some(mode) = mode {
                if self
                    .palette
                    .as_ref()
                    .is_some_and(|s| s.palette.mode() == mode)
                {
                    self.close_palette();
                } else {
                    self.open_palette(mode);
                }
                self.touch(at);
                return;
            }
        }
        let key = match &event.logical_key {
            Key::Named(NamedKey::Backspace) => Some(PaletteKey::Backspace),
            Key::Named(NamedKey::Delete) => Some(PaletteKey::Delete),
            Key::Named(NamedKey::ArrowLeft) => Some(PaletteKey::Left),
            Key::Named(NamedKey::ArrowRight) => Some(PaletteKey::Right),
            Key::Named(NamedKey::ArrowUp) => Some(PaletteKey::Up),
            Key::Named(NamedKey::ArrowDown) => Some(PaletteKey::Down),
            Key::Named(NamedKey::Home) => Some(PaletteKey::Home),
            Key::Named(NamedKey::End) => Some(PaletteKey::End),
            Key::Named(NamedKey::PageUp) => Some(PaletteKey::PageUp),
            Key::Named(NamedKey::PageDown) => Some(PaletteKey::PageDown),
            Key::Named(NamedKey::Enter) => Some(PaletteKey::Enter),
            Key::Named(NamedKey::Escape) => Some(PaletteKey::Escape),
            _ => None,
        };
        let key = key.or_else(|| {
            if self.mods.control_key() && !self.mods.alt_key() {
                // Ctrl+V : colle la première ligne du presse-papiers.
                let is_v = matches!(&plain, Key::Character(s) if s.eq_ignore_ascii_case("v"));
                return is_v
                    .then(|| platform::clipboard_get(self.hwnd))
                    .flatten()
                    .map(|t| PaletteKey::Text(t.lines().next().unwrap_or("").to_owned()));
            }
            event
                .text
                .as_ref()
                .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
                .map(|t| PaletteKey::Text(t.to_string()))
        });
        if let Some(k) = key {
            self.palette_apply(&k, el);
        }
        self.touch(at);
    }

    /// Renvoie le contenu du panneau au moteur de rendu s'il a changé.
    fn refresh_overlay(&mut self) {
        if !self.overlay_dirty {
            return;
        }
        self.overlay_dirty = false;
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        // Caractères trouvés : calculés pour les seules lignes visibles.
        if let Some(PaletteState {
            palette,
            finder: Some(finder),
            ..
        }) = self.palette.as_mut()
        {
            match palette.mode() {
                PaletteMode::Files => {
                    palette.fill_visible_highlights(|item| finder.indices(&item.label));
                }
                PaletteMode::Commands => {
                    palette.fill_visible_highlights(|item| {
                        // Texte classé = « intitulé nom » ; seuls les caractères de l'intitulé sont
                        // mis en évidence.
                        let name = match &item.target {
                            Target::Command(cmd) => cmd.name(),
                            Target::Plugin(name) => name.as_str(),
                            _ => return Vec::new(),
                        };
                        let n = item.label.chars().count() as u32;
                        finder
                            .indices(&format!("{} {name}", item.label))
                            .into_iter()
                            .filter(|&h| h < n)
                            .collect()
                    });
                }
                PaletteMode::Completion => {
                    palette.fill_visible_highlights(|item| finder.indices(&item.label));
                }
                _ => {}
            }
        }
        match &self.palette {
            Some(st) => {
                let overlay = st.palette.to_overlay(r.overlay_columns());
                r.set_overlay(Some(&overlay));
            }
            None => match &self.toast {
                Some((text, _)) => {
                    let cols = r.overlay_columns();
                    let first = text.lines().next().unwrap_or("");
                    let shown: String = if first.chars().count() > cols {
                        first
                            .chars()
                            .take(cols.saturating_sub(1))
                            .chain(std::iter::once('\u{2026}'))
                            .collect()
                    } else {
                        first.to_owned()
                    };
                    r.set_overlay(Some(&Overlay {
                        rows: vec![OverlayRow {
                            kind: OverlayRowKind::Note,
                            text: shown,
                            ..OverlayRow::default()
                        }],
                    }));
                }
                None => r.set_overlay(None),
            },
        }
    }

    /// Mesures du panneau, une fois l'image présentée.
    fn palette_after_present(&mut self, presented: Instant) {
        if self.palette_settled {
            if let Some(t) = self.palette_query_at.take() {
                self.palette_latency.push(presented - t);
            }
            self.palette_settled = false;
        }
        if let (Some(start), None) = (self.search_started, self.search_first) {
            if self
                .palette
                .as_ref()
                .is_some_and(|s| !s.palette.items().is_empty())
            {
                self.search_first = Some(start.elapsed());
            }
        }
    }

    // --- Dessin ------------------------------------------------------------------------------

    /// Dessine dans la DIB (sans copier vers la fenêtre).
    fn draw(&mut self) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        ed!(self).build_view();
        let scrolled = ed!(self).scrolled_since_draw();
        if let Err(e) = r.draw(&ed!(self).view(), scrolled) {
            eprintln!("erreur de rendu : {e}");
        }
        ed!(self).mark_drawn();
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let started = Instant::now();
        self.sync_syntax();
        self.sync_lsp();
        self.refresh_tabs();
        self.refresh_overlay();
        self.draw();
        if let Some(r) = self.renderer.as_mut() {
            let result = if self.full_present {
                self.full_present = false;
                r.present_all(self.hwnd)
            } else {
                r.present(self.hwnd)
            };
            if let Err(e) = result {
                eprintln!("erreur de présentation : {e}");
            }
        }
        let presented = Instant::now();
        if !self.plugins_started && !self.is_bench() {
            self.start_plugins();
        }
        self.palette_after_present(presented);
        if let Some(t) = self.input_at.take() {
            self.latency.push(presented - t);
        }
        if self.t_first_present.is_none() {
            self.t_first_present = Some(since_process_start());
        }
        self.refresh_title();
        self.update_ime_area();
        self.bench_after_present(el, presented - started);
    }

    fn refresh_title(&mut self) {
        let tab = &self.tabs[self.active];
        let title = match tab.loading {
            Some((done, total)) if total > 0 => format!(
                "Chargement… {} % — {}",
                done * 100 / total,
                tab.editor.display_name()
            ),
            Some(_) => format!("Chargement… — {}", tab.editor.display_name()),
            None => tab.editor.title(),
        };
        if title != self.title {
            if let Some(w) = &self.window {
                w.set_title(&title);
            }
            self.title = title;
        }
    }

    fn update_ime_area(&mut self) {
        let (Some(w), Some(r)) = (&self.window, &self.renderer) else {
            return;
        };
        let Some((row, utf16)) = ed!(self).caret_anchor() else {
            return;
        };
        let m = r.metrics();
        // Position approchée (chasse fixe) : suffisante pour placer la fenêtre de l'IME.
        let x = (m.pad_x + utf16 as f32 * m.advance) as i32;
        let y = m.top + (row as i32 + 1) * m.line_height;
        if self.ime_area != Some((x, y)) {
            self.ime_area = Some((x, y));
            w.set_ime_cursor_area(
                PhysicalPosition::new(x, y),
                PhysicalSize::new(1, m.line_height.max(1) as u32),
            );
        }
    }

    fn sync_viewport(&mut self) {
        if let (Some(w), Some(r)) = (&self.window, self.renderer.as_mut()) {
            let size = w.inner_size();
            if let Err(e) = r.resize((size.width, size.height)) {
                eprintln!("redimensionnement : {e}");
            }
            let rows = r.metrics().rows_for(r.size().1);
            ed!(self).set_viewport_rows(rows);
        }
        self.full_present = true;
    }

    // --- Entrées -----------------------------------------------------------------------------

    fn touch(&mut self, at: Instant) {
        self.input_at.get_or_insert(at);
        self.next_blink = Instant::now() + BLINK;
        self.request_redraw();
    }

    fn run_effect(&mut self, effect: Effect, el: &ActiveEventLoop) {
        match effect {
            Effect::SetClipboard(text) => {
                if !platform::clipboard_set(self.hwnd, &text) {
                    eprintln!("presse-papiers indisponible");
                }
            }
            Effect::RequestPaste => {
                if let Some(text) = platform::clipboard_get(self.hwnd) {
                    let now = self.now_ms();
                    ed!(self).insert_text(&text, now);
                }
            }
            Effect::Save => self.start_save(self.active),
            Effect::SaveAs => {
                let tab = &self.tabs[self.active];
                self.jobs
                    .send(Job::PickSave {
                        tab: tab.id,
                        owner: self.hwnd,
                        suggested: tab.editor.display_name(),
                    })
                    .ok();
            }
            Effect::Open => {
                self.jobs.send(Job::PickOpen { owner: self.hwnd }).ok();
            }
            Effect::NewTab => {
                let i = self.push_tab();
                self.activate(i);
            }
            Effect::CloseTab => self.close_tab(self.active),
            Effect::NextTab => self.activate((self.active + 1) % self.tabs.len()),
            Effect::PrevTab => {
                self.activate((self.active + self.tabs.len() - 1) % self.tabs.len());
            }
            Effect::OpenPalette(mode) => self.open_palette(mode),
            Effect::Lsp(action) => self.lsp_request(action),
            Effect::ReloadPlugins => {
                if let Some(tx) = &self.plugins_tx {
                    self.plugin_reload_requested = true;
                    tx.send(plugins::Request::Reload).ok();
                } else {
                    self.toast("Plugins non chargés");
                }
            }
            Effect::Quit => self.request_quit(el),
        }
    }

    fn execute(&mut self, cmd: Command, el: &ActiveEventLoop) {
        let now = self.now_ms();
        if let Some(effect) = ed!(self).execute(cmd, now) {
            self.run_effect(effect, el);
        }
    }

    /// Place le curseur sous la souris (clic ou glissement).
    fn mouse_to_caret(&mut self, extend: bool) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        let m = r.metrics();
        ed!(self).build_view();
        let y = (self.mouse.y as i32 - m.top).max(0);
        let row = (y / m.line_height.max(1)) as usize;
        let row = row.min(ed!(self).rows().saturating_sub(1));
        let utf16 = r
            .column_at(ed!(self).row_text(row), self.mouse.x as f32)
            .unwrap_or(0);
        let col = ed!(self).char_col_of_utf16(row, utf16);
        ed!(self).click(row, col, extend);
    }

    // --- Mesures -----------------------------------------------------------------------------

    fn start_bench_typing(&mut self, n: usize) {
        self.bench_started = true;
        // Caret au milieu du texte, dans une ligne visible.
        ed!(self).scroll_by(100);
        ed!(self).click(10, 0, false);
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            for _ in 0..n {
                std::thread::sleep(Duration::from_millis(20));
                if proxy
                    .send_event(UserEvent::BenchKey(Instant::now()))
                    .is_err()
                {
                    return;
                }
            }
            let _ = proxy.send_event(UserEvent::BenchDone);
        });
    }

    /// Frappes simulées dans le panneau : `n` caractères, 25 ms d'écart, la saisie est effacée
    /// entre deux requêtes.
    fn start_bench_palette(&mut self, n: usize) {
        self.bench_started = true;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            const QUERIES: [&str; 3] = [
                "mod_017/sub_05/file_0023.py",
                "sub_12 file_00 py",
                "mod_099 sub_19 file_0049 md",
            ];
            let mut typed = 0;
            'all: loop {
                for q in QUERIES {
                    for c in q.chars() {
                        if typed >= n {
                            break 'all;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                        let at = Instant::now();
                        if proxy
                            .send_event(UserEvent::BenchPalette { at, ch: Some(c) })
                            .is_err()
                        {
                            return;
                        }
                        typed += 1;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                    let _ = proxy.send_event(UserEvent::BenchPalette {
                        at: Instant::now(),
                        ch: None,
                    });
                    // laisse le temps d'effacer avant la requête suivante (sinon les premiers
                    // caractères attendent derrière l'effacement et faussent la mesure)
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            std::thread::sleep(Duration::from_millis(150));
            let _ = proxy.send_event(UserEvent::BenchDone);
        });
    }

    fn bench_after_present(&mut self, el: &ActiveEventLoop, frame: Duration) {
        let Some(kind) = self.opts.bench else {
            return;
        };
        match kind {
            BenchKind::Startup => self.finish_bench(el),
            BenchKind::Open => {
                if self.file_loaded {
                    self.t_file_shown = Some(since_process_start());
                    self.finish_bench(el);
                }
            }
            BenchKind::Typing(n) => {
                if !self.bench_started {
                    self.start_bench_typing(n);
                    self.request_redraw();
                }
            }
            // La frappe démarre à la première réponse de coloration (voir `user_event`).
            BenchKind::Syntax(_) => {}
            // La frappe démarre quand le serveur de langage est prêt (voir `on_lsp_event`).
            BenchKind::Lsp(_) => {}
            BenchKind::Plugin(n) => {
                if !self.plugins_started {
                    self.plugin_bench_left = n;
                    self.start_plugins();
                }
            }
            // Le panneau s'ouvre à la première image ; les frappes commencent quand le projet est
            // indexé (voir `UserEvent::IndexDone`).
            BenchKind::Palette(_) | BenchKind::PaletteSearch => {
                if !self.bench_started || kind == BenchKind::PaletteSearch {
                    if self.palette.is_none() && self.search_started.is_none() {
                        if kind == BenchKind::PaletteSearch {
                            self.bench_started = true;
                            self.open_palette(PaletteMode::Search);
                            self.search_started = Some(Instant::now());
                            let q = self.opts.bench_arg.clone().unwrap_or_default();
                            self.palette_apply(&PaletteKey::Text(q), el);
                        } else {
                            self.open_palette(PaletteMode::Files);
                            self.bench_started = true;
                        }
                        self.request_redraw();
                    } else if self.search_done.is_some() && self.search_first.is_some() {
                        self.finish_bench(el);
                    }
                }
            }
            BenchKind::Scroll(n) => {
                if !self.bench_started {
                    self.bench_started = true;
                } else {
                    self.scroll_frames.push(frame);
                }
                if self.scroll_frames.len() >= n {
                    self.finish_bench(el);
                } else {
                    let max = ed!(self).document().buffer().len_lines();
                    let rows = ed!(self).rows();
                    if ed!(self).first_line() + 3 + rows >= max {
                        let back = ed!(self).first_line() as isize;
                        ed!(self).scroll_by(-back);
                    } else {
                        ed!(self).scroll_by(3);
                    }
                    self.request_redraw();
                }
            }
        }
    }

    fn finish_bench(&mut self, el: &ActiveEventLoop) {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let mut report = String::new();
        let kind = self.opts.bench.map_or("?".to_owned(), |k| format!("{k:?}"));
        report.push_str(&format!(
            "paulwrite bench {kind} | fenêtre visible {:.1} ms | 1re image {:.1} ms (depuis la création du processus)\n",
            ms(self.t_visible),
            ms(self.t_first_present.unwrap_or_default())
        ));
        if let Some(t) = self.t_file_shown {
            report.push_str(&format!(
                "  fichier affiché : {:.0} ms depuis la création du processus\n",
                ms(t)
            ));
        }
        if let Some(s) = self.latency.summary() {
            report.push_str(&format!(
                "  frappe, événement -> image copiée vers la fenêtre : {s}\n"
            ));
        }
        if let Some(s) = self.color_latency.summary() {
            report.push_str(&format!(
                "  frappe -> couleurs à jour reçues par le thread UI (coloration) : {s}\n"
            ));
        }
        if let Some(s) = self.worker_time.summary() {
            report.push_str(&format!(
                "  thread de coloration, analyse + requête par modification : {s}\n"
            ));
        }
        if let Some(s) = self.scroll_frames.summary() {
            report.push_str(&format!("  défilement, dessin + copie par image : {s}\n"));
        }
        if let Some(s) = self.lsp_latency.summary() {
            report.push_str(&format!(
                "  serveur de langage, aller-retour d'une requête de survol (serveur local) : {s}\n"
            ));
        }
        if let Some((loaded, commands, time)) = self.plugin_load {
            report.push_str(&format!(
                "  plugins : {loaded} chargé(s), {commands} commande(s), chargement {:.1} ms (thread dédié)\n",
                ms(time)
            ));
        }
        if let Some(s) = self.plugin_latency.summary() {
            report.push_str(&format!(
                "  plugins, commande -> actions appliquées (aller-retour complet) : {s}\n"
            ));
            report.push_str(&format!(
                "  texte final du document : {} octets\n",
                ed!(self).document().buffer().len_bytes()
            ));
        }
        if let Some(t) = self.index_time {
            report.push_str(&format!(
                "  Goto Anything, parcours + indexation du projet : {:.0} ms\n",
                ms(t)
            ));
        }
        if let Some(s) = self.palette_settle_latency.summary() {
            report.push_str(&format!(
                "  Goto Anything, frappe -> classement terminé (avant dessin) : {s}\n"
            ));
        }
        if let Some(s) = self.palette_latency.summary() {
            report.push_str(&format!(
                "  Goto Anything, frappe -> résultats à jour affichés : {s}\n"
            ));
        }
        if let (Some(first), Some(done)) = (self.search_first, self.search_done) {
            report.push_str(&format!(
                "  recherche dans le projet : premiers résultats affichés {:.0} ms, terminée {:.0} ms ({} résultats)\n",
                ms(first),
                ms(done),
                self.search_count
            ));
        }
        print!("{report}");
        if let Some(path) = &self.opts.bench_out {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = f.write_all(report.as_bytes());
            }
        }
        el.exit();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let bench_offscreen = matches!(
            self.opts.bench,
            Some(
                BenchKind::Typing(_)
                    | BenchKind::Scroll(_)
                    | BenchKind::Open
                    | BenchKind::Syntax(_)
                    | BenchKind::Palette(_)
                    | BenchKind::PaletteSearch
                    | BenchKind::Plugin(_)
                    | BenchKind::Lsp(_)
            )
        );
        // `PAULWRITE_OFFSCREEN=1` : fenêtre hors de l'écran et sans focus, pour les tests
        // automatiques qui envoient des messages Windows à la fenêtre (aucune gêne pour l'utilisateur).
        let offscreen = bench_offscreen || std::env::var_os("PAULWRITE_OFFSCREEN").is_some();
        let mut attrs = Window::default_attributes()
            .with_title(ed!(self).title())
            .with_inner_size(LogicalSize::new(1100.0, 720.0))
            .with_visible(false);
        if self.is_bench() || offscreen {
            attrs = attrs.with_active(false);
        }
        if offscreen {
            // Mesure de débit : fenêtre hors de la zone visible, pour ne pas gêner l'utilisateur.
            attrs = attrs.with_position(PhysicalPosition::new(-6000, 0));
        }
        let window = Arc::new(el.create_window(attrs).expect("création de la fenêtre"));
        self.hwnd = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        let size = window.inner_size();
        let renderer = SoftRenderer::new(
            window.scale_factor() as f32,
            (size.width, size.height),
            Colors::default(),
        )
        .expect("initialisation du rendu");
        self.renderer = Some(renderer);
        self.window = Some(window.clone());
        // Barre d'onglets : fixe la zone des rangées avant la première image.
        self.refresh_tabs();

        match self.opts.bench {
            Some(BenchKind::Typing(_) | BenchKind::Scroll(_)) => {
                ed!(self).load(
                    paulwrite_core::Document::from_text(&synthetic_text(5000)),
                    None,
                );
                if let Some(rows) = self.rows_now() {
                    ed!(self).set_viewport_rows(rows);
                }
            }
            Some(BenchKind::Syntax(_)) => {
                ed!(self).load(
                    paulwrite_core::Document::from_text(&synthetic_rust(20_000)),
                    None,
                );
                if let Some(rows) = self.rows_now() {
                    ed!(self).set_viewport_rows(rows);
                }
                // Pas de fichier : on force le langage et on suit les modifications.
                self.config = Config {
                    highlight_max_bytes: usize::MAX,
                    highlight: true,
                    ..Config::default()
                };
                self.attach_syntax(self.active, Some(Language::Rust));
            }
            Some(BenchKind::Lsp(_)) => {
                ed!(self).load(
                    paulwrite_core::Document::from_text(&synthetic_rust(5000)),
                    None,
                );
                if let Some(rows) = self.rows_now() {
                    ed!(self).set_viewport_rows(rows);
                }
                let server = std::env::var("PAULWRITE_LSP_SERVER").unwrap_or_default();
                self.config = Config {
                    highlight_max_bytes: usize::MAX,
                    lsp: true,
                    lsp_servers: HashMap::from([("rust".to_owned(), server)]),
                    ..Config::default()
                };
                // Un chemin fictif suffit : le client n'ouvre pas le fichier, il reçoit son texte.
                ed!(self).set_path(std::env::temp_dir().join("paulwrite-bench-lsp.rs"));
                self.attach_syntax(self.active, Some(Language::Rust));
                self.lsp_open_tab(self.active);
            }
            Some(BenchKind::Palette(_) | BenchKind::PaletteSearch) => {
                // Le premier argument est le dossier du projet à parcourir, pas un fichier.
                if !self.opts.paths.is_empty() {
                    self.project_root = Some(self.opts.paths.remove(0));
                }
            }
            _ => {}
        }

        window.set_ime_allowed(true);
        window.set_cursor(Cursor::Icon(CursorIcon::Text));
        for path in std::mem::take(&mut self.opts.paths) {
            self.open_path(path);
        }

        // Première image dessinée avant l'affichage : la fenêtre apparaît avec son contenu.
        self.sync_syntax();
        self.refresh_tabs();
        self.draw();
        window.set_visible(true);
        self.t_visible = since_process_start();
        self.full_present = true;
        window.request_redraw();
        el.set_control_flow(ControlFlow::Wait);
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::BenchKey(at) => {
                let now = self.now_ms();
                ed!(self).insert_text("x", now);
                self.bench_keys += 1;
                if matches!(self.opts.bench, Some(BenchKind::Lsp(_))) && self.bench_keys % 10 == 0 {
                    // Une requête de survol toutes les 10 frappes : aller-retour avec le serveur.
                    if let Some(path) = self.tabs[self.active].lsp_path.clone() {
                        self.lsp_next_id += 1;
                        let (line, col16) = ed!(self).caret_lsp_position();
                        self.lsp_req_at = Some(Instant::now());
                        if let Some(tx) = &self.lsp_tx {
                            tx.send(lsp::Request::Hover {
                                id: self.lsp_next_id,
                                path,
                                line,
                                col16,
                            })
                            .ok();
                        }
                    }
                }
                self.input_at.get_or_insert(at);
                self.last_edit_at = Some(at);
                self.request_redraw();
            }
            UserEvent::BenchDone => self.finish_bench(el),
            UserEvent::Lsp(event) => self.on_lsp_event(event),
            UserEvent::Plugin(resp) => match resp {
                plugins::Response::Loaded(report) => {
                    self.plugin_commands = report.commands.clone();
                    self.plugin_load =
                        Some((report.loaded, report.commands.len(), report.load_time));
                    if !report.errors.is_empty() {
                        let shown: Vec<&str> =
                            report.errors.iter().take(5).map(String::as_str).collect();
                        self.show_error(format!(
                            "Erreurs de plugin ({} au total) :\n{}",
                            report.errors.len(),
                            shown.join("\n")
                        ));
                    } else if self.plugin_reload_requested {
                        self.toast(format!(
                            "{} plugin(s) rechargé(s), {} commande(s)",
                            report.loaded,
                            report.commands.len()
                        ));
                    }
                    self.plugin_reload_requested = false;
                    if matches!(self.opts.bench, Some(BenchKind::Plugin(_))) {
                        self.bench_plugin_next(el);
                    }
                }
                plugins::Response::Done { id, actions, error } => {
                    self.apply_plugin_actions(actions, el);
                    if let Some(e) = error {
                        self.toast(format!("Plugin : {e}"));
                    }
                    if id != 0 && matches!(self.opts.bench, Some(BenchKind::Plugin(_))) {
                        if let Some(t) = self.plugin_sent_at.take() {
                            self.plugin_latency.push(t.elapsed());
                        }
                        self.bench_plugin_next(el);
                    }
                }
            },
            UserEvent::PaletteWake => {
                // Autorise le prochain réveil *avant* de lire les résultats : un résultat publié
                // pendant la lecture déclenchera un nouveau réveil, rien n'est perdu.
                self.wake_pending.store(false, Ordering::Release);
                let settled = self.refresh_palette_items(false);
                self.note_settled(settled);
                self.request_redraw();
            }
            UserEvent::IndexDone => {
                if let Some(st) = self.palette.as_mut() {
                    st.indexing = false;
                }
                if let Some(t) = self.index_started.take() {
                    self.index_time = Some(t.elapsed());
                }
                let settled = self.refresh_palette_items(false);
                self.note_settled(settled);
                if let Some(BenchKind::Palette(n)) = self.opts.bench {
                    // Le projet est indexé : on peut commencer à taper (une seule fois).
                    if self.palette.is_some() && self.palette_latency.is_empty() {
                        self.start_bench_palette(n);
                    }
                }
                self.request_redraw();
            }
            UserEvent::Search { generation, event } => {
                self.on_search_event(generation, event);
                self.request_redraw();
            }
            UserEvent::BenchPalette { at, ch } => {
                match ch {
                    Some(c) => {
                        self.palette_query_at = Some(at);
                        self.palette_settled = false;
                        self.palette_apply(&PaletteKey::Text(c.to_string()), el);
                    }
                    None => {
                        // Effacement entre deux requêtes : pas de mesure pour ces retours arrière.
                        self.palette_query_at = None;
                        self.palette_settled = false;
                        if self
                            .palette
                            .as_mut()
                            .is_some_and(|s| s.palette.clear() == Outcome::QueryChanged)
                        {
                            self.palette_query_changed();
                        }
                    }
                }
                self.request_redraw();
            }
            UserEvent::Syntax(resp) => {
                let Some(i) = self.tab_index(resp.tab) else {
                    return;
                };
                // Une réponse pour un texte déjà dépassé est ignorée : une plus récente suit.
                if resp.revision != self.tabs[i].editor.revision() {
                    return;
                }
                self.worker_time.push(resp.elapsed);
                self.tabs[i].editor.set_spans(resp.range, resp.spans);
                if let (Some(t), true) = (self.last_edit_at.take(), i == self.active) {
                    self.color_latency.push(t.elapsed());
                }
                if let Some(BenchKind::Syntax(n)) = self.opts.bench {
                    if !self.bench_started {
                        // Première coloration reçue : le document est prêt, on commence à taper.
                        self.worker_time = Samples::default();
                        self.start_bench_typing(n);
                    }
                }
                if i == self.active {
                    self.request_redraw();
                }
            }
            UserEvent::Done(done) => match done {
                Done::Config(c) => {
                    // Les modes de mesure ont leur propre configuration (voir `resumed`).
                    if !self.is_bench() {
                        self.config = c;
                    }
                }
                Done::Directory { tab, path } => {
                    // Un dossier devient la racine du projet ; son onglet n'a pas de raison d'être.
                    self.project_root = Some(path);
                    if let Some(i) = self.tab_index(tab) {
                        self.tabs[i].loading = None;
                        if self.tabs.len() > 1 {
                            self.close_now(i);
                        } else {
                            self.tabs[i]
                                .editor
                                .load(paulwrite_core::Document::new(), None);
                        }
                    }
                    self.tabs_dirty = true;
                    self.request_redraw();
                }
                Done::SaveConfirmed { for_, choice } => {
                    self.confirming = false;
                    match (for_, choice) {
                        (ConfirmFor::CloseTab(id), platform::SaveChoice::Save) => {
                            if let Some(i) = self.tab_index(id) {
                                self.after_save = Some(AfterSave::CloseTab(id));
                                self.start_save(i);
                            }
                        }
                        (ConfirmFor::CloseTab(id), platform::SaveChoice::Discard) => {
                            if let Some(i) = self.tab_index(id) {
                                self.close_now(i);
                            }
                        }
                        (ConfirmFor::Quit(id), platform::SaveChoice::Save) => {
                            if let Some(i) = self.tab_index(id) {
                                self.after_save = Some(AfterSave::ContinueQuit);
                                self.start_save(i);
                            } else {
                                self.continue_quit(el);
                            }
                        }
                        (ConfirmFor::Quit(_), platform::SaveChoice::Discard) => {
                            self.continue_quit(el);
                        }
                        (_, platform::SaveChoice::Cancel) => self.quit_queue.clear(),
                    }
                }
                Done::Progress { tab, done, total } => {
                    if let Some(i) = self.tab_index(tab) {
                        self.tabs[i].loading = Some((done, total));
                        if i == self.active {
                            self.refresh_title();
                        }
                    }
                }
                Done::Loaded { tab, path, result } => {
                    let Some(i) = self.tab_index(tab) else {
                        return; // onglet fermé pendant la lecture
                    };
                    self.tabs[i].loading = None;
                    match result {
                        Ok(doc) => {
                            self.tabs[i].editor.load(doc, Some(path));
                            self.file_loaded = true;
                            self.tabs[i].shown_modified = false;
                            if i == self.active {
                                if let Some(rows) = self.rows_now() {
                                    ed!(self).set_viewport_rows(rows);
                                }
                                if let Some(r) = self.renderer.as_mut() {
                                    r.invalidate();
                                }
                                self.full_present = true;
                            }
                            if let Some(l) = self.tabs[i].pending_goto.take() {
                                self.tabs[i].editor.goto(l, 0);
                            }
                            self.attach_syntax(i, None);
                            self.plugin_event("open", Some(i));
                            self.lsp_open_tab(i);
                        }
                        Err(e) => {
                            self.show_error(format!(
                                "Impossible d'ouvrir « {} » :\n{e}",
                                path.display()
                            ));
                            // L'onglet créé pour ce fichier n'a plus de raison d'être.
                            if self.tabs.len() > 1 {
                                self.close_now(i);
                            } else {
                                self.tabs[i]
                                    .editor
                                    .load(paulwrite_core::Document::new(), None);
                            }
                        }
                    }
                    self.tabs_dirty = true;
                    self.request_redraw();
                }
                Done::Saved {
                    tab,
                    path,
                    revision,
                    result,
                } => {
                    let index = self.tab_index(tab);
                    match result {
                        Ok(()) => {
                            if let Some(i) = index {
                                if self.tabs[i].editor.path() == Some(path.as_path()) {
                                    self.tabs[i].editor.mark_saved(revision);
                                }
                                self.tabs_dirty = true;
                                self.plugin_event("save", Some(i));
                                self.lsp_save(i);
                            }
                            match self.after_save.take() {
                                Some(AfterSave::CloseTab(id)) if id == tab => {
                                    if let Some(i) = index {
                                        self.close_now(i);
                                    }
                                }
                                Some(AfterSave::ContinueQuit) => self.continue_quit(el),
                                other => self.after_save = other,
                            }
                            self.request_redraw();
                        }
                        Err(e) => {
                            self.after_save = None;
                            self.quit_queue.clear();
                            self.show_error(format!(
                                "Échec de l'enregistrement de « {} » :\n{e}",
                                path.display()
                            ));
                        }
                    }
                }
                Done::OpenPicked(Some(path)) => self.open_path(path),
                Done::OpenPicked(None) => {}
                Done::SavePicked {
                    tab,
                    path: Some(path),
                } => {
                    if let Some(i) = self.tab_index(tab) {
                        self.lsp_close_tab(i);
                        self.tabs[i].editor.set_path(path);
                        // Le nouveau nom peut changer le langage (« .txt » devenu « .rs »).
                        self.attach_syntax(i, None);
                        self.lsp_open_tab(i);
                        self.start_save(i);
                        self.tabs_dirty = true;
                        self.request_redraw();
                    }
                }
                Done::SavePicked { path: None, .. } => {
                    self.after_save = None;
                    self.quit_queue.clear();
                }
            },
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.request_quit(el),
            WindowEvent::RedrawRequested => self.redraw(el),
            WindowEvent::Resized(_) => {
                self.sync_viewport();
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Déplacement vers un autre écran / changement de DPI : nouvelle police.
                if let Some(r) = self.renderer.as_mut() {
                    if let Err(e) = r.set_scale(scale_factor as f32) {
                        eprintln!("changement d'échelle : {e}");
                    }
                }
                self.ime_area = None;
                self.tabs_dirty = true;
                self.sync_viewport();
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if self.palette.is_some() {
                    self.palette_keyboard(&event, el);
                    return;
                }
                let at = Instant::now();
                let plain = event.key_without_modifiers();
                if let Some(cmd) = keymap::command_for(&plain, self.mods) {
                    self.execute(cmd, el);
                } else if !(self.mods.control_key() && !self.mods.alt_key()) {
                    if let Some(text) = &event.text {
                        if !text.is_empty() && !text.chars().any(char::is_control) {
                            let now = self.now_ms();
                            ed!(self).insert_text(text, now);
                        }
                    }
                }
                self.touch(at);
            }
            WindowEvent::Ime(ime) => {
                let at = Instant::now();
                if self.palette.is_some() {
                    if let Ime::Commit(text) = ime {
                        self.palette_apply(&PaletteKey::Text(text), el);
                        self.touch(at);
                    }
                    return;
                }
                match ime {
                    Ime::Commit(text) => {
                        ed!(self).set_preedit(None);
                        let now = self.now_ms();
                        ed!(self).insert_text(&text, now);
                    }
                    Ime::Preedit(text, cursor) => {
                        ed!(self).set_preedit(Some(Preedit {
                            text,
                            cursor: cursor.map(|(start, _)| start),
                        }));
                    }
                    Ime::Enabled | Ime::Disabled => ed!(self).set_preedit(None),
                }
                self.touch(at);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(st) = self.palette.as_mut() {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -y * 3.0,
                        MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / 20.0,
                    };
                    st.palette.scroll(lines.round() as isize);
                    self.overlay_dirty = true;
                    self.request_redraw();
                    return;
                }
                let lh = self
                    .renderer
                    .as_ref()
                    .map_or(20.0, |r| r.metrics().line_height as f32);
                self.wheel_lines += match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 3.0,
                    MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / lh,
                };
                let whole = self.wheel_lines.trunc();
                if whole != 0.0 {
                    self.wheel_lines -= whole;
                    ed!(self).scroll_by(whole as isize);
                    self.touch(Instant::now());
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = position;
                if self.dragging {
                    self.mouse_to_caret(true);
                    self.touch(Instant::now());
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                let (x, y) = (self.mouse.x as f32, self.mouse.y as f32);
                if self.palette.is_some() {
                    // Un clic sur un résultat le valide ; hors du panneau, il se ferme.
                    if button == MouseButton::Left {
                        match self.renderer.as_ref().and_then(|r| r.overlay_row_at(x, y)) {
                            Some(0) => {}
                            Some(i) => {
                                let picked = self
                                    .palette
                                    .as_mut()
                                    .is_some_and(|st| st.palette.select_visible(i - 1));
                                if picked {
                                    self.accept_palette(el);
                                } else {
                                    self.overlay_dirty = true;
                                }
                            }
                            None => self.close_palette(),
                        }
                        self.request_redraw();
                    }
                    return;
                }
                let hit = self.renderer.as_ref().and_then(|r| r.tab_at(x, y));
                let in_bar = self
                    .renderer
                    .as_ref()
                    .is_some_and(|r| (self.mouse.y as i32) < r.metrics().top);
                match (button, hit) {
                    (MouseButton::Left, Some((i, true))) | (MouseButton::Middle, Some((i, _))) => {
                        self.close_tab(i);
                    }
                    (MouseButton::Left, Some((i, false))) => self.activate(i),
                    (MouseButton::Left, None) if !in_bar => {
                        self.dragging = true;
                        self.mouse_to_caret(self.mods.shift_key());
                        self.touch(Instant::now());
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.dragging = false,
            WindowEvent::DroppedFile(path) => self.open_path(path),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.is_bench() {
            el.set_control_flow(ControlFlow::Wait);
            return;
        }
        let now = Instant::now();
        if self.toast.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.toast = None;
            self.overlay_dirty = true;
            self.request_redraw();
        }
        if now >= self.next_blink {
            let on = !ed!(self).caret_visible();
            ed!(self).set_caret_visible(on);
            self.next_blink = now + BLINK;
            self.request_redraw();
        }
        let wake = self
            .toast
            .as_ref()
            .map_or(self.next_blink, |(_, until)| (*until).min(self.next_blink));
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

/// Lance l'application.
///
/// # Errors
/// Échec de création de la boucle d'événements.
pub fn run(opts: Options) -> Result<(), String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let worker_proxy = proxy.clone();
    let jobs = worker::spawn(move |done| {
        let _ = worker_proxy.send_event(UserEvent::Done(done));
    });
    let syntax_proxy = proxy.clone();
    let syntax_tx = syntax::spawn(move |resp| {
        let _ = syntax_proxy.send_event(UserEvent::Syntax(resp));
    });
    let mut app = App::new(opts, jobs, syntax_tx, proxy);
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn analyse_des_arguments() {
        let o = parse(&["notes.txt", "autre.rs"]).unwrap();
        assert_eq!(
            o.paths,
            vec![PathBuf::from("notes.txt"), PathBuf::from("autre.rs")]
        );
        assert_eq!(o.bench, None);
        let o = parse(&["--bench-typing", "300", "--bench-out", "r.txt"]).unwrap();
        assert_eq!(o.bench, Some(BenchKind::Typing(300)));
        assert_eq!(o.bench_out, Some(PathBuf::from("r.txt")));
        assert_eq!(
            parse(&["--bench-startup"]).unwrap().bench,
            Some(BenchKind::Startup)
        );
        assert_eq!(
            parse(&["--bench-syntax", "100"]).unwrap().bench,
            Some(BenchKind::Syntax(100))
        );
        assert!(parse(&["--bench-typing"]).is_err());
        assert!(parse(&["--bench-typing", "x"]).is_err());
        assert!(parse(&["--inconnue"]).is_err());
    }

    #[test]
    fn les_textes_synthetiques_sont_deterministes() {
        let a = synthetic_text(50);
        assert_eq!(a, synthetic_text(50));
        assert_eq!(a.lines().count(), 50);
        let r = synthetic_rust(200);
        assert_eq!(r, synthetic_rust(200));
        assert!(r.lines().count() >= 200);
        // le Rust synthétique est analysable sans erreur
        let rope = ropey::Rope::from_str(&r);
        let mut h = syntax::Highlighter::new(Language::Rust).unwrap();
        h.update(&rope, &[]);
        assert!(!h.highlights(&rope, 0..r.len()).is_empty());
    }
}
