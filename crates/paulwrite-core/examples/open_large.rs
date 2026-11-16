//! Charge un fichier et affiche le temps d'ouverture.
//!
//! Sert à mesurer le pic mémoire depuis l'extérieur (le processus reste vivant 4 s après le
//! chargement pour qu'un outil puisse lire `PeakWorkingSet64`) :
//!
//! ```text
//! cargo run --release -p paulwrite-core --example open_large -- bench-data/text-500mb.txt [mmap]
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use paulwrite_core::io::{load_file, LoadOptions};

fn main() {
    let mut args = std::env::args().skip(1);
    let path: PathBuf = args
        .next()
        .expect("usage: open_large <fichier> [mmap]")
        .into();
    let mut opts = LoadOptions::default();
    if args.next().as_deref() == Some("mmap") {
        opts.mmap_threshold = 0;
    }
    let start = Instant::now();
    let loaded = load_file(&path, &opts, None).expect("chargement");
    let elapsed = start.elapsed();
    println!(
        "ouvert en {:.0} ms : {} caractères, {} octets, {} lignes, {:?}, {:?}",
        elapsed.as_secs_f64() * 1000.0,
        loaded.rope.len_chars(),
        loaded.rope.len_bytes(),
        loaded.rope.len_lines(),
        loaded.encoding,
        loaded.line_ending,
    );
    std::thread::sleep(Duration::from_secs(4));
    drop(loaded);
}
