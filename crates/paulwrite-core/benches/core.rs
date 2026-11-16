//! Benchmarks de `paulwrite-core` (criterion).
//!
//! Les fichiers de test sont générés de façon déterministe dans `<workspace>/bench-data/` à la
//! première exécution (ignorés par git). Taille du gros fichier : 500 Mo.
//!
//! Lancer : `cargo bench -p paulwrite-core` (ou `-- <filtre>` pour un seul scénario).

use std::fs::{self, File};
use std::hint::black_box;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use paulwrite_core::io::{load_file, LoadOptions};
use paulwrite_core::{Buffer, Document, Selection, SelectionSet};

const BIG_BYTES: u64 = 500 * 1024 * 1024;

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench-data")
}

/// Petit générateur pseudo-aléatoire déterministe (xorshift64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Génère (une seule fois) un fichier de code factice d'environ `bytes` octets : lignes de 20 à
/// 120 caractères, indentation, quelques caractères multi-octets.
fn ensure_big_file(bytes: u64) -> PathBuf {
    let dir = data_dir();
    fs::create_dir_all(&dir).expect("create bench-data");
    let path = dir.join(format!("text-{}mb.txt", bytes / (1024 * 1024)));
    if fs::metadata(&path)
        .map(|m| m.len() >= bytes)
        .unwrap_or(false)
    {
        return path;
    }
    eprintln!(
        "génération de {} ({} Mo)...",
        path.display(),
        bytes / (1024 * 1024)
    );
    let mut w = BufWriter::with_capacity(4 << 20, File::create(&path).expect("create file"));
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let words = [
        "let", "fn", "value", "buffer", "cursor", "select", "é", "€", "😀", "compute", "render",
        "line", "struct", "impl", "return", "match", "undo", "redo", "paul", "write", "{", "}",
        "(", ")", ";", "=", "+", "//",
    ];
    let mut written = 0u64;
    let mut line = String::new();
    while written < bytes {
        line.clear();
        for _ in 0..(rng.next() % 4) {
            line.push_str("    ");
        }
        let target = 20 + (rng.next() % 100) as usize;
        while line.len() < target {
            line.push_str(words[(rng.next() % words.len() as u64) as usize]);
            line.push(' ');
        }
        line.push('\n');
        w.write_all(line.as_bytes()).expect("write");
        written += line.len() as u64;
    }
    w.flush().expect("flush");
    path
}

fn load_big() -> Document {
    let path = ensure_big_file(BIG_BYTES);
    let loaded = load_file(&path, &LoadOptions::default(), None).expect("load big file");
    Document::from_buffer(
        Buffer::from_rope(loaded.rope),
        loaded.encoding,
        loaded.line_ending,
    )
}

fn carets_spread(doc: &Document, n: usize) -> SelectionSet {
    let len = doc.buffer().len_chars();
    let step = len / n;
    SelectionSet::from_vec((0..n).map(|i| Selection::caret(i * step)).collect(), 0)
}

fn medium_doc() -> Document {
    let text: String = (0..1000)
        .map(|i| format!("let value_{i} = compute({i});\n"))
        .collect();
    Document::from_text(&text)
}

// --- 1. Ouverture ----------------------------------------------------------------------------

fn bench_open(c: &mut Criterion) {
    let path = ensure_big_file(BIG_BYTES);
    let size = fs::metadata(&path).unwrap().len();
    let mut g = c.benchmark_group("open");
    g.sample_size(10)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(30));
    g.throughput(Throughput::Bytes(size));
    // Fichier en cache système : mesure le décodage + la construction du rope, pas le disque.
    g.bench_function("500MB_mmap_warm_cache", |b| {
        b.iter(|| {
            let loaded = load_file(&path, &LoadOptions::default(), None).unwrap();
            black_box(loaded.rope.len_chars())
        });
    });
    g.bench_function("500MB_stream_warm_cache", |b| {
        let opts = LoadOptions {
            mmap_threshold: u64::MAX,
            ..LoadOptions::default()
        };
        b.iter(|| {
            let loaded = load_file(&path, &opts, None).unwrap();
            black_box(loaded.rope.len_chars())
        });
    });
    g.finish();
}

// --- 2. Insertion dans un gros fichier ---------------------------------------------------------

fn bench_insert_big(c: &mut Criterion) {
    let mut doc = load_big();
    let mid = doc.buffer().len_chars() / 2;
    let mut buf = doc.buffer().clone();
    let mut g = c.benchmark_group("insert_500MB");
    g.bench_function("buffer_insert_middle", |b| {
        b.iter(|| buf.insert(black_box(mid), black_box("a")));
    });
    g.bench_function("buffer_insert_then_delete_middle", |b| {
        b.iter(|| {
            buf.insert(black_box(mid), "a");
            buf.delete(mid..mid + 1);
        });
    });
    drop(buf);
    // Chemin complet d'une frappe : édition + enregistrement dans l'historique, puis annulation
    // (pour garder l'historique de taille constante pendant la mesure).
    doc.set_selections(SelectionSet::single(Selection::caret(mid)));
    let mut t = 0u64;
    g.bench_function("document_type_char_then_undo_middle", |b| {
        b.iter(|| {
            t += 5000; // jamais groupé
            doc.insert_text(black_box("a"), t);
            doc.undo();
        });
    });
    g.finish();
}

// --- 3. Multi-curseurs -----------------------------------------------------------------------

fn bench_multicursor(c: &mut Criterion) {
    let mut g = c.benchmark_group("multicursor_1000");

    // Document moyen (~30 Ko, 1000 lignes) : on mesure l'insertion seule, sans l'annulation.
    g.bench_function("medium_doc_insert_1000_carets", |b| {
        b.iter_batched(
            || {
                let mut d = medium_doc();
                let sels = (0..1000)
                    .map(|i| Selection::caret(d.buffer().line_start(i) + 4))
                    .collect::<Vec<_>>();
                d.set_selections(SelectionSet::from_vec(sels, 0));
                d
            },
            |mut d| {
                d.insert_text(black_box("x"), 0);
                d
            },
            BatchSize::LargeInput,
        );
    });
    g.bench_function("medium_doc_insert_then_undo_1000_carets", |b| {
        let mut d = medium_doc();
        let sels = (0..1000)
            .map(|i| Selection::caret(d.buffer().line_start(i) + 4))
            .collect::<Vec<_>>();
        d.set_selections(SelectionSet::from_vec(sels, 0));
        let mut t = 0u64;
        b.iter(|| {
            t += 5000;
            d.insert_text(black_box("x"), t);
            d.undo();
        });
    });

    // Gros fichier : 1000 curseurs répartis dans les 500 Mo.
    let mut big = load_big();
    big.set_selections(carets_spread(&big, 1000));
    let mut t = 0u64;
    g.bench_function("500MB_insert_then_undo_1000_carets", |b| {
        b.iter(|| {
            t += 5000;
            big.insert_text(black_box("x"), t);
            big.undo();
        });
    });
    g.bench_function("500MB_backspace_then_undo_1000_carets", |b| {
        b.iter(|| {
            t += 5000;
            big.delete_backward(t);
            big.undo();
        });
    });
    g.finish();
}

// --- 4. Divers (utile pour les étapes suivantes) -------------------------------------------------

fn bench_misc(c: &mut Criterion) {
    let doc = load_big();
    let buf = doc.buffer();
    let len = buf.len_chars();
    let lines = buf.len_lines();
    let mut g = c.benchmark_group("misc_500MB");
    let mut rng = Rng(42);
    g.bench_function("char_to_line_col_random", |b| {
        b.iter(|| black_box(buf.char_to_line_col((rng.next() % len as u64) as usize)));
    });
    g.bench_function("line_access_random", |b| {
        b.iter(|| black_box(buf.line((rng.next() % lines as u64) as usize).len_chars()));
    });
    g.bench_function("char_to_byte_random", |b| {
        b.iter(|| black_box(buf.char_to_byte((rng.next() % len as u64) as usize)));
    });
    g.finish();

    let mut g = c.benchmark_group("history");
    g.bench_function("type_10000_chars_grouped_into_one_undo_step", |b| {
        b.iter(|| {
            let mut d = Document::new();
            for i in 0..10_000u64 {
                d.insert_text("a", i * 10);
            }
            assert_eq!(d.history().undo_depth(), 1);
            black_box(d.buffer().len_chars())
        });
    });
    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().configure_from_args();
    targets = bench_open, bench_insert_big, bench_multicursor, bench_misc
}
criterion_main!(benches);
