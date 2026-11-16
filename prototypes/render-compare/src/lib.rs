//! Scénario commun aux deux prototypes de rendu.
//!
//! 1. Démarrage : fenêtre → moteur de rendu prêt → première image présentée.
//! 2. Défilement : 600 images d'affilée, 3 lignes par image, dans un texte de 5000 lignes.
//! 3. Frappe : 300 frappes simulées par un autre thread (une toutes les ~8 ms), mesure du temps
//!    entre l'arrivée de l'événement et la fin du rendu + présentation de l'image.
//!
//! Variable d'environnement `VSYNC=1` : présente avec synchronisation verticale (sinon sans).

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

pub const LINES: usize = 5000;
pub const SCROLL_FRAMES: usize = 600;
pub const SCROLL_STEP: usize = 3;
pub const TYPING_EVENTS: usize = 300;
pub const FONT_PX: f32 = 15.0;

pub const BG: [f32; 4] = [0.11, 0.12, 0.14, 1.0];
pub const COLORS: [[f32; 4]; 4] = [
    [0.85, 0.86, 0.88, 1.0], // texte
    [0.40, 0.65, 1.00, 1.0], // bleu
    [1.00, 0.65, 0.35, 1.0], // orange
    [0.95, 0.95, 0.95, 1.0], // curseur
];

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    lo: u32,
    hi: u32,
}

#[allow(unsafe_code)]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn GetProcessTimes(
        h: isize,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetSystemTimePreciseAsFileTime(out: *mut FileTime);
}

/// Temps écoulé depuis la *création du processus* (inclut le chargeur Windows et l'init du CRT,
/// avant `main`). Précision ~ 1 ms.
#[must_use]
pub fn since_process_start() -> Duration {
    let (mut c, mut e, mut k, mut u) = (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
    let mut now = FileTime::default();
    // SAFETY: appels Win32 sans effet de bord, pointeurs vers des variables locales valides.
    unsafe {
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        GetSystemTimePreciseAsFileTime(&mut now);
    }
    let to_u64 = |f: FileTime| (u64::from(f.hi) << 32) | u64::from(f.lo);
    Duration::from_nanos(to_u64(now).saturating_sub(to_u64(c)) * 100)
}

/// Une ligne visible : indice dans le document, révision (pour les caches), texte.
pub struct VisibleLine<'a> {
    pub index: usize,
    pub rev: u32,
    pub text: &'a str,
}

/// Ce que le moteur de rendu doit dessiner.
pub struct Frame<'a> {
    pub lines: Vec<VisibleLine<'a>>,
    /// (ligne visible, colonne) du curseur.
    pub cursor: Option<(usize, usize)>,
    pub width: u32,
    pub height: u32,
}

pub trait Renderer {
    fn render(&mut self, frame: &Frame<'_>, vsync: bool);
    fn resize(&mut self, width: u32, height: u32);
    /// Hauteur d'une ligne en pixels physiques.
    fn line_height(&self) -> f32;
}

/// Trois plages colorées (octets, texte ASCII) : le moteur dessine donc plusieurs séquences par ligne.
#[must_use]
pub fn color_runs(text: &str) -> [(usize, usize, usize); 3] {
    let n = text.len();
    let (a, b) = (n / 3, 2 * n / 3);
    [(0, a, 0), (a, b, 1), (b, n, 2)]
}

fn generate_lines() -> Vec<String> {
    let words = [
        "let", "fn", "value", "buffer", "cursor", "select", "compute", "render", "line", "struct",
        "impl", "return", "match", "undo", "redo", "paul", "write", "{", "}", "(", ")", ";", "=", "+",
        "//", "self", "mut", "pub",
    ];
    let mut s = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    (0..LINES)
        .map(|_| {
            let mut line = " ".repeat((next() % 4) as usize * 4);
            let target = 30 + (next() % 80) as usize;
            while line.len() < target {
                line.push_str(words[(next() % words.len() as u64) as usize]);
                line.push(' ');
            }
            line
        })
        .collect()
}

#[derive(Debug)]
pub enum UserEvent {
    Key(Instant),
}

#[derive(PartialEq, Eq)]
enum Phase {
    First,
    Scroll,
    Typing,
}

struct App<R: Renderer, F: Fn(Arc<Window>) -> R> {
    name: &'static str,
    make: F,
    t0: Instant,
    vsync: bool,
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<R>,
    lines: Vec<String>,
    revs: Vec<u32>,
    first_line: usize,
    phase: Phase,
    size: (u32, u32),
    // résultats
    t_window: Duration,
    t_renderer: Duration,
    t_first_frame: Duration,
    /// Délai entre la création du processus et le début de `run` (chargeur, CRT).
    t_pre_main: Duration,
    /// Première image, mesurée depuis la création du processus.
    t_first_frame_process: Duration,
    /// Fenêtre visible *et* première image déjà dessinée, depuis la création du processus.
    t_visible_process: Duration,
    /// `HIDDEN=1` : fenêtre créée masquée, première image dessinée, puis affichée.
    hidden: bool,
    scroll_us: Vec<f64>,
    typing_latency_us: Vec<f64>,
    typing_render_us: Vec<f64>,
    // frappe
    typed: usize,
    cursor_row: usize,
    cursor_col: usize,
    pending: Option<Instant>,
}

impl<R: Renderer, F: Fn(Arc<Window>) -> R> App<R, F> {
    fn redraw(&mut self, el: &ActiveEventLoop) {
        let (Some(window), Some(renderer)) = (self.window.clone(), self.renderer.as_mut()) else {
            return;
        };
        let (w, h) = self.size;
        let lh = renderer.line_height().max(1.0);
        let visible = (h as f32 / lh).ceil() as usize + 1;

        if self.phase == Phase::Scroll {
            self.first_line = (self.first_line + SCROLL_STEP) % (LINES - visible);
        }
        let first = self.first_line;
        let lines: Vec<VisibleLine<'_>> = (first..(first + visible).min(LINES))
            .map(|i| VisibleLine { index: i, rev: self.revs[i], text: &self.lines[i] })
            .collect();
        let cursor = (self.phase == Phase::Typing).then_some((self.cursor_row, self.cursor_col));
        let frame = Frame { lines, cursor, width: w, height: h };

        let t_start = Instant::now();
        renderer.render(&frame, self.vsync);
        let t_end = Instant::now();
        let render_us = (t_end - t_start).as_secs_f64() * 1e6;

        match self.phase {
            Phase::First => {
                self.t_first_frame = self.t0.elapsed();
                self.t_first_frame_process = since_process_start();
                if !self.hidden && std::env::var("EXIT_AFTER_FIRST").is_ok_and(|v| v == "1") {
                    // Mesure de démarrage seule : la fenêtre ne reste qu'un instant à l'écran.
                    self.report();
                    el.exit();
                    return;
                }
                self.phase = Phase::Scroll;
                window.request_redraw();
            }
            Phase::Scroll => {
                self.scroll_us.push(render_us);
                if self.scroll_us.len() >= SCROLL_FRAMES {
                    self.phase = Phase::Typing;
                    self.first_line = 100;
                    self.cursor_row = 10;
                    window.request_redraw();
                    let proxy = self.proxy.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(300));
                        for _ in 0..TYPING_EVENTS {
                            std::thread::sleep(Duration::from_millis(8));
                            if proxy.send_event(UserEvent::Key(Instant::now())).is_err() {
                                break;
                            }
                        }
                    });
                } else {
                    window.request_redraw();
                }
            }
            Phase::Typing => {
                if let Some(t) = self.pending.take() {
                    self.typing_latency_us.push((t_end - t).as_secs_f64() * 1e6);
                    self.typing_render_us.push(render_us);
                }
                if self.typed >= TYPING_EVENTS && self.pending.is_none() {
                    self.report();
                    el.exit();
                }
            }
        }
    }

    fn report(&self) {
        let stats = |v: &[f64]| -> String {
            if v.is_empty() {
                return "n/a".into();
            }
            let mut s = v.to_vec();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let q = |p: f64| s[((s.len() as f64 - 1.0) * p).round() as usize] / 1000.0;
            let mean = s.iter().sum::<f64>() / s.len() as f64 / 1000.0;
            format!(
                "moy {mean:.2} ms | p50 {:.2} | p95 {:.2} | p99 {:.2} | max {:.2}",
                q(0.5),
                q(0.95),
                q(0.99),
                q(1.0)
            )
        };
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!("=== {} (vsync={}) ===", self.name, self.vsync);
        println!(
            "démarrage : avant main {:.1} ms | fenêtre {:.1} ms | moteur prêt {:.1} ms | 1re image {:.1} ms (depuis main)",
            ms(self.t_pre_main),
            ms(self.t_window),
            ms(self.t_renderer),
            ms(self.t_first_frame)
        );
        println!("DÉMARRAGE À FROID (création du processus -> 1re image présentée) : {:.1} ms", ms(self.t_first_frame_process));
        println!("FENÊTRE VISIBLE avec 1re image (hidden={}) : {:.1} ms", self.hidden, ms(self.t_visible_process));
        println!("défilement ({} images, rendu+présentation) : {}", self.scroll_us.len(), stats(&self.scroll_us));
        println!("frappe, rendu seul ({} mesures)          : {}", self.typing_render_us.len(), stats(&self.typing_render_us));
        println!("frappe, événement -> image présentée     : {}", stats(&self.typing_latency_us));
    }
}

impl<R: Renderer, F: Fn(Arc<Window>) -> R> ApplicationHandler<UserEvent> for App<R, F> {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        use winit::platform::windows::WindowAttributesExtWindows;
        let trace = std::env::var("TRACE").is_ok();
        if trace {
            eprintln!("  [win] resumed atteint à {:.1} ms (après build de la boucle)", self.t0.elapsed().as_secs_f64() * 1000.0);
        }
        let mut attrs = Window::default_attributes()
            .with_title(format!("render-compare : {}", self.name))
            .with_inner_size(LogicalSize::new(1280.0, 800.0));
        // SIZE=3840x2160 : taille intérieure en pixels physiques (test grand écran).
        if let Some((sw, sh)) = std::env::var("SIZE").ok().and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))
        }) {
            attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(sw, sh));
        }
        if std::env::var("NO_DND").is_ok() {
            attrs = attrs.with_drag_and_drop(false);
        }
        if self.hidden {
            attrs = attrs.with_visible(false);
        }
        // QUIET=1 : fenêtre de test sans prise de focus et hors de la zone visible de l'écran
        // (n'empêche pas la mesure : la fenêtre est quand même créée, affichée et peinte).
        // NOFOCUS=1 : ne vole pas le focus. QUIET=1 : en plus, hors écran. ATTENTION : hors écran,
        // DWM ne compose pas la fenêtre et le « démarrage » devient irréaliste (~80 ms au lieu de
        // ~210 ms) ; QUIET sert donc seulement aux mesures de défilement/frappe.
        if std::env::var("NOFOCUS").is_ok_and(|v| v == "1") {
            attrs = attrs.with_active(false);
        }
        if std::env::var("QUIET").is_ok_and(|v| v == "1") {
            use winit::dpi::PhysicalPosition;
            attrs = attrs.with_active(false).with_position(PhysicalPosition::new(-6000, 0));
        }
        let t_create = Instant::now();
        let window = Arc::new(el.create_window(attrs).expect("fenêtre"));
        if trace {
            eprintln!("  [win] create_window : {:.1} ms", t_create.elapsed().as_secs_f64() * 1000.0);
        }
        self.t_window = self.t0.elapsed();
        let size = window.inner_size();
        self.size = (size.width, size.height);
        self.renderer = Some((self.make)(window.clone()));
        self.t_renderer = self.t0.elapsed();
        self.window = Some(window.clone());
        if self.hidden {
            // Première image dessinée hors écran, puis fenêtre affichée.
            self.redraw(el);
            let t_show = Instant::now();
            window.set_visible(true);
            if trace {
                eprintln!("  [win] set_visible(true) : {:.1} ms", t_show.elapsed().as_secs_f64() * 1000.0);
            }
            self.t_visible_process = since_process_start();
            if std::env::var("EXIT_AFTER_FIRST").is_ok_and(|v| v == "1") {
                self.report();
                el.exit();
            }
        } else {
            self.t_visible_process = since_process_start();
            window.request_redraw();
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: UserEvent) {
        let UserEvent::Key(at) = ev;
        if self.phase != Phase::Typing {
            return;
        }
        let row = self.cursor_row;
        let idx = self.first_line + row;
        let col = self.cursor_col.min(self.lines[idx].len());
        self.lines[idx].insert(col, 'x');
        self.revs[idx] += 1;
        self.cursor_col = col + 1;
        self.typed += 1;
        if self.typed % 60 == 0 {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.pending.get_or_insert(at);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => {
                self.size = (s.width.max(1), s.height.max(1));
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(self.size.0, self.size.1);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(el),
            _ => {}
        }
    }
}

/// Lance le scénario avec un moteur de rendu fabriqué par `make` (appelé après création de la fenêtre).
pub fn run<R: Renderer + 'static>(name: &'static str, make: impl Fn(Arc<Window>) -> R + 'static) {
    let t0 = Instant::now();
    let t_pre_main = since_process_start();
    let vsync = std::env::var("VSYNC").is_ok_and(|v| v == "1");
    let t_build = Instant::now();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("event loop");
    if std::env::var("TRACE").is_ok() {
        eprintln!(
            "  [win] pré-main {:.1} ms | EventLoop::build {:.1} ms",
            t_pre_main.as_secs_f64() * 1000.0,
            t_build.elapsed().as_secs_f64() * 1000.0
        );
    }
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App {
        name,
        make,
        t0,
        vsync,
        proxy,
        window: None,
        renderer: None,
        lines: generate_lines(),
        revs: vec![0; LINES],
        first_line: 0,
        phase: Phase::First,
        size: (1280, 800),
        t_window: Duration::ZERO,
        t_renderer: Duration::ZERO,
        t_first_frame: Duration::ZERO,
        t_pre_main,
        t_first_frame_process: Duration::ZERO,
        t_visible_process: Duration::ZERO,
        hidden: std::env::var("HIDDEN").is_ok_and(|v| v == "1"),
        scroll_us: Vec::new(),
        typing_latency_us: Vec::new(),
        typing_render_us: Vec::new(),
        typed: 0,
        cursor_row: 10,
        cursor_col: 0,
        pending: None,
    };
    event_loop.run_app(&mut app).expect("boucle d'événements");
}
