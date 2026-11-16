//! Génère un faux projet de N fichiers (100 000 par défaut) pour les mesures de recherche.
//!
//! Usage : `cargo run --release -p paulwrite-search --example gen_project -- <dossier> [N]`
//!
//! Contenu déterministe. Structure : `mod_XXX/sub_YY/file_ZZZZ.ext`, 20 sous-dossiers par module,
//! 50 fichiers par sous-dossier. Un fichier sur 1000 contient `needle_xyz` (motif rare), un sur 20
//! une fonction `*_handler` (motif courant). Un `.gitignore` exclut `target/` et `node_modules/`,
//! qui contiennent chacun des fichiers avec `needle_xyz` : une recherche correcte ne doit pas les
//! voir.

use std::fs;
use std::path::{Path, PathBuf};

const WORDS: [&str; 24] = [
    "let", "fn", "value", "buffer", "cursor", "select", "compute", "render", "line", "struct",
    "impl", "return", "match", "undo", "redo", "paul", "write", "self", "mut", "pub", "const",
    "static", "enum", "trait",
];

fn content(index: usize) -> String {
    let mut s = 0x9E37_79B9_7F4A_7C15_u64 ^ (index as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut out = String::with_capacity(1400);
    for line in 0..30 {
        if line == 7 && index % 1000 == 0 {
            out.push_str("    // marqueur needle_xyz\n");
        }
        if line == 12 && index % 20 == 0 {
            out.push_str(&format!(
                "fn file_{index}_handler(x: i32) -> i32 {{ x + 1 }}\n"
            ));
        }
        out.push_str(&" ".repeat((next() % 3) as usize * 4));
        for _ in 0..(4 + next() % 6) {
            out.push_str(WORDS[(next() % WORDS.len() as u64) as usize]);
            out.push(' ');
        }
        out.push('\n');
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage : gen_project <dossier> [N]"));
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100_000);
    let marker = root.join(".generated");
    if fs::read_to_string(&marker).is_ok_and(|m| m.trim() == n.to_string()) {
        println!("déjà généré : {} fichiers dans {}", n, root.display());
        return;
    }
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".gitignore"), "target/\nnode_modules/\n").unwrap();

    let exts = ["rs", "js", "txt", "py", "md"];
    let threads = 8;
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        for t in 0..threads {
            let root = &root;
            scope.spawn(move || {
                for i in (t..n).step_by(threads) {
                    let (module, sub, file) = (i / 1000, (i / 50) % 20, i % 50);
                    let dir = root
                        .join(format!("mod_{module:03}"))
                        .join(format!("sub_{sub:02}"));
                    fs::create_dir_all(&dir).unwrap();
                    let ext = exts[i % exts.len()];
                    fs::write(dir.join(format!("file_{file:04}.{ext}")), content(i)).unwrap();
                }
            });
        }
    });
    // dossiers ignorés : doivent rester invisibles
    for dir in ["target/debug", "node_modules/pkg"] {
        let d: &Path = &root.join(dir);
        fs::create_dir_all(d).unwrap();
        for i in 0..1500 {
            fs::write(d.join(format!("ignored_{i}.js")), "needle_xyz\n").unwrap();
        }
    }
    fs::write(&marker, n.to_string()).unwrap();
    println!(
        "{} fichiers (+ 3000 ignorés) générés dans {} en {:.1} s",
        n,
        root.display(),
        started.elapsed().as_secs_f64()
    );
}
