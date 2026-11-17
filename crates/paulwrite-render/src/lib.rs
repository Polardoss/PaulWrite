//! Rendu du texte : Direct2D logiciel + DirectWrite dans une DIB mémoire, avec régions sales.
//!
//! Choix et mesures : `docs/rendering.md`. Principes :
//! - aucune initialisation de pilote GPU (démarrage < 100 ms) ;
//! - seules les rangées dont la signature change sont redessinées ; un défilement déplace les
//!   pixels déjà dessinés ;
//! - le rendu va dans une DIB ; la copie vers la fenêtre ([`SoftRenderer::present`]) est séparée,
//!   ce qui rend le rendu testable sans fenêtre.
//!
//! # `unsafe`
//! Ce crate est une couche FFI Win32 (Direct2D, DirectWrite, GDI) : `unsafe_code` y est autorisé,
//! chaque bloc `unsafe` est petit et justifié.

#![allow(unsafe_code)]

pub mod platform;
mod png;
mod soft;

pub use png::write_png;

pub use soft::{
    Band, Chrome, Colors, FoldMark, Metrics, MinimapSource, Overlay, OverlayRow, OverlayRowKind,
    Row, RowSource, Scroll, Sidebar, SidebarKind, SidebarRow, SoftRenderer, StripHit, TabButton,
    TabInfo, KIND_SLOTS,
};

use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    lo: u32,
    hi: u32,
}

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

/// Temps écoulé depuis la **création du processus** (inclut le chargeur Windows, avant `main`).
/// Précision ≈ 1 ms. Sert à mesurer le démarrage à froid.
#[must_use]
pub fn since_process_start() -> Duration {
    let mut c = FileTime::default();
    let (mut e, mut k, mut u) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    let mut now = FileTime::default();
    // SAFETY: appels Win32 sans effet de bord ; les pointeurs visent des variables locales valides.
    unsafe {
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        GetSystemTimePreciseAsFileTime(&mut now);
    }
    let to_u64 = |f: FileTime| (u64::from(f.hi) << 32) | u64::from(f.lo);
    Duration::from_nanos(to_u64(now).saturating_sub(to_u64(c)) * 100)
}
