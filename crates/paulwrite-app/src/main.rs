//! Binaire `paulwrite`.
//!
//! Usage : `paulwrite [fichier...]`. Options de mesure (`--bench-startup`, `--bench-open`,
//! `--bench-typing N`, `--bench-scroll N`, `--bench-syntax N`, `--bench-out FICHIER`) : voir
//! `docs/benchmarks.md`.

// Pas de console en release : une application graphique ne doit pas ouvrir de terminal.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use std::path::PathBuf;

use paulwrite_app::app::{self, Options};

/// Journal des plantages : `%LOCALAPPDATA%\PaulWrite\crash.log`. Le binaire de production
/// s'arrête au premier `panic` et n'a pas de console : sans ce fichier, un plantage ne laisserait
/// aucune trace.
fn crash_log_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("PaulWrite").join("crash.log"))
}

fn install_crash_log() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("?");
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let report = format!(
            "[{secs} s depuis 1970] paulwrite {} : panique dans le thread « {name} »\n{info}\n{}\n\n",
            env!("CARGO_PKG_VERSION"),
            std::backtrace::Backtrace::force_capture()
        );
        eprint!("{report}");
        if let Some(path) = crash_log_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = f.write_all(report.as_bytes());
            }
        }
    }));
}

fn main() {
    install_crash_log();
    let opts = match Options::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("paulwrite : {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = app::run(opts) {
        eprintln!("paulwrite : {e}");
        std::process::exit(1);
    }
}
