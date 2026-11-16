//! Application winit : fenêtre, événements clavier/souris/IME/DPI, worker de fichiers, et modes de
//! mesure (`--bench-*`) de la latence et du démarrage.
//!
//! Le thread UI ne fait ni E/S ni calcul lourd : fichiers et boîtes de dialogue passent par le
//! worker ([`crate::worker`]). Le rendu est à la demande : une image n'est dessinée que si un
//! événement l'a demandée (aucune boucle permanente ; seul le clignotement du curseur réveille
//! la boucle deux fois par seconde).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use paulwrite_render::{platform, since_process_start, Colors, SoftRenderer};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Cursor, CursorIcon, Window, WindowId};

use crate::command::{Command, Effect};
use crate::editor::{Editor, Preedit};
use crate::keymap;
use crate::metrics::Samples;
use crate::worker::{self, Done, Job};

const BLINK: Duration = Duration::from_millis(530);

/// Événements envoyés au thread UI par d'autres threads.
#[derive(Debug)]
pub enum UserEvent {
    /// Résultat du worker de fichiers.
    Done(Done),
    /// Frappe simulée (mode `--bench-typing`), horodatée à l'émission.
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
}

/// Options de la ligne de commande.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Fichier à ouvrir.
    pub path: Option<PathBuf>,
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
                "--bench-scroll" => o.bench = Some(BenchKind::Scroll(count("--bench-scroll")?)),
                "--bench-out" => {
                    o.bench_out = Some(PathBuf::from(
                        it.next().ok_or("--bench-out attend un chemin")?,
                    ));
                }
                s if s.starts_with("--") => return Err(format!("option inconnue : {s}")),
                _ => o.path = Some(PathBuf::from(a)),
            }
        }
        Ok(o)
    }
}

/// Texte synthétique pour les mesures (déterministe, 5000 lignes de pseudo-code).
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

/// Ce qu'il reste à faire une fois l'enregistrement terminé.
#[derive(Debug)]
enum AfterSave {
    Quit,
    Open(PathBuf),
}

struct App {
    opts: Options,
    jobs: Sender<Job>,
    proxy: EventLoopProxy<UserEvent>,
    t0: Instant,
    window: Option<Arc<Window>>,
    hwnd: isize,
    renderer: Option<SoftRenderer>,
    editor: Editor,
    mods: ModifiersState,
    /// La prochaine présentation doit copier toute l'image (fenêtre affichée, redimensionnée…).
    full_present: bool,
    // souris
    mouse: PhysicalPosition<f64>,
    dragging: bool,
    wheel_lines: f32,
    // clignotement
    next_blink: Instant,
    // titre, chargement, enregistrement
    title: String,
    loading: Option<(PathBuf, u64, u64)>,
    after_save: Option<AfterSave>,
    // IME
    ime_area: Option<(i32, i32)>,
    // mesures
    input_at: Option<Instant>,
    latency: Samples,
    scroll_frames: Samples,
    t_visible: Duration,
    t_first_present: Option<Duration>,
    /// Le fichier demandé est chargé et la première image qui l'affiche est présentée.
    file_loaded: bool,
    t_file_shown: Option<Duration>,
    bench_started: bool,
}

impl App {
    fn new(opts: Options, jobs: Sender<Job>, proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            opts,
            jobs,
            proxy,
            t0: Instant::now(),
            window: None,
            hwnd: 0,
            renderer: None,
            editor: Editor::new(),
            mods: ModifiersState::empty(),
            full_present: true,
            mouse: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            wheel_lines: 0.0,
            next_blink: Instant::now() + BLINK,
            title: String::new(),
            loading: None,
            after_save: None,
            ime_area: None,
            input_at: None,
            latency: Samples::default(),
            scroll_frames: Samples::default(),
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

    // --- Dessin ------------------------------------------------------------------------------

    /// Dessine dans la DIB (sans copier vers la fenêtre).
    fn draw(&mut self) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        self.editor.build_view();
        let scrolled = self.editor.scrolled_since_draw();
        if let Err(e) = r.draw(&self.editor.view(), scrolled) {
            eprintln!("erreur de rendu : {e}");
        }
        self.editor.mark_drawn();
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let started = Instant::now();
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
        let title = match &self.loading {
            Some((p, done, total)) if *total > 0 => format!(
                "Chargement… {} % — {}",
                done * 100 / total,
                p.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
            Some((p, ..)) => format!(
                "Chargement… — {}",
                p.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
            None => self.editor.title(),
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
        let Some((row, utf16)) = self.editor.caret_anchor() else {
            return;
        };
        let m = r.metrics();
        // Position approchée (chasse fixe) : suffisante pour placer la fenêtre de l'IME.
        let x = (m.pad_x + utf16 as f32 * m.advance) as i32;
        let y = (row as i32 + 1) * m.line_height;
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
            self.editor.set_viewport_rows(rows);
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
                    self.editor.insert_text(&text, now);
                }
            }
            Effect::Save => self.start_save(),
            Effect::SaveAs => {
                self.jobs
                    .send(Job::PickSave {
                        owner: self.hwnd,
                        suggested: self.editor.display_name(),
                    })
                    .ok();
            }
            Effect::Open => {
                self.jobs.send(Job::PickOpen { owner: self.hwnd }).ok();
            }
            Effect::Quit => self.request_quit(el),
        }
    }

    fn execute(&mut self, cmd: Command, el: &ActiveEventLoop) {
        let now = self.now_ms();
        if let Some(effect) = self.editor.execute(cmd, now) {
            self.run_effect(effect, el);
        }
    }

    fn start_save(&mut self) {
        match self.editor.path() {
            None => {
                self.jobs
                    .send(Job::PickSave {
                        owner: self.hwnd,
                        suggested: self.editor.display_name(),
                    })
                    .ok();
            }
            Some(path) => {
                let doc = self.editor.document();
                self.jobs
                    .send(Job::Save {
                        path: path.to_path_buf(),
                        rope: doc.buffer().rope().clone(),
                        encoding: doc.encoding(),
                        revision: doc.buffer().revision(),
                    })
                    .ok();
            }
        }
    }

    fn request_quit(&mut self, el: &ActiveEventLoop) {
        if self.editor.is_modified() && !self.is_bench() {
            match platform::confirm_save(self.hwnd, &self.editor.display_name()) {
                platform::SaveChoice::Save => {
                    self.after_save = Some(AfterSave::Quit);
                    self.start_save();
                }
                platform::SaveChoice::Discard => el.exit(),
                platform::SaveChoice::Cancel => {}
            }
        } else {
            el.exit();
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        if self.editor.is_modified() {
            match platform::confirm_save(self.hwnd, &self.editor.display_name()) {
                platform::SaveChoice::Save => {
                    self.after_save = Some(AfterSave::Open(path));
                    self.start_save();
                    return;
                }
                platform::SaveChoice::Discard => {}
                platform::SaveChoice::Cancel => return,
            }
        }
        self.loading = Some((path.clone(), 0, 0));
        self.jobs.send(Job::Open(path)).ok();
        self.refresh_title();
    }

    /// Place le curseur sous la souris (clic ou glissement).
    fn mouse_to_caret(&mut self, extend: bool) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        let m = r.metrics();
        self.editor.build_view();
        let row = ((self.mouse.y.max(0.0) as i32) / m.line_height.max(1)) as usize;
        let row = row.min(self.editor.rows().saturating_sub(1));
        let utf16 = r
            .column_at(self.editor.row_text(row), self.mouse.x as f32)
            .unwrap_or(0);
        let col = self.editor.char_col_of_utf16(row, utf16);
        self.editor.click(row, col, extend);
    }

    // --- Mesures -----------------------------------------------------------------------------

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
                    self.bench_started = true;
                    // Caret au milieu du texte, dans une ligne visible.
                    self.editor.scroll_by(100);
                    self.editor.click(10, 0, false);
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
                    self.request_redraw();
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
                    let max = self.editor.document().buffer().len_lines();
                    let rows = self.editor.rows();
                    if self.editor.first_line() + 3 + rows >= max {
                        self.editor.scroll_by(-(self.editor.first_line() as isize));
                    } else {
                        self.editor.scroll_by(3);
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
                "  fichier affiché : {:.0} ms depuis la création du processus
",
                ms(t)
            ));
        }
        if let Some(s) = self.latency.summary() {
            report.push_str(&format!(
                "  frappe, événement -> image copiée vers la fenêtre : {s}\n"
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
            Some(BenchKind::Typing(_) | BenchKind::Scroll(_) | BenchKind::Open)
        );
        let bench_synthetic = matches!(
            self.opts.bench,
            Some(BenchKind::Typing(_) | BenchKind::Scroll(_))
        );
        let mut attrs = Window::default_attributes()
            .with_title(self.editor.title())
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
        let rows = renderer.metrics().rows_for(size.height);
        self.renderer = Some(renderer);
        self.window = Some(window.clone());

        if bench_synthetic {
            self.editor.load(
                paulwrite_core::Document::from_text(&synthetic_text(5000)),
                None,
            );
        }
        self.editor.set_viewport_rows(rows);
        window.set_ime_allowed(true);
        window.set_cursor(Cursor::Icon(CursorIcon::Text));
        if let Some(path) = self.opts.path.clone() {
            self.loading = Some((path.clone(), 0, 0));
            self.jobs.send(Job::Open(path)).ok();
        }

        // Première image dessinée avant l'affichage : la fenêtre apparaît avec son contenu.
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
                self.editor.insert_text("x", now);
                self.input_at.get_or_insert(at);
                self.request_redraw();
            }
            UserEvent::BenchDone => self.finish_bench(el),
            UserEvent::Done(done) => match done {
                Done::Progress { path, done, total } => {
                    self.loading = Some((path, done, total));
                    self.refresh_title();
                }
                Done::Loaded { path, result } => {
                    self.loading = None;
                    match result {
                        Ok(doc) => {
                            self.editor.load(doc, Some(path));
                            self.file_loaded = true;
                            if let Some(r) = self.renderer.as_mut() {
                                r.invalidate();
                            }
                            self.full_present = true;
                        }
                        Err(e) => {
                            platform::show_error(
                                self.hwnd,
                                &format!("Impossible d'ouvrir « {} » :\n{e}", path.display()),
                            );
                        }
                    }
                    self.request_redraw();
                }
                Done::Saved {
                    path,
                    revision,
                    result,
                } => match result {
                    Ok(()) => {
                        if self.editor.path() == Some(path.as_path()) {
                            self.editor.mark_saved(revision);
                        }
                        match self.after_save.take() {
                            Some(AfterSave::Quit) => el.exit(),
                            Some(AfterSave::Open(p)) => self.open_path(p),
                            None => {}
                        }
                        self.request_redraw();
                    }
                    Err(e) => {
                        self.after_save = None;
                        platform::show_error(
                            self.hwnd,
                            &format!("Échec de l'enregistrement de « {} » :\n{e}", path.display()),
                        );
                    }
                },
                Done::OpenPicked(Some(path)) => self.open_path(path),
                Done::SavePicked(Some(path)) => {
                    self.editor.set_path(path);
                    self.start_save();
                    self.request_redraw();
                }
                Done::OpenPicked(None) => {}
                Done::SavePicked(None) => self.after_save = None,
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
                            self.editor.insert_text(text, now);
                        }
                    }
                }
                self.touch(at);
            }
            WindowEvent::Ime(ime) => {
                let at = Instant::now();
                match ime {
                    Ime::Commit(text) => {
                        self.editor.set_preedit(None);
                        let now = self.now_ms();
                        self.editor.insert_text(&text, now);
                    }
                    Ime::Preedit(text, cursor) => {
                        self.editor.set_preedit(Some(Preedit {
                            text,
                            cursor: cursor.map(|(start, _)| start),
                        }));
                    }
                    Ime::Enabled | Ime::Disabled => self.editor.set_preedit(None),
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
                    self.editor.scroll_by(whole as isize);
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
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    self.dragging = true;
                    self.mouse_to_caret(self.mods.shift_key());
                    self.touch(Instant::now());
                }
                ElementState::Released => self.dragging = false,
            },
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
            let on = !self.editor.caret_visible();
            self.editor.set_caret_visible(on);
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
    let mut app = App::new(opts, jobs, proxy);
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
        let o = parse(&["notes.txt"]).unwrap();
        assert_eq!(o.path, Some(PathBuf::from("notes.txt")));
        assert_eq!(o.bench, None);
        let o = parse(&["--bench-typing", "300", "--bench-out", "r.txt"]).unwrap();
        assert_eq!(o.bench, Some(BenchKind::Typing(300)));
        assert_eq!(o.bench_out, Some(PathBuf::from("r.txt")));
        assert_eq!(
            parse(&["--bench-startup"]).unwrap().bench,
            Some(BenchKind::Startup)
        );
        assert!(parse(&["--bench-typing"]).is_err());
        assert!(parse(&["--bench-typing", "x"]).is_err());
        assert!(parse(&["--inconnue"]).is_err());
    }

    #[test]
    fn le_texte_synthetique_est_deterministe() {
        let a = synthetic_text(50);
        assert_eq!(a, synthetic_text(50));
        assert_eq!(a.lines().count(), 50);
    }
}
