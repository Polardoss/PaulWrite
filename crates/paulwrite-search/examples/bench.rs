//! Mesures de la recherche sur un projet généré par `gen_project`.
//!
//! Usage : `cargo run --release -p paulwrite-search --example bench -- <dossier>`

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use paulwrite_search::{
    list_files, search_text, Event, FuzzyFinder, IndexOptions, SearchOptions, TextQuery,
};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn stats(v: &mut [Duration]) -> String {
    v.sort();
    let q = |p: f64| ms(v[((v.len() - 1) as f64 * p).round() as usize]);
    format!(
        "n={} p50 {:.2} ms | p95 {:.2} | p99 {:.2} | max {:.2}",
        v.len(),
        q(0.5),
        q(0.95),
        q(0.99),
        q(1.0)
    )
}

fn bench_list(root: &Path) {
    for pass in 1..=3 {
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = AtomicBool::new(false);
        let t = Instant::now();
        let mut first = None;
        let n = std::thread::scope(|s| {
            let h = s.spawn(|| list_files(root, &IndexOptions::default(), &cancel, tx));
            for _ in rx.iter() {
                first.get_or_insert(t.elapsed());
            }
            h.join().unwrap()
        });
        println!(
            "liste des fichiers, passage {pass} : {n} fichiers | premier lot {:.1} ms | total {:.0} ms",
            ms(first.unwrap_or_default()),
            ms(t.elapsed())
        );
    }
}

fn bench_text(root: &Path, label: &str, pattern: &str, regex: bool, max: usize) {
    for pass in 1..=3 {
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = AtomicBool::new(false);
        let t = Instant::now();
        let mut first = None;
        let mut total = 0usize;
        let mut summary = None;
        std::thread::scope(|s| {
            s.spawn(|| {
                search_text(
                    root,
                    &TextQuery {
                        pattern: pattern.to_owned(),
                        regex,
                        case_sensitive: true,
                    },
                    &SearchOptions {
                        max_results: max,
                        ..SearchOptions::default()
                    },
                    &cancel,
                    tx,
                )
                .unwrap();
            });
            for e in rx.iter() {
                match e {
                    Event::Matches(m) => {
                        first.get_or_insert(t.elapsed());
                        total += m.len();
                    }
                    Event::Done(sum) => summary = Some(sum),
                }
            }
        });
        let s = summary.unwrap();
        println!(
            "recherche {label}, passage {pass} : {total} résultats | premiers résultats {:.1} ms | total {:.0} ms | {} fichiers examinés",
            ms(first.unwrap_or_default()),
            ms(t.elapsed()),
            s.files
        );
    }
}

fn bench_fuzzy(root: &Path) {
    // 1. indexation : liste du projet injectée dans nucleo
    let mut finder = FuzzyFinder::new(|| {});
    let injector = finder.injector();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    std::thread::scope(|s| {
        s.spawn(|| list_files(root, &IndexOptions::default(), &cancel, tx));
        for batch in rx.iter() {
            for p in batch {
                FuzzyFinder::add(&injector, p);
            }
        }
    });
    while finder.tick(10).running {}
    println!(
        "fuzzy : {} chemins indexés en {:.0} ms",
        finder.item_count(),
        ms(t.elapsed())
    );

    // 2. frappes : chaque caractère ajouté relance le classement ; on mesure jusqu'au résultat
    let mut per_key = Vec::new();
    let mut first_key = Vec::new();
    for query in [
        "mod_017/sub_05/file_0023.py",
        "sub_12 file_00 py",
        "mod_099 sub_19 file_0049 md",
        "mod_999",
        "readme",
    ] {
        finder.set_query("");
        while finder.tick(10).running {}
        let mut typed = String::new();
        for (i, c) in query.chars().enumerate() {
            typed.push(c);
            // `BENCH_KEY_PAUSE_MS` : pause entre deux frappes, comme un vrai clavier (les threads
            // de classement se rendorment entre deux frappes)
            if let Some(ms) = std::env::var("BENCH_KEY_PAUSE_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
            {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            let t = Instant::now();
            finder.set_query(&typed);
            while finder.tick(1).running {}
            let d = t.elapsed();
            // un dernier tick pour récupérer un éventuel changement publié juste après
            finder.tick(0);
            per_key.push(d);
            if i == 0 {
                first_key.push(d);
            }
        }
        println!(
            "  « {query} » -> {} résultats, meilleur : {}",
            finder.matched_count(),
            finder.top(1).first().map_or("-", String::as_str)
        );
    }
    println!("fuzzy, temps par frappe : {}", stats(&mut per_key));
    println!(
        "fuzzy, 1re lettre (tous les candidats) : {}",
        stats(&mut first_key)
    );
}

fn main() {
    let root = std::env::args().nth(1).expect("usage : bench <dossier>");
    let root = Path::new(&root);
    bench_list(root);
    bench_text(
        root,
        "littéral rare « needle_xyz »",
        "needle_xyz",
        false,
        10_000,
    );
    bench_text(
        root,
        "regex courante « fn \\w+_handler »",
        r"fn \w+_handler",
        true,
        10_000,
    );
    bench_text(
        root,
        "regex courante limitée à 100 résultats",
        r"fn \w+_handler",
        true,
        100,
    );
    bench_fuzzy(root);
}
