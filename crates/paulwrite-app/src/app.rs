//! Application winit : fenêtre, onglets, événements clavier/souris/IME/DPI, workers (fichiers et
//! coloration syntaxique), et modes de mesure (`--bench-*`) de la latence et du démarrage.
//!
//! Le thread UI ne fait ni E/S ni calcul lourd : fichiers et boîtes de dialogue passent par le
//! worker ([`crate::worker`]), l'analyse syntaxique par son thread dédié. Le rendu est à la
//! demande : une image n'est dessinée que si un événement l'a demandée (aucune boucle
//! permanente ; seul le clignotement du curseur réveille la boucle deux fois par seconde).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use paulwrite_render::{platform, since_process_start, Colors, SoftRenderer, TabInfo};
use paulwrite_syntax::{self as syntax, Language};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Cursor, CursorIcon, Window, WindowId};

use crate::command::{Command, Effect};
use crate::config::Config;
use crate::editor::Preedit;
use crate::keymap;
use crate::metrics::Samples;
use crate::tab::{Tab, SYNTAX_MARGIN_LINES};
use crate::worker::{self, Done, Job, TabId};

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
}

/// Options de la ligne de commande.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Fichiers à ouvrir (un onglet chacun).
    pub paths: Vec<PathBuf>,
    pub bench: Option<BenchKind>,
    /// Fichier où ajouter le rapport de mesure (en plus de la sortie standard).
    pub bench_out: Option<PathBuf>,
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
        // `absolute` ne touche pas au disque : seulement le dossier courant.
        let path = std::path::absolute(&path).unwrap_or(path);
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.editor.path() == Some(path.as_path()))
        {
            self.activate(i);
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
            self.activate(index);
            let name = self.tabs[index].editor.display_name();
            match platform::confirm_save(self.hwnd, &name) {
                platform::SaveChoice::Save => {
                    self.after_save = Some(AfterSave::CloseTab(self.tabs[index].id));
                    self.start_save(index);
                }
                platform::SaveChoice::Discard => self.close_now(index),
                platform::SaveChoice::Cancel => {}
            }
        } else {
            self.close_now(index);
        }
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
            let name = self.tabs[index].editor.display_name();
            match platform::confirm_save(self.hwnd, &name) {
                platform::SaveChoice::Save => {
                    self.after_save = Some(AfterSave::ContinueQuit);
                    self.start_save(index);
                    return;
                }
                platform::SaveChoice::Discard => {}
                platform::SaveChoice::Cancel => {
                    self.quit_queue.clear();
                    return;
                }
            }
        }
        el.exit();
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
        self.refresh_tabs();
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
            )
        );
        let mut attrs = Window::default_attributes()
            .with_title(ed!(self).title())
            .with_inner_size(LogicalSize::new(1100.0, 720.0))
            .with_visible(false);
        if self.is_bench() {
            attrs = attrs.with_active(false);
        }
        if bench_offscreen {
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
                };
                self.attach_syntax(self.active, Some(Language::Rust));
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
                self.input_at.get_or_insert(at);
                self.last_edit_at = Some(at);
                self.request_redraw();
            }
            UserEvent::BenchDone => self.finish_bench(el),
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
                Done::Config(c) => self.config = c,
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
                            self.attach_syntax(i, None);
                        }
                        Err(e) => {
                            platform::show_error(
                                self.hwnd,
                                &format!("Impossible d'ouvrir « {} » :\n{e}", path.display()),
                            );
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
                            platform::show_error(
                                self.hwnd,
                                &format!(
                                    "Échec de l'enregistrement de « {} » :\n{e}",
                                    path.display()
                                ),
                            );
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
                        self.tabs[i].editor.set_path(path);
                        // Le nouveau nom peut changer le langage (« .txt » devenu « .rs »).
                        self.attach_syntax(i, None);
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
        if now >= self.next_blink {
            let on = !ed!(self).caret_visible();
            ed!(self).set_caret_visible(on);
            self.next_blink = now + BLINK;
            self.request_redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next_blink));
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
