//! Mesure de la coloration : analyse complète, puis frappes au milieu du document (analyse
//! incrémentale, requête de coloration sur la fenêtre visible).
//!
//! Usage : `cargo run --release -p paulwrite-syntax --example bench -- [taille en Ko] [frappes]`

use std::time::Instant;

use paulwrite_core::Buffer;
use paulwrite_syntax::{Highlighter, Language};

/// Code Rust valide, ~50 octets par ligne, jusqu'à `bytes` octets.
fn synthetic_rust(bytes: usize) -> String {
    let mut out = String::from("use std::collections::HashMap;\n\n");
    let mut i = 0usize;
    while out.len() < bytes {
        out.push_str(&format!(
            "/// Fonction numéro {i}.\nfn compute_{i}(a: i32, b: &str) -> Option<usize> {{\n    let mut map: HashMap<String, usize> = HashMap::new();\n    // calcul {i}\n    if a > {n} {{\n        map.insert(format!(\"clé-{{}}\", b), {n});\n    }}\n    map.get(b).copied()\n}}\n\n",
            n = i * 7 % 1000
        ));
        i += 1;
    }
    out
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let mut args = std::env::args().skip(1);
    let kb: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1024);
    let edits: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(300);

    let text = synthetic_rust(kb * 1024);
    let mut buf = Buffer::from_text(&text);
    buf.track_edits(true);
    println!(
        "texte : {:.2} Mo, {} lignes",
        text.len() as f64 / 1_048_576.0,
        buf.len_lines()
    );

    let t = Instant::now();
    let mut h = Highlighter::new(Language::Rust).expect("grammaire");
    println!(
        "création (compilation de la requête) : {:.1} ms",
        ms(t.elapsed())
    );

    let t = Instant::now();
    h.update(buf.rope(), &[]);
    println!("analyse complète : {:.1} ms", ms(t.elapsed()));

    // fenêtre de 230 lignes autour du milieu
    let mid_line = buf.len_lines() / 2;
    let window = |buf: &Buffer| {
        let rope = buf.rope();
        rope.line_to_byte(mid_line - 100)..rope.line_to_byte((mid_line + 130).min(rope.len_lines()))
    };
    let t = Instant::now();
    let spans = h.highlights(buf.rope(), window(&buf));
    println!(
        "coloration d'une fenêtre de 230 lignes : {:.2} ms ({} plages)",
        ms(t.elapsed()),
        spans.len()
    );

    let mut update = Vec::with_capacity(edits);
    let mut query = Vec::with_capacity(edits);
    let at0 = buf.line_start(mid_line);
    for i in 0..edits {
        buf.insert(at0 + i, "x");
        let e = buf.take_edits();
        let t = Instant::now();
        h.update(buf.rope(), &e);
        let tu = t.elapsed();
        let t = Instant::now();
        let _ = h.highlights(buf.rope(), window(&buf));
        update.push(tu);
        query.push(t.elapsed());
    }
    let stat = |v: &mut Vec<std::time::Duration>| {
        v.sort();
        let q = |p: f64| ms(v[((v.len() - 1) as f64 * p).round() as usize]);
        format!(
            "p50 {:.2} ms | p95 {:.2} | p99 {:.2} | max {:.2}",
            q(0.5),
            q(0.95),
            q(0.99),
            q(1.0)
        )
    };
    println!("{edits} frappes au milieu du document :");
    println!("  analyse incrémentale : {}", stat(&mut update));
    println!("  requête de coloration : {}", stat(&mut query));
}
