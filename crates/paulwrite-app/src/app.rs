//! Application winit : fenêtre, onglets, événements clavier/souris/IME/DPI, workers (fichiers et
//! coloration syntaxique), et modes de mesure (`--bench-*`) de la latence et du démarrage.
//!
//! Le thread UI ne fait ni E/S ni calcul lourd : fichiers et boîtes de dialogue passent par le
//! worker ([`crate::worker`]), l'analyse syntaxique par son thread dédié. Le rendu est à la
//! demande : une image n'est dessinée que si un événement l'a demandée (aucune boucle
//! permanente ; seul le clignotement du curseur réveille la boucle deux fois par seconde).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use paulwrite_lsp as lsp;
use paulwrite_plugins as plugins;
use paulwrite_render::watch::{self, DirWatcher};
use paulwrite_render::{
    platform, since_process_start, Chrome, Colors, GitMark, SoftRenderer, StripHit, TabButton,
    TabInfo,
};
use paulwrite_search::{
    self as search, FindOptions, FuzzyFinder, IndexOptions, SearchOptions, TextQuery,
};
use paulwrite_syntax::{self as syntax, Language};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::platform::windows::{
    EventLoopBuilderExtWindows, IconExtWindows, WindowAttributesExtWindows,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Cursor, CursorIcon, Icon, Window, WindowId};

use crate::command::{Command, Effect, LspAction};
use crate::config::Config;
use crate::editing;
use crate::editor::{Preedit, Squiggle};
use crate::git::{self, GitStatus};
use crate::i18n::Lang;
use crate::keymap;
use crate::menu;
use crate::metrics::Samples;
use crate::palette::{parse_goto, Item, Outcome, Palette, PaletteKey, PaletteMode, Target};
use crate::project_replace;
use crate::session::{self, Session, TabState};
use crate::sidebar::{Click, SidebarTree};
use crate::tab::{Tab, SYNTAX_MARGIN_LINES};
use crate::worker::{self, ConfirmFor, Done, Job, Stamp, TabId};

const BLINK: Duration = Duration::from_millis(530);
/// Délai minimal entre deux reconstructions de la minimap pendant la frappe : elle se met à jour
/// au plus tous les 150 ms (et à la fin de la frappe), jamais à chaque touche.
const MINIMAP_DELAY: Duration = Duration::from_millis(150);
/// Silence après la dernière modification du disque avant de relire les dossiers affichés.
const DIR_REFRESH_DELAY: Duration = Duration::from_millis(250);
/// Silence après la dernière frappe avant de redemander la coloration du document entier (minimap).
const OVERVIEW_DELAY: Duration = Duration::from_millis(400);

/// Éditeur de l'onglet actif (macro plutôt que méthode : emprunt disjoint des autres champs).
macro_rules! ed {
    ($s:expr) => {
        $s.tabs[$s.active].editor
    };
}

/// Moteur de rendu du volet qui a le focus (macro : emprunt disjoint des autres champs).
macro_rules! fr_mut {
    ($s:expr) => {
        if $s.focus == 1 {
            $s.second.as_mut()
        } else {
            $s.renderer.as_mut()
        }
    };
}

mod find;
mod links;
mod panes;
mod project;
mod repo;

use find::FindState;
pub use find::FoundEvent;

/// Événements envoyés au thread UI par d'autres threads.
#[derive(Debug)]
pub enum UserEvent {
    /// Résultat du worker de fichiers.
    Done(Done),
    /// Un élément a été créé, supprimé ou renommé dans ce dossier du projet (surveillance du
    /// disque).
    DirChanged(PathBuf),
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
    /// Étape d'une capture d'écran (`--screenshot`).
    Screenshot(u8),
    /// Entrée choisie dans la barre de menus (identifiant : voir [`menu::build`]).
    Menu(u32),
    /// Résultat d'une recherche dans le document courant (Ctrl+F).
    Found(FoundEvent),
    /// État Git d'un dossier (thread `git`).
    Git(git::GitResult),
    /// Lignes changées d'un fichier par rapport à `HEAD` (thread `diff`).
    Diff(git::DiffResult),
}

/// Pont entre le crochet de messages de la boucle d'événements (créé avant elle) et l'application.
static MENU_PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

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
    /// Écrit une capture PNG de la fenêtre (hors écran) puis quitte : contrôle visuel du rendu.
    /// `PAULWRITE_SCREENSHOT_PALETTE=files|commands` ouvre le panneau avant la capture.
    pub screenshot: Option<PathBuf>,
    /// Désactive le retour à la ligne (comparaisons de mesures).
    pub no_wrap: bool,
    /// Fenêtre ouverte par « Nouvelle fenêtre » : vide, sans reprise ni enregistrement de session.
    pub new_window: bool,
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
                "--screenshot" => {
                    o.screenshot = Some(PathBuf::from(
                        it.next().ok_or("--screenshot attend un chemin")?,
                    ));
                }
                "--no-wrap" => o.no_wrap = true,
                "--new-window" => o.new_window = true,
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
    /// Recherche et remplacement : le champ « remplacer par » et le champ qui a la saisie.
    replace: Option<Palette>,
    focus_replace: bool,
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

/// Ce que la souris est en train de glisser dans la barre latérale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarDrag {
    /// Le bord droit : la largeur de la colonne.
    Resize,
    /// Le curseur de sa barre de défilement.
    Scroll,
}

/// Ce que survole la souris, tel que transmis au moteur de rendu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Hover {
    /// Volet sous la souris.
    pane: usize,
    /// La gouttière.
    gutter: bool,
    /// Rangée dont la flèche de repli est sous le pointeur.
    fold_arrow: Option<usize>,
    /// Onglet (indice, croix de fermeture ?).
    tab: Option<(usize, bool)>,
    /// Ligne de la barre latérale.
    sidebar_row: Option<usize>,
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
    /// Écran partagé : moteur du volet de droite (voir `panes.rs`).
    second: Option<SoftRenderer>,
    tabs: Vec<Tab>,
    /// Onglet du volet qui a le focus : toute la saisie agit sur lui.
    active: usize,
    /// Volet qui a le focus (0 ou 1), et onglet affiché dans chaque volet.
    focus: usize,
    shown: [TabId; 2],
    next_tab_id: TabId,
    /// Dernier numéro de groupe de vues jumelles distribué (voir `links.rs`).
    next_link: u64,
    /// La liste des onglets (libellés, modifié, actif) doit être renvoyée au moteur de rendu.
    tabs_dirty: bool,
    mods: ModifiersState,
    /// La prochaine présentation doit copier toute l'image (fenêtre affichée, redimensionnée…).
    full_present: bool,
    // souris
    mouse: PhysicalPosition<f64>,
    dragging: bool,
    wheel_lines: f32,
    /// Retour à la ligne automatique voulu (commun à tous les onglets ; désactivé de fait pour
    /// un fichier trop gros, voir `Config::can_wrap`).
    word_wrap: bool,
    /// Langue de l'interface (réglage `language`, sinon celle de Windows).
    lang: Lang,
    /// La souris survole la gouttière : les flèches de repli sont affichées.
    fold_hover: bool,
    /// Glissement en cours dans la barre latérale (largeur ou défilement).
    sidebar_drag: Option<SidebarDrag>,
    /// Surveillance du dossier du projet (changements faits hors de l'éditeur).
    watcher: Option<DirWatcher>,
    /// Dossiers dont le contenu a changé sur le disque, à relire quand la rafale est finie.
    dirty_dirs: HashSet<PathBuf>,
    refresh_due: Option<Instant>,
    /// Dernier survol transmis au moteur de rendu : gouttière, flèche, onglet, barre latérale.
    hover: Hover,
    // clignotement
    next_blink: Instant,
    // titre, enregistrement, fermeture
    title: String,
    after_save: Option<AfterSave>,
    quit_queue: Vec<TabId>,
    /// Une question « recharger le fichier modifié sur le disque ? » est posée.
    reload_asked: bool,
    /// Diagnostic des frappes (`PAULWRITE_KEYLOG`).
    keylog: bool,
    /// Demandes d'état Git (thread dédié), dernier état reçu, dossier demandé et texte de la barre
    /// d'état.
    git_tx: Sender<PathBuf>,
    /// Demandes des lignes changées d'un fichier (marqueurs de la gouttière), thread dédié.
    diff_tx: Sender<PathBuf>,
    git: Option<Arc<GitStatus>>,
    git_dir: Option<PathBuf>,
    git_label: String,
    /// Remplacement dans le projet en attente de confirmation (puis d'application).
    project_params: Option<project_replace::Params>,
    /// Taille du texte (pixels) et largeur d'une tabulation (colonnes) : réglages `font_size` et
    /// `tab_size`, Ctrl+molette ajuste la taille.
    font_px: u32,
    tab_size: usize,
    /// Onglets fermés (le plus récent en dernier), pour « Rouvrir l'onglet fermé ».
    closed: Vec<TabState>,
    /// Fichiers ouverts récemment, le plus récent d'abord.
    recent: Vec<PathBuf>,
    /// Cette fenêtre reprend la session précédente et l'enregistre en se fermant (pas pour une
    /// nouvelle fenêtre, ni quand des fichiers sont donnés sur la ligne de commande).
    session_enabled: bool,
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
    /// Texte de la barre d'état, reconstruit à chaque image (tampon réutilisé).
    status_buf: String,
    /// Mesures : durée de chaque étape d'une image (préparation, interface, dessin, copie).
    stages: [Samples; 6],
    /// Dernière minimap fournie au rendu : clé (onglet, révision, première ligne) et instant.
    minimap_key: [Option<(TabId, u64, usize, u64)>; 2],
    /// Échéance de la prochaine demande de coloration du document entier (après une frappe).
    overview_due: Option<Instant>,
    minimap_at: Instant,
    /// La minimap est en retard sur le texte : une image est à redemander à l'échéance.
    minimap_pending: bool,
    /// Glissement en cours dans la zone de droite : `true` barre de défilement, `false` minimap.
    strip_drag: Option<bool>,
    /// Forme du pointeur actuellement affichée.
    cursor_icon: CursorIcon,
    /// Arborescence du dossier ouvert ; la barre latérale est affichée si `sidebar_open`.
    sidebar: Option<SidebarTree>,
    sidebar_open: bool,
    /// Commandes de la barre de menus, par identifiant (voir [`menu::build`]).
    menu_commands: Vec<Command>,
    /// Ctrl+K a été pressé : la touche suivante complète un raccourci à deux temps.
    chord_k: bool,

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
    /// Recherche dans le document courant (voir `find.rs`).
    find: Option<FindState>,
    find_opts: FindOptions,
    find_generation: u64,
    /// Où était le curseur à l'ouverture du panneau : la saisie sélectionne à partir de là.
    find_origin: usize,
    /// Dernier texte cherché (préremplit le panneau, sert à F3).
    last_find: String,
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
        git_tx: Sender<PathBuf>,
        diff_tx: Sender<PathBuf>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let word_wrap = !opts.no_wrap;
        // `PAULWRITE_SCREENSHOT_SESSION=1` : la capture reprend et réenregistre la session.
        let session_enabled = opts.bench.is_none()
            && (opts.screenshot.is_none()
                || std::env::var_os("PAULWRITE_SCREENSHOT_SESSION").is_some())
            && !opts.new_window
            && opts.paths.is_empty();
        Self {
            session_enabled,
            opts,
            config: Config::default(),
            jobs,
            syntax_tx,
            proxy,
            t0: Instant::now(),
            window: None,
            hwnd: 0,
            renderer: None,
            second: None,
            tabs: vec![Tab::new(0)],
            active: 0,
            focus: 0,
            shown: [0, 0],
            next_tab_id: 1,
            next_link: 0,
            tabs_dirty: true,
            mods: ModifiersState::empty(),
            full_present: true,
            mouse: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            wheel_lines: 0.0,
            word_wrap,
            lang: Lang::system(),
            fold_hover: false,
            hover: Hover::default(),
            sidebar_drag: None,
            watcher: None,
            dirty_dirs: HashSet::new(),
            refresh_due: None,
            next_blink: Instant::now() + BLINK,
            title: String::new(),
            after_save: None,
            quit_queue: Vec::new(),
            reload_asked: false,
            keylog: std::env::var_os("PAULWRITE_KEYLOG").is_some(),
            git_tx,
            diff_tx,
            git: None,
            git_dir: None,
            git_label: String::new(),
            project_params: None,
            font_px: crate::config::DEFAULT_FONT_SIZE,
            tab_size: editing::DEFAULT_TAB_WIDTH,
            closed: Vec::new(),
            recent: Vec::new(),
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
            status_buf: String::new(),
            stages: Default::default(),
            minimap_key: [None, None],
            overview_due: None,
            minimap_at: Instant::now(),
            minimap_pending: false,
            strip_drag: None,
            cursor_icon: CursorIcon::Text,
            sidebar: None,
            sidebar_open: false,
            menu_commands: Vec::new(),
            chord_k: false,
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
            find: None,
            find_opts: FindOptions::default(),
            find_generation: 0,
            find_origin: 0,
            last_find: String::new(),
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

    /// Rangées visibles dans le volet qui a le focus.
    fn rows_now(&self) -> Option<usize> {
        self.rows_in(self.focus)
    }

    /// Crée un onglet vide à la fin de la liste (dans le volet qui a le focus) et renvoie son
    /// indice.
    fn push_tab(&mut self) -> usize {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut tab = Tab::new(id);
        tab.pane = self.focus;
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    /// Rend actif l'onglet `index` (et donne le focus à son volet) : tout l'écran est redessiné.
    fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.find_panel_open() {
            self.close_palette();
        }
        // l'historique d'annulation d'un fichier à deux vues suit celle qui reçoit la saisie
        self.claim_history(index);
        self.active = index;
        let pane = self.tabs[index]
            .pane
            .min(usize::from(self.second.is_some()));
        self.tabs[index].pane = pane;
        self.focus = pane;
        self.shown[pane] = self.tabs[index].id;
        self.invalidate_panes();
        for p in 0..=usize::from(self.second.is_some()) {
            let i = self.shown_index(p);
            if let Some(rows) = self.rows_in(p) {
                self.tabs[i].editor.set_viewport_rows(rows);
            }
            self.sync_text_layout_for(i, p);
            // seul l'éditeur du volet qui a le focus a un curseur qui clignote
            self.tabs[i].editor.set_caret_visible(p == pane);
        }
        self.full_present = true;
        self.tabs_dirty = true;
        self.ime_area = None;
        // sans dossier ouvert, l'état Git suit le fichier de l'onglet actif ; les marqueurs de la
        // gouttière (par rapport à HEAD) sont ceux de ce fichier
        if self.project_root.is_none() {
            self.request_git();
        }
        self.request_diff(index);
        self.request_redraw();
    }

    /// Largeur du texte et retour à la ligne de l'onglet actif : la largeur dépend de la fenêtre,
    /// de la gouttière, de la barre latérale et de la minimap ; le retour à la ligne, du réglage
    /// et de la taille du fichier.
    fn sync_text_layout_for(&mut self, i: usize, pane: usize) {
        let Some(r) = (if pane == 1 {
            self.second.as_ref()
        } else {
            self.renderer.as_ref()
        }) else {
            return;
        };
        let cols = r.text_cols();
        let len = self.tabs[i].editor.document().buffer().len_bytes();
        let wrap = self.word_wrap && self.config.can_wrap(len);
        let marks = self.fold_hover && self.hover.pane == pane;
        let lang = self.lang;
        let style = editing::comment_style(self.tabs[i].language);
        let e = &mut self.tabs[i].editor;
        e.set_lang(lang);
        e.set_comment_style(style);
        e.set_wrap(wrap);
        e.set_fold_marks(marks);
        e.set_viewport_cols(cols);
    }

    /// Envoie la liste des onglets de chaque volet au moteur de rendu si elle a changé.
    fn refresh_tabs(&mut self) {
        // L'état « modifié » de l'onglet affiché change à la frappe : on le surveille.
        for pane in 0..=usize::from(self.second.is_some()) {
            let i = self.shown_index(pane);
            let m = self.tabs[i].editor.is_modified();
            if self.tabs[i].shown_modified != m {
                self.tabs[i].shown_modified = m;
                self.tabs_dirty = true;
                // les marqueurs Git de la gouttière disparaissent à la première frappe (les numéros
                // de ligne ne correspondent plus) et reviennent à l'enregistrement
                if self.tabs[i].editor.has_changes() {
                    self.refresh_gutter();
                }
            }
        }
        if !self.tabs_dirty {
            return;
        }
        self.tabs_dirty = false;
        let labels: Vec<String> = self.tabs.iter().map(Tab::label).collect();
        for pane in 0..=usize::from(self.second.is_some()) {
            let shown = self.shown_index(pane);
            let infos: Vec<TabInfo<'_>> = self
                .tabs
                .iter()
                .zip(&labels)
                .enumerate()
                .filter(|(_, (t, _))| t.pane == pane)
                .map(|(i, (t, label))| TabInfo {
                    label,
                    modified: t.shown_modified,
                    active: i == shown,
                    git: match (self.git.as_deref(), t.editor.path()) {
                        (Some(g), Some(path)) => g.mark_of(path, false),
                        _ => GitMark::None,
                    },
                })
                .collect();
            let r = if pane == 1 {
                self.second.as_mut()
            } else {
                self.renderer.as_mut()
            };
            if let Some(r) = r {
                r.set_tabs(&infos);
                let rows = r.metrics().rows_for(r.size().1);
                self.tabs[shown].editor.set_viewport_rows(rows);
            }
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
        // un onglet vierge réutilisé ne peut pas rester la jumelle d'un autre
        self.unlink(index);
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
                // la minimap colorée : le worker colore tout le document quand il est au repos
                tx.send(syntax::Request::Overview { tab: tab.id }).ok();
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
        self.sync_syntax_tab(self.active);
        if self.second.is_some() {
            let other = self.shown_index(1 - self.focus);
            self.sync_syntax_tab(other);
        }
    }

    /// Même chose pour l'onglet `i` (celui de chaque volet est suivi).
    fn sync_syntax_tab(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        if !tab.syntax_on {
            return;
        }
        let edits = tab.editor.take_edits();
        if !edits.is_empty() {
            self.overview_due = Some(Instant::now() + OVERVIEW_DELAY);
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
        // On se souvient de l'onglet (seulement un vrai fichier lu ou enregistré).
        if let Some(t) = self.tabs.get(index) {
            // (une vue jumelle ne se « rouvre » pas : le fichier reste ouvert dans l'autre vue)
            if let (Some(path), true, 0) = (t.editor.path(), t.disk_stamp.is_some(), t.link) {
                let (line, col) = t.editor.caret_line_col();
                let closed = TabState {
                    path: path.to_path_buf(),
                    line,
                    col,
                    first: t.editor.first_line(),
                };
                self.closed.push(closed);
                if self.closed.len() > 20 {
                    self.closed.remove(0);
                }
            }
        }
        self.plugin_event("close", Some(index));
        self.lsp_close_tab(index);
        // une autre vue du fichier reprend l'historique d'annulation commun
        self.release_history(index);
        let active_id = self.tabs[self.active].id;
        let tab = self.tabs.remove(index);
        self.tidy_links();
        if tab.syntax_on {
            self.syntax_tx
                .send(syntax::Request::Close { tab: tab.id })
                .ok();
        }
        if self.tabs.is_empty() {
            self.push_tab();
        }
        // l'onglet actif reste le même ; si c'est lui qu'on ferme, le suivant de son volet (sinon
        // le précédent) le remplace, comme celui qu'affiche l'autre volet
        self.active = match self.tab_index(active_id) {
            Some(i) => i,
            None => self
                .neighbor_in_pane(index, tab.pane)
                .unwrap_or_else(|| index.min(self.tabs.len() - 1)),
        };
        if self.shown[tab.pane] == tab.id {
            if let Some(i) = self.neighbor_in_pane(index, tab.pane) {
                self.shown[tab.pane] = self.tabs[i].id;
            }
        }
        self.normalize_panes();
        self.activate(self.active);
    }

    /// Ferme l'onglet `index`, après confirmation s'il contient des modifications.
    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        // fermer une vue jumelle ne perd rien : les modifications restent dans l'autre vue
        let sole_view = self.peers_of(index).is_empty();
        if self.tabs[index].editor.is_modified() && !self.is_bench() && sole_view {
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
                lang: self.lang,
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
                        lang: self.lang,
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

    // --- Taille du texte et tabulation ---------------------------------------------------------

    /// Applique `font_px` et `tab_size` : nouvelle police, nouvelle géométrie.
    fn apply_text_style(&mut self) {
        editing::set_tab_width(self.tab_size);
        for r in [self.renderer.as_mut(), self.second.as_mut()]
            .into_iter()
            .flatten()
        {
            if let Err(e) = r.set_text_style(self.font_px as f32, self.tab_size as f32) {
                eprintln!("taille du texte : {e}");
            }
        }
        self.ime_area = None;
        self.tabs_dirty = true;
        self.sync_viewport();
        self.request_redraw();
    }

    /// Change la taille du texte de `delta` pixels (`0` : taille par défaut) et la retient dans
    /// `config.toml` (par le worker).
    fn zoom(&mut self, delta: i32) {
        use crate::config::{DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE};
        let target = if delta == 0 {
            DEFAULT_FONT_SIZE
        } else {
            (self.font_px as i32 + delta).clamp(MIN_FONT_SIZE as i32, MAX_FONT_SIZE as i32) as u32
        };
        if target == self.font_px {
            return;
        }
        self.font_px = target;
        self.apply_text_style();
        if !self.is_bench() && self.opts.screenshot.is_none() {
            self.jobs
                .send(Job::SaveSetting {
                    key: "font_size",
                    value: target.to_string(),
                })
                .ok();
        }
    }

    // --- Onglets fermés et fichiers récents ----------------------------------------------------

    /// Rouvre le dernier onglet fermé qui n'est pas déjà ouvert, au même endroit.
    fn reopen_closed(&mut self) {
        while let Some(t) = self.closed.pop() {
            if self
                .tabs
                .iter()
                .any(|x| x.editor.path() == Some(t.path.as_path()))
            {
                continue;
            }
            self.open_path_at(t.path.clone(), None);
            if let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|x| x.editor.path() == Some(t.path.as_path()))
            {
                tab.pending_restore = Some((t.line, t.col, t.first));
            }
            return;
        }
    }

    /// Note qu'un fichier vient d'être ouvert : en tête de la liste, et enregistré par le worker.
    fn note_recent(&mut self, path: &std::path::Path) {
        self.recent = session::recent_with(&self.recent, path);
        self.jobs.send(Job::AddRecent(path.to_path_buf())).ok();
    }

    /// Liste des fichiers récents.
    fn open_recent_list(&mut self) {
        let items: Vec<Item> = self
            .recent
            .iter()
            .map(|p| Item {
                label: p.file_name().map_or_else(
                    || p.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                detail: p
                    .parent()
                    .map_or_else(String::new, |d| d.display().to_string()),
                highlights: Vec::new(),
                target: Target::Path(p.clone()),
            })
            .collect();
        if items.is_empty() {
            self.show_error(self.lang.pick(
                "No recent files yet.".to_owned(),
                "Aucun fichier r\u{e9}cent pour l'instant.".to_owned(),
            ));
            return;
        }
        self.open_list_palette(PaletteMode::Recent, items, String::new());
    }

    // --- Fichiers modifiés hors de l'éditeur ---------------------------------------------------

    /// Au retour sur la fenêtre : le worker compare l'empreinte des fichiers ouverts à celle du
    /// disque (un `stat` par fichier, jamais sur le thread UI).
    fn check_files(&mut self) {
        if self.is_bench()
            || (self.opts.screenshot.is_some()
                && std::env::var_os("PAULWRITE_SCREENSHOT_RELOAD").is_none())
        {
            return;
        }
        let files: Vec<_> = self
            .tabs
            .iter()
            .filter(|t| t.loading.is_none() && !t.reloading && t.disk_stamp.is_some())
            .filter_map(|t| Some((t.id, t.editor.path()?.to_path_buf(), t.disk_stamp)))
            .collect();
        if !files.is_empty() {
            self.jobs.send(Job::CheckFiles { files }).ok();
        }
    }

    /// Relit le fichier de l'onglet depuis le disque ; le curseur et la vue sont rétablis.
    fn reload_tab(&mut self, index: usize) {
        let tab = &mut self.tabs[index];
        let Some(path) = tab.editor.path().map(std::path::Path::to_path_buf) else {
            return;
        };
        let (line, col) = tab.editor.caret_line_col();
        tab.pending_restore = Some((line, col, tab.editor.first_line()));
        tab.reloading = true;
        let id = tab.id;
        self.jobs.send(Job::Open { tab: id, path }).ok();
    }

    /// Des fichiers ouverts ont changé sur le disque : ceux sans modification de votre part sont
    /// rechargés sans bruit, pour les autres on demande (une question à la fois).
    fn files_changed(&mut self, changed: Vec<(TabId, Stamp)>) {
        for (id, stamp) in changed {
            let Some(i) = self.tab_index(id) else {
                continue;
            };
            let tab = &self.tabs[i];
            if tab.loading.is_some() || tab.reloading {
                continue;
            }
            if !tab.editor.is_modified() {
                self.reload_tab(i);
            } else if !self.reload_asked && !self.confirming {
                self.reload_asked = true;
                self.jobs
                    .send(Job::ConfirmReload {
                        owner: self.hwnd,
                        tab: id,
                        name: tab.editor.display_name(),
                        stamp,
                        lang: self.lang,
                    })
                    .ok();
            }
        }
    }

    // --- Session -------------------------------------------------------------------------------

    /// État à enregistrer : les onglets qui ont un fichier, le dossier, la barre latérale.
    fn current_session(&self) -> Session {
        let mut tabs = Vec::new();
        let mut active = 0;
        // position, dans la session, de la première vue de chaque fichier à plusieurs vues
        let mut first_view: HashMap<u64, usize> = HashMap::new();
        for (i, t) in self.tabs.iter().enumerate() {
            let Some(path) = t.editor.path() else {
                continue;
            };
            if t.link != 0 {
                // un fichier affiché dans deux vues n'est enregistré qu'une fois
                if let Some(&pos) = first_view.get(&t.link) {
                    if i == self.active {
                        active = pos;
                    }
                    continue;
                }
                first_view.insert(t.link, tabs.len());
            }
            if i == self.active {
                active = tabs.len();
            }
            // Un fichier encore en lecture garde la position qu'on voulait lui rendre.
            let (line, col, first) = match t.pending_restore {
                Some(v) if t.loading.is_some() => v,
                _ => {
                    let (line, col) = t.editor.caret_line_col();
                    (line, col, t.editor.first_line())
                }
            };
            tabs.push(TabState {
                path: path.to_path_buf(),
                line,
                col,
                first,
            });
        }
        Session {
            root: self.project_root.clone(),
            sidebar_open: self.sidebar_open,
            sidebar_width: self.renderer.as_ref().map(|r| r.sidebar_width_logical()),
            active,
            tabs,
        }
    }

    /// Rouvre la session précédente : dossier, barre latérale, onglets (celui qui était actif est
    /// lu en premier), curseur et vue rétablis dès que chaque fichier est chargé.
    fn restore_session(&mut self, session: Session) {
        if let Some(root) = session.root.clone() {
            self.set_project_root(root);
            self.sidebar_open = session.sidebar_open;
            if let (Some(w), Some(r)) = (session.sidebar_width, self.renderer.as_mut()) {
                let scale = self
                    .window
                    .as_ref()
                    .map_or(1.0, |w| w.scale_factor() as f32);
                r.set_sidebar_width_px(w * scale);
            }
        }
        if session.tabs.is_empty() {
            return;
        }
        // Les onglets dans l'ordre de la session (le premier réutilise l'onglet vierge).
        let mut ids = Vec::with_capacity(session.tabs.len());
        for (n, t) in session.tabs.iter().enumerate() {
            let index = if n == 0 { 0 } else { self.push_tab() };
            let tab = &mut self.tabs[index];
            tab.editor.set_path(t.path.clone());
            tab.loading = Some((0, 0));
            tab.pending_restore = Some((t.line, t.col, t.first));
            ids.push((tab.id, t.path.clone()));
        }
        // Lecture : l'onglet actif d'abord, il est le seul visible.
        let active = session.active.min(ids.len() - 1);
        let first = ids.remove(active);
        for (tab, path) in std::iter::once(first).chain(ids) {
            self.jobs.send(Job::Open { tab, path }).ok();
        }
        self.activate(active);
    }

    /// Enregistre la session (attend un court instant que le worker ait écrit le fichier : le
    /// processus s'arrête juste après).
    fn save_session(&mut self) {
        if !self.session_enabled {
            return;
        }
        let (ack, wait) = crossbeam_channel::bounded(1);
        let job = Job::SaveSession {
            session: self.current_session(),
            ack: Some(ack),
        };
        if self.jobs.send(job).is_ok() {
            let _ = wait.recv_timeout(Duration::from_millis(500));
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
        self.save_session();
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
        self.sync_lsp_tab(self.active);
        if self.second.is_some() {
            let other = self.shown_index(1 - self.focus);
            self.sync_lsp_tab(other);
        }
    }

    /// Même chose pour l'onglet `i`.
    fn sync_lsp_tab(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
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
            self.toast(self.lang.pick(
                "Language servers are off: add \u{ab} lsp = true \u{bb} to config.toml",
                "Serveurs de langage désactivés : ajoutez « lsp = true » à config.toml",
            ));
            return;
        }
        let tab = &self.tabs[self.active];
        let Some(path) = tab.lsp_path.clone() else {
            self.toast(self.lang.pick(
                "No language server for this document",
                "Pas de serveur de langage pour ce document",
            ));
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
            palette: self.new_palette(mode),
            root: self.palette_root(),
            finder: None,
            commands: Vec::new(),
            candidate_index: HashMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            reset: true,
            indexing: false,
            generation: 0,
            search_matches: 0,
            replace: None,
            focus_replace: false,
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
            palette: self.new_palette(PaletteMode::Completion),
            root: self.palette_root(),
            finder: None,
            commands: Vec::new(),
            candidate_index: HashMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            reset: true,
            indexing: false,
            generation: 0,
            search_matches: 0,
            replace: None,
            focus_replace: false,
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

    /// Ouvre un dossier : il devient le projet et sa barre latérale s'affiche.
    fn set_project_root(&mut self, path: PathBuf) {
        self.sidebar = Some(SidebarTree::new(path.clone()));
        // Surveille le disque : un fichier créé ou supprimé hors de l'éditeur apparaît tout seul.
        let proxy = self.proxy.clone();
        self.watcher = watch::watch_dir(&path, move |dir| {
            let _ = proxy.send_event(UserEvent::DirChanged(dir));
        });
        self.dirty_dirs.clear();
        self.sidebar_open = true;
        self.project_root = Some(path);
        // l'état Git de l'ancien dossier n'a plus de sens
        self.set_git(None);
        self.request_git();
        self.request_redraw();
    }

    /// Affiche ou masque la barre latérale ; sans dossier ouvert, propose d'en choisir un.
    fn toggle_sidebar(&mut self) {
        if self.sidebar.is_none() {
            self.jobs.send(Job::PickFolder { owner: self.hwnd }).ok();
            return;
        }
        self.sidebar_open = !self.sidebar_open;
        self.request_redraw();
    }

    /// Envoie la barre latérale au moteur de rendu et demande au worker les dossiers à lister.
    fn refresh_sidebar(&mut self) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        match self.sidebar.as_mut() {
            Some(tree) if self.sidebar_open => {
                tree.set_lang(self.lang);
                for dir in tree.take_listing_requests() {
                    self.jobs.send(Job::ListDir { path: dir }).ok();
                }
                let capacity = r.sidebar_capacity();
                r.set_sidebar(Some(tree.render(capacity)));
            }
            _ => r.set_sidebar(None),
        }
    }

    /// Fait défiler la barre latérale à la fraction `f` (0..1) de l'arborescence.
    fn sidebar_scroll_to(&mut self, f: f32) {
        let capacity = self.renderer.as_ref().map_or(20, |r| r.sidebar_capacity());
        if let Some(tree) = self.sidebar.as_mut() {
            tree.scroll_to_fraction(f, capacity);
            self.request_redraw();
        }
    }

    /// Clic dans la barre latérale : déplie un dossier ou ouvre un fichier.
    fn sidebar_click(&mut self, y: f32) {
        let Some(row) = self
            .renderer
            .as_ref()
            .and_then(|r| r.sidebar_row_at(1.0, y))
        else {
            return;
        };
        let Some(tree) = self.sidebar.as_mut() else {
            return;
        };
        match tree.click(row) {
            Click::OpenFile(path) => self.open_path(path),
            Click::Toggled | Click::Nothing => {}
        }
        self.request_redraw();
    }

    /// Liste des onglets ouverts (flèche à droite de la barre d'onglets).
    fn open_tab_list(&mut self) {
        let items: Vec<Item> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| Item {
                label: t.label(),
                detail: String::new(),
                highlights: Vec::new(),
                target: Target::Tab(i),
            })
            .collect();
        self.open_list_palette(PaletteMode::Tabs, items, String::new());
        if let Some(st) = self.palette.as_mut() {
            st.palette
                .select_visible(self.active.min(crate::palette::MAX_VISIBLE - 1));
        }
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
            self.lang
                .pick("No diagnostics", "Aucun diagnostic")
                .to_owned()
        } else {
            self.lang.pick(
                format!("{} diagnostic(s)", items.len()),
                format!("{} diagnostic(s)", items.len()),
            )
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
                    self.toast(self.lang.pick("No completions", "Aucune complétion"));
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
                    None => self.toast(self.lang.pick(
                        "Nothing to show at the cursor",
                        "Rien à afficher sous le curseur",
                    )),
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
                    [] => self.toast(
                        self.lang
                            .pick("Definition not found", "Définition introuvable"),
                    ),
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
                self.toast(self.lang.pick(
                    format!("Language server \u{ab} {language} \u{bb} ready"),
                    format!("Serveur de langage « {language} » prêt"),
                ));
                if let Some(BenchKind::Lsp(n)) = self.opts.bench {
                    self.start_bench_typing(n);
                }
            }
            lsp::Event::ServerError { language, message } => {
                // une seule boîte de message par langage (sinon une par document ouvert)
                if self.lsp_errors_shown.insert(language.clone()) {
                    self.show_error(self.lang.pick(
                        format!(
                            "Language server \u{ab} {language} \u{bb} unavailable:\n{message}\n\nInstall it or change `lsp_{language}` in the configuration."
                        ),
                        format!(
                            "Serveur de langage « {language} » indisponible :\n{message}\n\nInstallez-le ou changez `lsp_{language}` dans la configuration."
                        ),
                    ));
                }
            }
            lsp::Event::ServerExited { language } => {
                self.toast(self.lang.pick(
                    format!("Language server \u{ab} {language} \u{bb} stopped"),
                    format!("Le serveur de langage « {language} » s'est arrêté"),
                ));
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
            self.toast(self.lang.pick("Plugins not loaded", "Plugins non chargés"));
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
                    None => self.toast(self.lang.pick(
                        format!("Unknown command: {name}"),
                        format!("Commande inconnue : {name}"),
                    )),
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

    /// Panneau vide dans la langue de l'interface.
    fn new_palette(&self, mode: PaletteMode) -> Palette {
        let mut p = Palette::new(mode);
        p.set_lang(self.lang);
        p
    }

    /// Applique la langue de l'interface partout : éditeurs, barre latérale, panneau, menus.
    fn apply_lang(&mut self) {
        let lang = self.lang;
        for t in &mut self.tabs {
            t.editor.set_lang(lang);
        }
        if let Some(st) = self.palette.as_mut() {
            st.palette.set_lang(lang);
        }
        if self.hwnd != 0 {
            let (bar, table) = menu::build(lang);
            if platform::set_menu(self.hwnd, &bar) {
                self.menu_commands = table;
            }
        }
        self.refresh_git_label();
        self.tabs_dirty = true;
        self.overlay_dirty = true;
        self.request_redraw();
    }

    /// Choix de la langue dans les préférences : appliqué tout de suite et enregistré dans
    /// `config.toml` (sur le worker).
    fn set_language(&mut self, lang: Lang) {
        self.lang = lang;
        self.apply_lang();
        self.jobs
            .send(Job::SaveSetting {
                key: "language",
                value: lang.code().to_owned(),
            })
            .ok();
        self.toast(lang.pick("Language: English", "Langue : Français"));
    }

    /// Ouvre le panneau dans ce mode (remplace celui qui était ouvert).
    fn open_palette(&mut self, mode: PaletteMode) {
        self.close_palette();
        let root = self.palette_root();
        let mut st = PaletteState {
            palette: self.new_palette(mode),
            root: root.clone(),
            finder: None,
            commands: Vec::new(),
            candidate_index: HashMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            reset: true,
            indexing: false,
            generation: 0,
            search_matches: 0,
            replace: None,
            focus_replace: false,
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
                    let title = cmd.title_in(self.lang);
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
                    let title = self.lang.pick(
                        format!("Plugin: {}", c.title),
                        format!("Plugin : {}", c.title),
                    );
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
            PaletteMode::Find
            | PaletteMode::Replace
            | PaletteMode::ReplaceWith
            | PaletteMode::ProjectReplace => {}
            PaletteMode::GotoLine
            | PaletteMode::Completion
            | PaletteMode::Diagnostics
            | PaletteMode::Locations
            | PaletteMode::Tabs
            | PaletteMode::Recent
            | PaletteMode::Hover => {}
            PaletteMode::Search => {
                st.palette.set_note(format!(
                    "{} {}",
                    self.lang.pick("In", "Dans"),
                    root.file_name().map_or_else(
                        || root.display().to_string(),
                        |n| n.to_string_lossy().into_owned()
                    )
                ));
            }
        }
        if mode == PaletteMode::ProjectReplace {
            self.project_prepare(&mut st);
        }
        let finding = matches!(mode, PaletteMode::Find | PaletteMode::Replace);
        if finding {
            self.find_prepare(&mut st);
        }
        self.palette = Some(st);
        let settled = self.refresh_palette_items(true);
        self.note_settled(settled);
        if finding {
            self.find_opened();
        }
        if mode == PaletteMode::ProjectReplace {
            self.project_refresh_note();
        }
        self.overlay_dirty = true;
        self.request_redraw();
    }

    /// Ferme le panneau et arrête ce qui tournait pour lui.
    fn close_palette(&mut self) {
        if let Some(st) = self.palette.take() {
            st.cancel.store(true, Ordering::Relaxed);
            if matches!(st.palette.mode(), PaletteMode::Find | PaletteMode::Replace) {
                self.find_closed(st.palette.query());
            }
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
                    st.palette.set_note(match (st.indexing, self.lang) {
                        (true, Lang::En) => format!("{total} files indexed\u{2026}"),
                        (true, Lang::Fr) => format!("{total} fichiers index\u{e9}s\u{2026}"),
                        (false, Lang::En) => format!("{matched} of {total} files"),
                        (false, Lang::Fr) => format!("{matched} sur {total} fichiers"),
                    });
                }
                !status.running
            }
            PaletteMode::GotoLine => {
                let items = match parse_goto(st.palette.query()) {
                    Some(n) => {
                        let n = n.min(total_lines);
                        vec![Item {
                            label: self
                                .lang
                                .pick(format!("Go to line {n}"), format!("Aller à la ligne {n}")),
                            detail: format!("sur {total_lines}"),
                            highlights: Vec::new(),
                            target: Target::Line(n),
                        }]
                    }
                    None => Vec::new(),
                };
                st.palette.set_note(if items.is_empty() {
                    self.lang.pick(
                        format!("Line number, from 1 to {total_lines}"),
                        format!("Numéro de ligne, de 1 à {total_lines}"),
                    )
                } else {
                    String::new()
                });
                st.palette.set_items(items, true);
                true
            }
            PaletteMode::Search
            | PaletteMode::Diagnostics
            | PaletteMode::Locations
            | PaletteMode::Tabs
            | PaletteMode::Recent
            | PaletteMode::Find
            | PaletteMode::Replace
            | PaletteMode::ReplaceWith
            | PaletteMode::ProjectReplace
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
            PaletteMode::Diagnostics
            | PaletteMode::Locations
            | PaletteMode::Tabs
            | PaletteMode::Recent
            | PaletteMode::Find
            | PaletteMode::Replace
            | PaletteMode::ReplaceWith
            | PaletteMode::ProjectReplace
            | PaletteMode::Hover => {}
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
            st.palette.set_note(
                self.lang
                    .pick("Type at least 2 characters", "Saisir au moins 2 caractères")
                    .to_owned(),
            );
            return;
        }
        st.palette.set_note(
            self.lang
                .pick("Searching\u{2026}", "Recherche\u{2026}")
                .to_owned(),
        );
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
            Target::Tab(i) => self.activate(i),
            Target::Path(path) => self.open_path(path),
        }
    }

    /// Transmet une touche au panneau et en tire les conséquences.
    fn palette_apply(&mut self, key: &PaletteKey, el: &ActiveEventLoop) {
        if self.project_panel_open() {
            self.project_key(key);
            self.request_redraw();
            return;
        }
        if self.find_panel_open() {
            self.find_key(key);
            self.request_redraw();
            return;
        }
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
                Command::Find => Some(PaletteMode::Find),
                Command::Replace => Some(PaletteMode::Replace),
                Command::ReplaceInProject => Some(PaletteMode::ProjectReplace),
                Command::FindNext | Command::FindPrevious if self.find_panel_open() => {
                    self.find_step(cmd == Command::FindNext);
                    self.touch(at);
                    return;
                }
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
        // Alt+C / Alt+W / Alt+R : casse, mot entier, expression régulière (recherche)
        if (self.find_panel_open() || self.project_panel_open())
            && self.mods.alt_key()
            && !self.mods.control_key()
        {
            if let Key::Character(c) = &plain {
                if let Some(letter) = c.chars().next().map(|l| l.to_ascii_lowercase()) {
                    if matches!(letter, 'c' | 'w' | 'r') {
                        self.find_toggle(letter);
                        self.touch(at);
                        return;
                    }
                }
            }
        }
        let key = match &event.logical_key {
            Key::Named(NamedKey::Tab) => Some(PaletteKey::Tab),
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
                let columns = r.overlay_columns();
                let mut overlay = st.palette.to_overlay(columns);
                // recherche et remplacement : la saisie « remplacer par » vient sous la première ;
                // seul le champ qui a la saisie montre son curseur
                if let Some(rep) = &st.replace {
                    let mut row = rep.to_overlay(columns).rows.remove(0);
                    if st.focus_replace {
                        overlay.rows[0].caret = None;
                    } else {
                        row.caret = None;
                    }
                    overlay.rows.insert(1, row);
                }
                r.set_overlay(Some(&overlay));
            }
            // Les messages brefs s'affichent dans la barre d'état (voir `refresh_chrome`).
            None => r.set_overlay(None),
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
        let (i0, i1) = (self.shown_index(0), self.shown_index(1));
        // le second volet d'abord : le moteur principal recopie son image
        if let Some(r2) = self.second.as_mut() {
            let e = &mut self.tabs[i1].editor;
            e.build_view();
            let scrolled = e.scrolled_since_draw();
            if let Err(err) = r2.draw(&e.view(), scrolled) {
                eprintln!("erreur de rendu du second volet : {err}");
            }
            e.mark_drawn();
        }
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        let t0 = Instant::now();
        let e = &mut self.tabs[i0].editor;
        e.build_view();
        let t1 = Instant::now();
        let scrolled = e.scrolled_since_draw();
        if let Err(err) = r.draw_with(&e.view(), scrolled, self.second.as_ref()) {
            eprintln!("erreur de rendu : {err}");
        }
        e.mark_drawn();
        if self.is_bench() {
            self.stages[4].push(t1 - t0);
            self.stages[5].push(t1.elapsed());
        }
    }

    /// Gouttière, barre d'état, minimap et barre de défilement de chaque volet (la barre d'état
    /// décrit l'onglet du volet qui a le focus).
    fn refresh_chrome(&mut self) {
        self.layout_panes();
        self.refresh_pane_chrome(0);
        if self.second.is_some() {
            self.refresh_pane_chrome(1);
        }
    }

    fn refresh_pane_chrome(&mut self, pane: usize) {
        let i = self.shown_index(pane);
        let r = if pane == 1 {
            self.second.as_mut()
        } else {
            self.renderer.as_mut()
        };
        let Some(r) = r else {
            return;
        };
        let tab = &self.tabs[i];
        if pane == 0 {
            // la barre d'état de la fenêtre : l'onglet qui a le focus
            let focused = &self.tabs[self.active];
            self.status_buf.clear();
            match &self.toast {
                Some((text, _)) => self
                    .status_buf
                    .push_str(text.lines().next().unwrap_or_default()),
                None => focused.editor.write_status(&mut self.status_buf),
            }
            let language = focused
                .language
                .map_or(self.lang.pick("Plain Text", "Texte brut"), |l| {
                    l.display_name()
                });
            let tab_size = self.lang.pick(
                format!("Tab Size: {}", self.tab_size),
                format!("Tabulation : {}", self.tab_size),
            );
            let without_git = [tab_size.as_str(), language];
            let with_git = [self.git_label.as_str(), tab_size.as_str(), language];
            r.set_chrome(&Chrome {
                line_digits: tab.editor.line_digits(),
                status_left: &self.status_buf,
                status_right: if self.git_label.is_empty() {
                    &without_git
                } else {
                    &with_git
                },
                scroll: Some(tab.editor.scroll()),
            });
        } else {
            // le second volet n'a pas de barre d'état
            r.set_chrome(&Chrome {
                line_digits: tab.editor.line_digits(),
                status_left: "",
                status_right: &[],
                scroll: Some(tab.editor.scroll()),
            });
        }
        // La minimap suit le défilement tout de suite, mais le texte tapé avec retard.
        let key = (
            tab.id,
            tab.editor.revision(),
            tab.editor.first_line(),
            tab.editor.overview_version(),
        );
        let missing = r.minimap_needs_source();
        if self.minimap_key[pane] != Some(key) || missing {
            let text_only = !missing
                && self.minimap_key[pane].is_some_and(|(id, _, first, colors)| {
                    id == key.0 && first == key.2 && colors == key.3
                });
            if text_only && self.minimap_at.elapsed() < MINIMAP_DELAY {
                self.minimap_pending = true;
            } else {
                // clé d'image : l'onglet fait partie de la révision (deux onglets peuvent avoir la même)
                let revision = key.1
                    ^ key.0.wrapping_mul(0x9e37_79b9_7f4a_7c15)
                    ^ key.3.wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
                if let Err(e) = r.set_minimap(&tab.editor.minimap(), revision) {
                    eprintln!("minimap : {e}");
                }
                self.minimap_key[pane] = Some(key);
                self.minimap_at = Instant::now();
                self.minimap_pending = false;
            }
        }
        // La gouttière ou la barre d'état peuvent changer le nombre de rangées visibles.
        let rows = r.metrics().rows_for(r.size().1);
        if rows != self.tabs[i].editor.rows() {
            self.tabs[i].editor.set_viewport_rows(rows);
        }
        self.sync_text_layout_for(i, pane);
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let started = Instant::now();
        // les vues jumelles d'un même fichier se rattrapent avant tout le reste
        self.sync_links();
        self.sync_syntax();
        self.sync_lsp();
        self.refresh_tabs();
        self.refresh_overlay();
        self.refresh_sidebar();
        let t_prepared = Instant::now();
        self.refresh_chrome();
        let t_chrome = Instant::now();
        self.draw();
        let t_drawn = Instant::now();
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
        if self.is_bench() {
            self.stages[0].push(t_prepared - started);
            self.stages[1].push(t_chrome - t_prepared);
            self.stages[2].push(t_drawn - t_chrome);
            self.stages[3].push(presented - t_drawn);
        }
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
        self.update_cursor();
        self.update_ime_area();
        self.bench_after_present(el, presented - started);
    }

    fn refresh_title(&mut self) {
        let tab = &self.tabs[self.active];
        let title = match tab.loading {
            Some((done, total)) if total > 0 => format!(
                "{} {} % \u{2014} {}",
                self.lang.pick("Loading\u{2026}", "Chargement\u{2026}"),
                done * 100 / total,
                tab.editor.display_name()
            ),
            Some(_) => format!(
                "{} \u{2014} {}",
                self.lang.pick("Loading\u{2026}", "Chargement\u{2026}"),
                tab.editor.display_name()
            ),
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
        let origin = self.pane_origin(self.focus);
        let (Some(w), Some(r)) = (
            &self.window,
            if self.focus == 1 {
                &self.second
            } else {
                &self.renderer
            },
        ) else {
            return;
        };
        let Some((row, utf16)) = ed!(self).caret_anchor() else {
            return;
        };
        let m = r.metrics();
        // Position approchée (chasse fixe) : suffisante pour placer la fenêtre de l'IME.
        let x = (origin + m.pad_x + utf16 as f32 * m.advance) as i32;
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
        }
        self.layout_panes();
        for p in 0..=usize::from(self.second.is_some()) {
            let i = self.shown_index(p);
            if let Some(rows) = self.rows_in(p) {
                self.tabs[i].editor.set_viewport_rows(rows);
            }
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
                        lang: self.lang,
                    })
                    .ok();
            }
            Effect::Open => {
                self.jobs
                    .send(Job::PickOpen {
                        owner: self.hwnd,
                        lang: self.lang,
                    })
                    .ok();
            }
            Effect::Zoom(d) => self.zoom(d),
            Effect::ReopenClosed => self.reopen_closed(),
            Effect::OpenRecent => self.open_recent_list(),
            Effect::NewWindow => {
                self.jobs.send(Job::NewWindow).ok();
            }
            Effect::NewTab => {
                let i = self.push_tab();
                self.activate(i);
            }
            Effect::CloseTab => self.close_tab(self.active),
            Effect::NextTab => self.step_tab(1),
            Effect::PrevTab => self.step_tab(-1),
            Effect::ToggleSplit => self.toggle_split(),
            Effect::FocusOtherPane => self.focus_other_pane(),
            Effect::MoveToOtherPane => self.move_to_other_pane(),
            Effect::CloneToOtherPane => self.clone_to_other_pane(),
            Effect::OpenPalette(mode) => self.open_palette(mode),
            Effect::OpenFolder => {
                self.jobs.send(Job::PickFolder { owner: self.hwnd }).ok();
            }
            Effect::CloseFolder => {
                self.watcher = None;
                self.dirty_dirs.clear();
                self.project_root = None;
                self.sidebar = None;
                self.sidebar_open = false;
                self.set_git(None);
                self.request_git();
                self.request_redraw();
            }
            Effect::ToggleSidebar => self.toggle_sidebar(),
            Effect::OpenSettings => match Config::path() {
                Some(path) => self.open_path(path),
                None => self.toast(self.lang.pick(
                    "Configuration folder not found (%APPDATA%)",
                    "Dossier de configuration introuvable (%APPDATA%)",
                )),
            },
            Effect::About => self.toast(self.lang.pick(
                format!(
                    "PaulWrite {} \u{2014} fast text editor",
                    env!("CARGO_PKG_VERSION")
                ),
                format!(
                    "PaulWrite {} \u{2014} éditeur de texte rapide",
                    env!("CARGO_PKG_VERSION")
                ),
            )),
            Effect::SetLanguage(lang) => self.set_language(lang),
            Effect::FindStep(forward) => self.find_step(forward),
            Effect::ToggleWordWrap => {
                self.word_wrap = !self.word_wrap;
                let large = !self
                    .config
                    .can_wrap(ed!(self).document().buffer().len_bytes());
                let state = match (self.word_wrap, large) {
                    (true, true) => self.lang.pick(
                        "on (not applied: file too large)",
                        "activé (sans effet : fichier trop gros)",
                    ),
                    (true, false) => self.lang.pick("on", "activé"),
                    (false, _) => self.lang.pick("off", "désactivé"),
                };
                self.toast(format!(
                    "{} {state}",
                    self.lang.pick("Word wrap:", "Retour à la ligne :")
                ));
                self.request_redraw();
            }
            Effect::Lsp(action) => self.lsp_request(action),
            Effect::ReloadPlugins => {
                if let Some(tx) = &self.plugins_tx {
                    self.plugin_reload_requested = true;
                    tx.send(plugins::Request::Reload).ok();
                } else {
                    self.toast(self.lang.pick("Plugins not loaded", "Plugins non chargés"));
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

    /// Capture d'écran : au bout d'un instant (lecture du fichier, coloration), ouvre le panneau
    /// demandé, puis écrit l'image de la fenêtre et quitte.
    fn screenshot_step(&mut self, step: u8, el: &ActiveEventLoop) {
        let proxy = self.proxy.clone();
        let later = move |ms: u64, step: u8| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(ms));
                let _ = proxy.send_event(UserEvent::Screenshot(step));
            });
        };
        if step == 0 {
            match std::env::var("PAULWRITE_SCREENSHOT_PALETTE").as_deref() {
                Ok("files") => self.open_palette(PaletteMode::Files),
                Ok("commands") => self.open_palette(PaletteMode::Commands),
                Ok("find") => self.open_palette(PaletteMode::Find),
                Ok("replace") => self.open_palette(PaletteMode::Replace),
                Ok("recent") => self.open_recent_list(),
                Ok("projectreplace") => self.open_palette(PaletteMode::ProjectReplace),
                _ => {}
            }
            // `PAULWRITE_SCREENSHOT_REOPEN=1` : ferme l'onglet actif (après avoir déplacé le curseur)
            // puis le rouvre.
            if std::env::var_os("PAULWRITE_SCREENSHOT_REOPEN").is_some() && self.tabs.len() > 1 {
                ed!(self).goto(3, 2);
                let i = self.active;
                self.close_now(i);
                self.reopen_closed();
            }
            // `PAULWRITE_SCREENSHOT_RELOAD=1` : le fichier ouvert est réécrit « par un autre programme »
            // puis on simule le retour sur la fenêtre (vérification des fichiers modifiés).
            if std::env::var_os("PAULWRITE_SCREENSHOT_RELOAD").is_some() {
                if let Some(path) = self.tabs[self.active]
                    .editor
                    .path()
                    .map(std::path::Path::to_path_buf)
                {
                    let _ = std::fs::write(
                        &path,
                        "texte modifie par un autre programme
second ligne
",
                    );
                }
                self.check_files();
            }
            // `PAULWRITE_SCREENSHOT_TYPE=texte` : saisi dans le panneau ouvert (recherche).
            if let Ok(text) = std::env::var("PAULWRITE_SCREENSHOT_TYPE") {
                if self.palette.is_some() {
                    let key = PaletteKey::Text(text);
                    self.palette_apply(&key, el);
                }
            }
            // `PAULWRITE_SCREENSHOT_COMMANDS=fold.fold_all,cursor.down` : commandes de l'éditeur
            // exécutées avant la capture ; `PAULWRITE_SCREENSHOT_HOVER=1` : souris sur la gouttière.
            if let Ok(list) = std::env::var("PAULWRITE_SCREENSHOT_COMMANDS") {
                for cmd in list.split(',').filter_map(|n| Command::from_name(n.trim())) {
                    // seuls les effets de disposition (volets, onglets) sont exécutés : jamais
                    // d'enregistrement ni de boîte de dialogue pendant une capture
                    if let Some(
                        effect @ (Effect::ToggleSplit
                        | Effect::FocusOtherPane
                        | Effect::MoveToOtherPane
                        | Effect::CloneToOtherPane
                        | Effect::NewTab
                        | Effect::NextTab
                        | Effect::PrevTab),
                    ) = ed!(self).execute(cmd, 0)
                    {
                        self.run_effect(effect, el);
                    }
                }
            }
            if std::env::var_os("PAULWRITE_SCREENSHOT_HOVER").is_some() {
                if let Some(r) = self.renderer.as_mut() {
                    r.set_fold_hover(true);
                }
                self.fold_hover = true;
            }
            later(800, 1);
            return;
        }
        // `PAULWRITE_SCREENSHOT_LANG=fr|en` : langue de la capture (la configuration l'emporterait).
        if let Some(lang) = std::env::var("PAULWRITE_SCREENSHOT_LANG")
            .ok()
            .and_then(|l| Lang::from_code(&l))
        {
            self.lang = lang;
            self.apply_lang();
        }
        self.redraw(el);
        if let (Some(path), Some(r)) = (&self.opts.screenshot, self.renderer.as_ref()) {
            let (w, h) = r.size();
            if let Err(e) = paulwrite_render::write_png(path, w, h, r.pixels()) {
                eprintln!("capture : {e}");
            }
        }
        self.save_session();
        el.exit();
    }

    /// Pointeur sous la souris : le curseur de texte seulement sur le texte (et la saisie du
    /// panneau), la flèche partout ailleurs (barre latérale, onglets, gouttière, minimap, barre
    /// d'état, liste du panneau).
    fn update_cursor(&mut self) {
        let pane = self.pane_at(self.mouse.x as f32);
        let origin = self.pane_origin(pane);
        let (Some(main), Some(w)) = (self.renderer.as_ref(), self.window.as_ref()) else {
            return;
        };
        let r = if pane == 1 {
            self.second.as_ref().unwrap_or(main)
        } else {
            main
        };
        let (x, y) = (self.mouse.x as f32, self.mouse.y as f32);
        let m = r.metrics();
        let resizing = self.sidebar_drag == Some(SidebarDrag::Resize);
        let icon = if self.palette.is_some() {
            if main.overlay_row_at(x, y) == Some(0) {
                CursorIcon::Text
            } else {
                CursorIcon::Default
            }
        } else if resizing || main.sidebar_edge_hit(x, y) {
            CursorIcon::ColResize
        } else if self.strip_drag.is_some() {
            CursorIcon::Default
        } else {
            let bottom = (main.size().1 as i32 - main.metrics().status_bar) as f32;
            let left = origin + (m.sidebar + m.gutter) as f32;
            let right = origin + (r.view_width() - m.minimap) as f32;
            if x >= left && x < right && y >= m.top as f32 && y < bottom {
                CursorIcon::Text
            } else {
                CursorIcon::Default
            }
        };
        if icon != self.cursor_icon {
            self.cursor_icon = icon;
            w.set_cursor(Cursor::Icon(icon));
        }
    }

    /// Survol : les flèches de repli (▼) n'apparaissent que sur la gouttière, et ce qui est sous
    /// le pointeur (flèche, onglet, ligne de la barre latérale) passe en blanc au lieu de gris.
    fn update_hover(&mut self, inside: bool) {
        let pane = self.pane_at(self.mouse.x as f32);
        let origin = self.pane_origin(pane);
        let (Some(main), second) = (self.renderer.as_mut(), self.second.as_mut()) else {
            return;
        };
        // `r` : le moteur du volet sous la souris ; `other` : l'autre volet, qui ne survole rien
        let (r, mut other) = match (pane, second) {
            (1, Some(s)) => (s, Some(main)),
            (_, s) => (main, s),
        };
        let (x, y) = (self.mouse.x as f32 - origin, self.mouse.y as f32);
        let m = r.metrics();
        let bottom = (r.size().1 as i32 - m.status_bar) as f32;
        let free = inside && self.palette.is_none() && self.strip_drag.is_none();
        let gutter = free
            && x >= m.sidebar as f32
            && x < (m.sidebar + m.gutter) as f32
            && y >= m.top as f32
            && y < bottom;
        let hot = (gutter && r.gutter_fold_hit(x))
            .then(|| ((y as i32 - m.top) / m.line_height.max(1)) as usize);
        let tab = if free { r.tab_at(x, y) } else { None };
        let side = if free { r.sidebar_row_at(x, y) } else { None };
        let now = Hover {
            pane,
            gutter,
            fold_arrow: hot,
            tab,
            sidebar_row: side,
        };
        if now != self.hover {
            self.hover = now;
            self.fold_hover = gutter;
            r.set_fold_hover(gutter);
            r.set_fold_hot(hot);
            r.set_hover_tab(tab);
            r.set_hover_sidebar(side);
            if let Some(o) = other.as_mut() {
                o.set_fold_hover(false);
                o.set_fold_hot(None);
                o.set_hover_tab(None);
                o.set_hover_sidebar(None);
            }
            self.request_redraw();
        }
    }

    /// Fait défiler le document d'après un clic dans la minimap ou la barre de défilement.
    fn strip_apply(&mut self, hit: StripHit) {
        match hit {
            StripHit::Minimap { line } => ed!(self).scroll_to_center(line),
            StripHit::Scrollbar { fraction } => ed!(self).scroll_to_fraction(fraction),
        }
        self.touch(Instant::now());
    }

    /// Glissement dans la zone de droite : seule l'ordonnée de la souris compte.
    fn strip_scroll(&mut self, scrollbar: bool) {
        let Some(r) = (if self.focus == 1 {
            self.second.as_ref()
        } else {
            self.renderer.as_ref()
        }) else {
            return;
        };
        let m = r.metrics();
        let w = r.view_width() as f32;
        let x = if scrollbar {
            w - 1.0
        } else {
            w - m.minimap as f32 + 1.0
        };
        let bottom = (r.size().1 as i32 - m.status_bar) as f32;
        let y = (self.mouse.y as f32).clamp(m.top as f32, bottom - 1.0);
        if let Some(hit) = r.strip_hit(x, y) {
            self.strip_apply(hit);
        }
    }

    /// Clic dans la gouttière : sélectionne la ligne.
    fn select_row_at_mouse(&mut self) {
        let local_x = self.mouse.x as f32 - self.pane_origin(self.focus);
        let Some(r) = (if self.focus == 1 {
            self.second.as_ref()
        } else {
            self.renderer.as_ref()
        }) else {
            return;
        };
        let m = r.metrics();
        let row = ((self.mouse.y as i32 - m.top).max(0) / m.line_height.max(1)) as usize;
        let row = row.min(ed!(self).rows().saturating_sub(1));
        if r.gutter_fold_hit(local_x) {
            // la flèche replie ou déplie ; hors flèche, un clic sélectionne la ligne
            ed!(self).toggle_fold_row(row);
        } else {
            ed!(self).select_row(row);
        }
    }

    /// Place le curseur sous la souris (clic ou glissement).
    fn mouse_to_caret(&mut self, extend: bool) {
        // abscisse de la souris dans le volet qui a le focus
        let mouse_x = self.mouse.x as f32 - self.pane_origin(self.focus);
        let Some(r) = fr_mut!(self) else {
            return;
        };
        let m = r.metrics();
        ed!(self).build_view();
        let y = (self.mouse.y as i32 - m.top).max(0);
        let row = (y / m.line_height.max(1)) as usize;
        let row = row.min(ed!(self).rows().saturating_sub(1));
        // le repère « … » d'une ligne repliée : un clic dessus la déplie
        if !extend && ed!(self).row_is_folded(row) {
            let width = crate::wrap::visual_width(ed!(self).row_plain_text(row).chars());
            let x0 = m.pad_x + width as f32 * m.advance;
            let x = mouse_x;
            if x >= x0 - 0.3 * m.advance && x < x0 + 2.0 * m.advance {
                ed!(self).toggle_fold_row(row);
                return;
            }
        }
        let utf16 = r
            .column_at(ed!(self).row_plain_text(row), mouse_x)
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
        if let Some(r) = self.renderer.as_ref() {
            let (w, h) = r.size();
            report.push_str(&format!(
                "  fenêtre {w}x{h} px
"
            ));
        }
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
        if let [Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)] =
            self.stages.each_ref().map(Samples::summary)
        {
            report.push_str(&format!(
                "  dessin = construction des rangées {:.2}/{:.2} + moteur de rendu {:.2}/{:.2}
",
                e.mean, e.p99, f.mean, f.p99
            ));
            report.push_str(&format!(
                "  étapes d'une image (moyenne, p99 en ms) : préparation {:.2}/{:.2} | interface {:.2}/{:.2} | dessin {:.2}/{:.2} | copie {:.2}/{:.2}
",
                a.mean, a.p99, b.mean, b.p99, c.mean, c.p99, d.mean, d.p99
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
        let offscreen = bench_offscreen
            || self.opts.screenshot.is_some()
            || std::env::var_os("PAULWRITE_OFFSCREEN").is_some();
        let mut attrs = Window::default_attributes()
            .with_title(ed!(self).title())
            .with_inner_size(LogicalSize::new(1100.0, 720.0))
            .with_visible(false);
        // Icône de l'exécutable (ressource 1, voir `build.rs`) : barre de titre et barre des tâches.
        if let Ok(icon) = Icon::from_resource(1, None) {
            attrs = attrs
                .with_window_icon(Some(icon.clone()))
                .with_taskbar_icon(Some(icon));
        }
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
        // Barre de menus native (avant de lire la taille : elle réduit la zone cliente).
        if !self.is_bench() {
            let (bar, table) = menu::build(self.lang);
            if platform::set_menu(self.hwnd, &bar) {
                self.menu_commands = table;
            }
        }
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
        if self.opts.screenshot.is_some() {
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                let _ = proxy.send_event(UserEvent::Screenshot(0));
            });
        }

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
            UserEvent::Screenshot(step) => self.screenshot_step(step, el),
            UserEvent::Menu(id) => {
                let index = id.wrapping_sub(menu::MENU_BASE) as usize;
                if let Some(cmd) = self.menu_commands.get(index).copied() {
                    self.execute(cmd, el);
                    self.touch(Instant::now());
                }
            }
            UserEvent::Plugin(resp) => match resp {
                plugins::Response::Loaded(report) => {
                    self.plugin_commands = report.commands.clone();
                    self.plugin_load =
                        Some((report.loaded, report.commands.len(), report.load_time));
                    if !report.errors.is_empty() {
                        let shown: Vec<&str> =
                            report.errors.iter().take(5).map(String::as_str).collect();
                        self.show_error(self.lang.pick(
                            format!(
                                "Plugin errors ({} in total):\n{}",
                                report.errors.len(),
                                shown.join("\n")
                            ),
                            format!(
                                "Erreurs de plugin ({} au total) :\n{}",
                                report.errors.len(),
                                shown.join("\n")
                            ),
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
                        self.toast(
                            self.lang
                                .pick(format!("Plugin: {e}"), format!("Plugin : {e}")),
                        );
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
            UserEvent::Found(ev) => self.on_found(ev),
            UserEvent::Git(result) => self.on_git(result),
            UserEvent::Diff(result) => self.on_diff(result),
            UserEvent::DirChanged(dir) => {
                // Seuls comptent les dossiers déjà lus ; les entrailles de `.git` changent sans cesse.
                let git = dir.components().any(|c| c.as_os_str() == ".git");
                let known = self.sidebar.as_ref().is_some_and(|t| t.is_listed(&dir));
                // un changement hors de `.git`, ou directement dans `.git` (index, HEAD : commit,
                // changement de branche) : l'état Git est à refaire ; pas ses entrailles
                if !git || dir.file_name().is_some_and(|n| n == ".git") {
                    self.request_git();
                }
                // un commit ou un changement de branche change aussi les marqueurs de la gouttière
                if dir.file_name().is_some_and(|n| n == ".git") {
                    self.request_diff(self.active);
                }
                if known && !git {
                    self.dirty_dirs.insert(dir);
                    self.refresh_due = Some(Instant::now() + DIR_REFRESH_DELAY);
                }
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
                if resp.overview {
                    // coloration du document entier : seulement pour la minimap
                    self.tabs[i].editor.set_overview(resp.range, resp.spans);
                    if i == self.active {
                        self.request_redraw();
                    }
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
                        self.word_wrap = c.word_wrap && !self.opts.no_wrap;
                        self.lang = c.lang();
                        // le style (police, tabulation) n'est refait que s'il change : rien à payer
                        // au démarrage avec les réglages par défaut
                        let restyle = c.font_size != self.font_px || c.tab_size != self.tab_size;
                        self.font_px = c.font_size;
                        self.tab_size = c.tab_size;
                        self.config = c;
                        self.apply_lang();
                        if restyle {
                            self.apply_text_style();
                        }
                    }
                }
                Done::Session(Some(session)) => {
                    // seulement si l'utilisateur n'a encore rien ouvert
                    if self.session_enabled && self.tabs.len() == 1 && self.tabs[0].is_pristine() {
                        self.restore_session(session);
                    }
                }
                Done::Session(None) => {}
                Done::ProjectReplaced(report) => self.on_project_report(report),
                Done::ProjectConfirmed(yes) => self.on_project_confirmed(yes),
                Done::Recent(list) => {
                    // ceux déjà ouverts depuis le lancement restent en tête
                    let mine = std::mem::take(&mut self.recent);
                    let rest: Vec<PathBuf> =
                        list.into_iter().filter(|p| !mine.contains(p)).collect();
                    self.recent = mine.into_iter().chain(rest).collect();
                    self.recent.truncate(session::MAX_RECENT);
                }
                Done::FilesChanged(changed) => self.files_changed(changed),
                Done::ReloadConfirmed { tab, reload, stamp } => {
                    self.reload_asked = false;
                    if let Some(i) = self.tab_index(tab) {
                        if reload {
                            self.reload_tab(i);
                        } else {
                            // on garde votre version : on ne redemande pas pour ce changement
                            self.tabs[i].disk_stamp = Some(stamp);
                        }
                    }
                }
                Done::FolderPicked(Some(path)) => self.set_project_root(path),
                Done::FolderPicked(None) => {}
                Done::DirListed { path, entries } => {
                    if let Some(tree) = self.sidebar.as_mut() {
                        tree.set_listing(path, entries);
                        self.request_redraw();
                    }
                }
                Done::Directory { tab, path } => {
                    // Un dossier devient la racine du projet ; son onglet n'a pas de raison d'être.
                    self.set_project_root(path);
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
                Done::Loaded {
                    tab,
                    path,
                    result,
                    stamp,
                } => {
                    let Some(i) = self.tab_index(tab) else {
                        return; // onglet fermé pendant la lecture
                    };
                    self.tabs[i].loading = None;
                    self.tabs[i].reloading = false;
                    self.tabs[i].disk_stamp = stamp;
                    match result {
                        Ok(doc) => {
                            // un fichier ouvert par l'utilisateur (pas une reprise ni un rechargement)
                            if self.tabs[i].pending_restore.is_none() && !self.is_bench() {
                                self.note_recent(&path);
                            }
                            self.tabs[i].editor.load(doc, Some(path));
                            // un fichier à deux vues : le nouveau document garde son journal
                            if self.tabs[i].link != 0 {
                                self.tabs[i].editor.set_mirror(true);
                            }
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
                            // sans dossier ouvert, l'état Git suit le fichier de l'onglet actif
                            if i == self.active {
                                if self.project_root.is_none() {
                                    self.request_git();
                                }
                                self.request_diff(i);
                            }
                            if let Some(l) = self.tabs[i].pending_goto.take() {
                                self.tabs[i].editor.goto(l, 0);
                            }
                            if let Some((line, col, first)) = self.tabs[i].pending_restore.take() {
                                let e = &mut self.tabs[i].editor;
                                e.goto(line, col.saturating_sub(1));
                                e.set_first_line(first);
                            }
                            self.attach_syntax(i, None);
                            self.plugin_event("open", Some(i));
                            self.lsp_open_tab(i);
                        }
                        Err(e) => {
                            self.show_error(self.lang.pick(
                                format!("Cannot open \u{ab} {} \u{bb}:\n{e}", path.display()),
                                format!("Impossible d'ouvrir « {} » :\n{e}", path.display()),
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
                    stamp,
                } => {
                    let index = self.tab_index(tab);
                    match result {
                        Ok(()) => {
                            if let Some(i) = index {
                                if self.tabs[i].editor.path() == Some(path.as_path()) {
                                    self.tabs[i].editor.mark_saved(revision);
                                    self.tabs[i].disk_stamp = stamp;
                                    self.mark_peers_saved(i, revision);
                                }
                                self.tabs_dirty = true;
                                self.request_git();
                                self.request_diff(i);
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
                            self.show_error(self.lang.pick(
                                format!("Cannot save \u{ab} {} \u{bb}:\n{e}", path.display()),
                                format!(
                                    "Échec de l'enregistrement de « {} » :\n{e}",
                                    path.display()
                                ),
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
                        // le nom vaut pour toutes les vues du fichier
                        for j in self.peers_of(i) {
                            self.tabs[j].editor.set_path(path.clone());
                            self.attach_syntax(j, None);
                        }
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
            WindowEvent::Focused(true) => {
                self.check_files();
                self.request_git();
                self.request_diff(self.active);
            }
            WindowEvent::Resized(_) => {
                self.sync_viewport();
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Déplacement vers un autre écran / changement de DPI : nouvelle police.
                for r in [self.renderer.as_mut(), self.second.as_mut()]
                    .into_iter()
                    .flatten()
                {
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
                // `PAULWRITE_KEYLOG=1` : écrit chaque frappe sur la sortie d'erreur (diagnostic d'un
                // raccourci qui ne réagit pas ; lancer `paulwrite` depuis un terminal).
                if self.keylog {
                    eprintln!(
                        "touche : sans modificateur {:?} | produite {:?} | physique {:?} | modificateurs {:?}",
                        plain, event.logical_key, event.physical_key, self.mods
                    );
                }
                // Raccourci à deux temps : Ctrl+K puis Ctrl+B (barre latérale), comme Sublime Text.
                let is_letter =
                    |k: &str| matches!(&plain, Key::Character(s) if s.eq_ignore_ascii_case(k));
                let ctrl_only = self.mods.control_key() && !self.mods.alt_key();
                if std::mem::take(&mut self.chord_k) && ctrl_only && is_letter("b") {
                    self.execute(Command::ToggleSidebar, el);
                    self.touch(at);
                    return;
                }
                if ctrl_only && !self.mods.shift_key() && is_letter("k") {
                    self.chord_k = true;
                    return;
                }
                if let Some(cmd) = keymap::command_for_event(&plain, &event.logical_key, self.mods)
                {
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
                // Ctrl+molette : taille du texte
                if self.mods.control_key() {
                    let y = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 20.0,
                    };
                    if y.abs() >= 0.5 {
                        self.zoom(if y > 0.0 { 1 } else { -1 });
                    }
                    return;
                }
                let on_sidebar = self
                    .renderer
                    .as_ref()
                    .is_some_and(|r| (self.mouse.x as i32) < r.metrics().sidebar);
                if on_sidebar {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -y * 3.0,
                        MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / 24.0,
                    };
                    let capacity = self.renderer.as_ref().map_or(20, |r| r.sidebar_capacity());
                    if let Some(tree) = self.sidebar.as_mut() {
                        tree.scroll_by(lines.round() as isize, capacity);
                        self.request_redraw();
                    }
                    return;
                }
                // défilement horizontal : molette latérale, ou Maj + molette (sans retour à la ligne)
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x, y),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32 / 20.0, p.y as f32 / 20.0),
                };
                let (dx, dy) = if self.mods.shift_key() {
                    (dy, 0.0)
                } else {
                    (dx, dy)
                };
                // la molette agit sur le volet sous la souris, même sans le focus
                let under = self.shown_index(self.pane_at(self.mouse.x as f32));
                if dx != 0.0 {
                    self.tabs[under]
                        .editor
                        .scroll_horizontally(-(dx * 6.0).round() as isize);
                    self.touch(Instant::now());
                    if dy == 0.0 {
                        return;
                    }
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
                    self.tabs[under].editor.scroll_by(whole as isize);
                    self.touch(Instant::now());
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = position;
                self.update_cursor();
                self.update_hover(true);
                match self.sidebar_drag {
                    Some(SidebarDrag::Resize) => {
                        if let Some(r) = self.renderer.as_mut() {
                            r.set_sidebar_width_px(position.x as f32);
                        }
                        self.request_redraw();
                        return;
                    }
                    Some(SidebarDrag::Scroll) => {
                        let f = self.renderer.as_ref().and_then(|r| {
                            r.sidebar_scroll_hit(
                                r.metrics().sidebar as f32 - 1.0,
                                position.y as f32,
                            )
                        });
                        if let Some(f) = f {
                            self.sidebar_scroll_to(f);
                        }
                        return;
                    }
                    None => {}
                }
                if let Some(scrollbar) = self.strip_drag {
                    self.strip_scroll(scrollbar);
                } else if self.dragging {
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
                // Barre latérale : son bord se glisse pour la redimensionner, son curseur pour
                // la faire défiler.
                if button == MouseButton::Left {
                    let grab = self.renderer.as_ref().and_then(|r| {
                        if r.sidebar_edge_hit(x, y) {
                            Some((SidebarDrag::Resize, None))
                        } else {
                            r.sidebar_scroll_hit(x, y)
                                .map(|f| (SidebarDrag::Scroll, Some(f)))
                        }
                    });
                    if let Some((drag, fraction)) = grab {
                        self.sidebar_drag = Some(drag);
                        if let Some(f) = fraction {
                            self.sidebar_scroll_to(f);
                        }
                        return;
                    }
                }
                // Écran partagé : un clic dans l'autre volet (hors barre d'état) lui donne d'abord
                // le focus ; la suite agit sur lui, avec des abscisses relatives à son bord gauche.
                let status_top = self
                    .renderer
                    .as_ref()
                    .map_or(i32::MAX, |r| r.size().1 as i32 - r.metrics().status_bar);
                let pane = self.pane_at(x);
                if pane != self.focus && (y as i32) < status_top {
                    let i = self.shown_index(pane);
                    self.activate(i);
                }
                let x = x - self.pane_origin(self.focus);
                let focused = if self.focus == 1 {
                    self.second.as_ref()
                } else {
                    self.renderer.as_ref()
                };
                let (hit, tab_button, strip, area) = match focused {
                    Some(r) => {
                        let m = r.metrics();
                        let bottom = r.size().1 as i32 - m.status_bar;
                        if x < m.sidebar as f32 && (y as i32) < bottom {
                            // la barre latérale prend tous les clics de sa colonne
                            if button == MouseButton::Left {
                                self.sidebar_click(y);
                            }
                            return;
                        }
                        if (y as i32) >= bottom && r.status_sidebar_button_at(x, y) {
                            if button == MouseButton::Left {
                                self.toggle_sidebar();
                            }
                            return;
                        }
                        (
                            r.tab_at(x, y),
                            r.tab_button_at(x, y),
                            r.strip_hit(x, y),
                            // zone des rangées : (gouttière ?, à l'intérieur ?)
                            Some((
                                x >= m.sidebar as f32 && x < (m.sidebar + m.gutter) as f32,
                                (self.mouse.y as i32) >= m.top && (self.mouse.y as i32) < bottom,
                            )),
                        )
                    }
                    None => (None, None, None, None),
                };
                let in_rows = area.is_some_and(|(_, inside)| inside);
                let in_gutter = area.is_some_and(|(gutter, _)| gutter);
                // indice de l'onglet dans la barre du volet -> indice dans `tabs`
                let hit = hit
                    .and_then(|(i, close)| self.pane_tabs(self.focus).get(i).map(|&g| (g, close)));
                match (button, hit, tab_button, strip) {
                    (MouseButton::Left, _, Some(b), _) => match b {
                        TabButton::Previous => self.step_tab(-1),
                        TabButton::Next => self.step_tab(1),
                        TabButton::New => {
                            let i = self.push_tab();
                            self.activate(i);
                        }
                        TabButton::List => self.open_tab_list(),
                    },
                    (MouseButton::Left, Some((i, true)), _, _)
                    | (MouseButton::Middle, Some((i, _)), _, _) => self.close_tab(i),
                    (MouseButton::Left, Some((i, false)), _, _) => self.activate(i),
                    (MouseButton::Left, None, None, Some(hit)) => {
                        self.strip_drag = Some(matches!(hit, StripHit::Scrollbar { .. }));
                        self.strip_apply(hit);
                    }
                    (MouseButton::Left, None, None, None) if in_rows && in_gutter => {
                        self.select_row_at_mouse();
                        self.touch(Instant::now());
                    }
                    (MouseButton::Left, None, None, None) if in_rows => {
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
            } => {
                self.dragging = false;
                self.strip_drag = None;
                self.sidebar_drag = None;
            }
            WindowEvent::CursorLeft { .. } => self.update_hover(false),
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
        if self.minimap_pending && now >= self.minimap_at + MINIMAP_DELAY {
            self.request_redraw();
        }
        if let Some(due) = self.refresh_due {
            if now >= due {
                self.refresh_due = None;
                for dir in self.dirty_dirs.drain() {
                    self.jobs.send(Job::ListDir { path: dir }).ok();
                }
            }
        }
        if let Some(due) = self.overview_due {
            if now >= due {
                self.overview_due = None;
                let tab = &self.tabs[self.active];
                if tab.syntax_on {
                    self.syntax_tx
                        .send(syntax::Request::Overview { tab: tab.id })
                        .ok();
                }
            }
        }
        let mut wake = self
            .toast
            .as_ref()
            .map_or(self.next_blink, |(_, until)| (*until).min(self.next_blink));
        if let Some(due) = self.overview_due {
            wake = wake.min(due);
        }
        if let Some(due) = self.refresh_due {
            wake = wake.min(due);
        }
        if self.minimap_pending {
            wake = wake.min(self.minimap_at + MINIMAP_DELAY);
        }
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

/// Lance l'application.
///
/// # Errors
/// Échec de création de la boucle d'événements.
pub fn run(opts: Options) -> Result<(), String> {
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    // Les choix de menu arrivent comme messages Windows bruts : on les transmet à l'application.
    builder.with_msg_hook(|msg| match platform::menu_command(msg) {
        Some(id) => {
            if let Some(p) = MENU_PROXY.get() {
                let _ = p.send_event(UserEvent::Menu(id));
            }
            true
        }
        None => false,
    });
    let event_loop = builder.build().map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let _ = MENU_PROXY.set(proxy.clone());
    let worker_proxy = proxy.clone();
    let jobs = worker::spawn(move |done| {
        let _ = worker_proxy.send_event(UserEvent::Done(done));
    });
    let syntax_proxy = proxy.clone();
    let syntax_tx = syntax::spawn(move |resp| {
        let _ = syntax_proxy.send_event(UserEvent::Syntax(resp));
    });
    let git_proxy = proxy.clone();
    let git_tx = git::spawn(move |result| {
        let _ = git_proxy.send_event(UserEvent::Git(result));
    });
    let diff_proxy = proxy.clone();
    let diff_tx = git::spawn_diff(move |result| {
        let _ = diff_proxy.send_event(UserEvent::Diff(result));
    });
    let mut app = App::new(opts, jobs, syntax_tx, git_tx, diff_tx, proxy);
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
        assert!(parse(&["--new-window"]).unwrap().new_window);
        assert!(!parse(&["a.txt"]).unwrap().new_window);
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
