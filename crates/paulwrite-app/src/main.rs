//! Binaire `paulwrite`.
//!
//! Usage : `paulwrite [fichier]`. Options de mesure (`--bench-startup`, `--bench-typing N`,
//! `--bench-scroll N`, `--bench-out FICHIER`) : voir `docs/benchmarks.md`.

// Pas de console en release : une application graphique ne doit pas ouvrir de terminal.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use paulwrite_app::app::{self, Options};

fn main() {
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
