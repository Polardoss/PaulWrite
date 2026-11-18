//! Coût d'une image de frappe, étape par étape, sans fenêtre (mêmes appels que l'application) :
//! `cargo test --release -p paulwrite-app --test frame_cost -- --ignored --nocapture`.

use std::time::Instant;

use paulwrite_app::editor::Editor;
use paulwrite_core::Document;
use paulwrite_render::{Chrome, Colors, GitMark, SoftRenderer, TabInfo};

fn text(lines: usize) -> String {
    let words = [
        "let", "fn", "value", "buffer", "{", "}", "(", ")", ";", "=", "self", "pub",
    ];
    let mut s = 0x2545_F491_4F6C_DD1D_u64;
    let mut out = String::new();
    for i in 0..lines {
        out.push_str(&" ".repeat(i % 4 * 4));
        for _ in 0..(8 + i % 9) {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            out.push_str(words[(s % words.len() as u64) as usize]);
            out.push(' ');
        }
        out.push('\n');
    }
    out
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn stats(v: &mut [f64]) -> String {
    v.sort_by(f64::total_cmp);
    format!(
        "moy {:.2} | p50 {:.2} | p99 {:.2}",
        v.iter().sum::<f64>() / v.len() as f64,
        v[v.len() / 2],
        v[v.len() * 99 / 100]
    )
}

#[test]
#[ignore = "mesure"]
fn cout_d_une_image_de_frappe() {
    let size = (1100u32, 720u32);
    let mut r = SoftRenderer::new(1.0, size, Colors::default()).unwrap();
    r.set_tabs(&[TabInfo {
        label: "bench.rs",
        modified: false,
        active: true,
        git: GitMark::None,
    }]);
    let rows = r.metrics().rows_for(size.1);
    let mut e = Editor::from_document(Document::from_text(&text(5000)), None);
    e.set_viewport_rows(rows);
    e.scroll_by(100);
    e.click(10, 0, false);
    let mut status = String::new();
    let frame =
        |e: &mut Editor, r: &mut SoftRenderer, status: &mut String, t: &mut [Vec<f64>; 4]| {
            let t0 = Instant::now();
            status.clear();
            e.write_status(status);
            r.set_chrome(&Chrome {
                line_digits: e.line_digits(),
                status_left: status,
                status_right: &["Tab Size: 4", "Rust"],
                scroll: Some(e.scroll()),
            });
            r.set_minimap(&e.minimap(), e.revision()).unwrap();
            t[0].push(ms(t0));
            let t1 = Instant::now();
            e.build_view();
            t[1].push(ms(t1));
            let t2 = Instant::now();
            r.draw(&e.view(), 0).unwrap();
            t[2].push(ms(t2));
            t[3].push(ms(t0));
        };
    let mut t: [Vec<f64>; 4] = Default::default();
    frame(&mut e, &mut r, &mut status, &mut t);
    let mut t: [Vec<f64>; 4] = Default::default();
    for i in 0..300u64 {
        e.insert_text("x", i * 20);
        frame(&mut e, &mut r, &mut status, &mut t);
    }
    println!(
        "image de frappe ({size:?}) :\n  interface (état, minimap) {}\n  construction des rangées  {}\n  moteur de rendu           {}\n  total                     {}",
        stats(&mut t[0]),
        stats(&mut t[1]),
        stats(&mut t[2]),
        stats(&mut t[3])
    );
}

/// Coût du retour à la ligne et du repli sur un gros document (≈ 10 Mo, 150 000 lignes) :
/// construction des rangées, défilements, sauts, Fold All. Sans fenêtre.
#[test]
#[ignore = "mesure"]
fn cout_du_retour_a_la_ligne_et_du_repli() {
    use paulwrite_app::command::Command;
    use paulwrite_core::Motion;

    let doc = text(150_000);
    println!(
        "document : {} lignes, {:.1} Mo",
        doc.lines().count(),
        doc.len() as f64 / 1e6
    );
    for wrap in [false, true] {
        let mut e = Editor::from_document(Document::from_text(&doc), None);
        e.set_viewport_rows(40);
        e.set_wrap(wrap);
        e.set_viewport_cols(90);
        // construction d'une image à des endroits répartis dans le document
        let mut v = Vec::new();
        for i in 0..1000 {
            e.set_first_line(i * 140);
            let t = Instant::now();
            e.build_view();
            v.push(ms(t));
        }
        println!("retour à la ligne {wrap:>5} : build_view {}", stats(&mut v));
        // défilement de 3 rangées + construction (molette)
        let mut v = Vec::new();
        for _ in 0..1000 {
            let t = Instant::now();
            e.scroll_by(3);
            e.build_view();
            v.push(ms(t));
        }
        println!("                    molette + build_view {}", stats(&mut v));
        // sauts : fin, début, page
        let t = Instant::now();
        e.execute(Command::Move(Motion::DocEnd), 0);
        e.build_view();
        let end = ms(t);
        let t = Instant::now();
        e.execute(Command::Move(Motion::DocStart), 0);
        e.build_view();
        let start = ms(t);
        let t = Instant::now();
        for _ in 0..100 {
            e.execute(Command::PageDown(false), 0);
        }
        let pages = ms(t) / 100.0;
        println!(
            "                    Ctrl+Fin {end:.2} ms | Ctrl+Début {start:.2} ms | PageSuiv {pages:.2} ms"
        );
        // frappe à la fin de la 40e ligne visible
        let mut v = Vec::new();
        for i in 0..500 {
            let t = Instant::now();
            e.insert_text("x", i);
            e.build_view();
            v.push(ms(t));
        }
        println!("                    frappe + build_view {}", stats(&mut v));
    }
    // Fold All sur 100 000 lignes
    let mut e = Editor::from_document(Document::from_text(&doc), None);
    e.set_viewport_rows(40);
    e.set_wrap(true);
    e.set_viewport_cols(90);
    let t = Instant::now();
    e.execute(Command::FoldAll, 0);
    e.build_view();
    println!(
        "Fold All : {:.1} ms ({} régions) | Unfold All : {:.1} ms",
        ms(t),
        e.fold_count(),
        {
            let t = Instant::now();
            e.execute(Command::UnfoldAll, 0);
            e.build_view();
            ms(t)
        }
    );
}

/// Coloration du document entier pour la minimap : durée côté worker et coût côté interface
/// (décalage des plages à chaque frappe, construction de l'image colorée de la minimap).
#[test]
#[ignore = "mesure"]
fn cout_de_la_minimap_coloree() {
    use paulwrite_core::Buffer;
    use paulwrite_syntax::{spawn, Language, Request};
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    // ≈ 1 Mo de Rust
    let mut src = String::from("use std::collections::HashMap;\n\n");
    let mut i = 0;
    while src.len() < 1_000_000 {
        src.push_str(&format!(
            "/// Fonction {i}.\nfn compute_{i}(a: i32, b: &str) -> Option<usize> {{\n    let mut map: HashMap<String, usize> = HashMap::new();\n    // calcul {i}\n    if a > {n} {{\n        map.insert(format!(\"cle-{{}}\", b), {n});\n    }}\n    map.get(b).copied()\n}}\n\n",
            n = i * 7 % 1000
        ));
        i += 1;
    }
    let buf = Buffer::from_text(&src);
    let (tx_out, rx_out) = mpsc::channel();
    let tx_out = Mutex::new(tx_out);
    let tx = spawn(move |r| {
        let _ = tx_out.lock().unwrap().send(r);
    });
    tx.send(Request::Open {
        tab: 1,
        language: Language::Rust,
        rope: buf.rope().clone(),
        revision: buf.revision(),
    })
    .unwrap();
    let _ = rx_out.recv_timeout(Duration::from_secs(60)).unwrap();
    let t = Instant::now();
    tx.send(Request::Overview { tab: 1 }).unwrap();
    let r = loop {
        let r = rx_out.recv_timeout(Duration::from_secs(60)).unwrap();
        if r.overview {
            break r;
        }
    };
    println!(
        "document : {:.2} Mo, {} lignes ; coloration entière (worker) : {:.0} ms, {} plages",
        src.len() as f64 / 1e6,
        src.lines().count(),
        ms(t),
        r.spans.len()
    );

    // côté interface : décalage des plages à chaque frappe, image de la minimap
    let size = (1100u32, 720u32);
    let mut rd = SoftRenderer::new(1.0, size, Colors::default()).unwrap();
    rd.set_tabs(&[TabInfo {
        label: "x.rs",
        modified: false,
        active: true,
        git: GitMark::None,
    }]);
    let rows = rd.metrics().rows_for(size.1);
    let mut e = Editor::from_document(Document::from_text(&src), None);
    e.set_viewport_rows(rows);
    e.enable_syntax(true);
    e.set_overview(r.range.clone(), r.spans);
    let mut status = String::new();
    e.write_status(&mut status);
    rd.set_chrome(&Chrome {
        line_digits: e.line_digits(),
        status_left: &status,
        status_right: &["Tab Size: 4", "Rust"],
        scroll: Some(e.scroll()),
    });
    let mut v = Vec::new();
    for k in 0..300 {
        let t = Instant::now();
        e.insert_text("x", k);
        let _ = e.take_edits();
        v.push(ms(t));
    }
    println!(
        "frappe + décalage des plages (take_edits) : {}",
        stats(&mut v)
    );
    let mut v = Vec::new();
    for k in 0..200 {
        let t = Instant::now();
        rd.set_minimap(&e.minimap(), 1000 + k).unwrap();
        v.push(ms(t));
    }
    println!("set_minimap, colorée : {}", stats(&mut v));
    e.enable_syntax(false);
    let mut v = Vec::new();
    for k in 0..200 {
        let t = Instant::now();
        rd.set_minimap(&e.minimap(), 5000 + k).unwrap();
        v.push(ms(t));
    }
    println!("set_minimap, grise : {}", stats(&mut v));
}

/// Barre latérale sur un très gros dossier : lecture du disque (worker) et travail de l'interface
/// (arbre à plat, fenêtre affichée, défilement). `PAULWRITE_BIG_DIR=<n>` fixe le nombre de fichiers.
#[test]
#[ignore = "mesure"]
fn cout_d_un_gros_dossier_dans_la_barre_laterale() {
    use paulwrite_app::sidebar::SidebarTree;
    use paulwrite_app::worker::list_dir;

    let n: usize = std::env::var("PAULWRITE_BIG_DIR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50_000);
    let dir = std::env::temp_dir().join(format!("paulwrite-bigdir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = Instant::now();
    for i in 0..n {
        std::fs::write(dir.join(format!("fichier_{i:06}.rs")), "").unwrap();
    }
    for i in 0..(n / 100) {
        std::fs::create_dir(dir.join(format!("dossier_{i:05}"))).unwrap();
    }
    println!(
        "{n} fichiers et {} dossiers créés en {:.1} s",
        n / 100,
        t.elapsed().as_secs_f64()
    );

    let mut v = Vec::new();
    let mut entries = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        entries = list_dir(&dir);
        v.push(ms(t));
    }
    println!(
        "lecture du dossier (worker, {} entrées) : {:.0} / {:.0} / {:.0} ms",
        entries.len(),
        v[0],
        v[1],
        v[2]
    );

    // côté interface : l'arbre à plat, la fenêtre affichée, le défilement
    let mut tree = SidebarTree::new(dir.clone());
    let _ = tree.take_listing_requests();
    let t = Instant::now();
    tree.set_listing(dir.clone(), entries.clone());
    let rows = tree.render(30).rows.len();
    println!(
        "set_listing + construction de l'arbre à plat + fenêtre ({rows} lignes) : {:.1} ms",
        ms(t)
    );
    let mut v = Vec::new();
    for k in 0..200 {
        let t = Instant::now();
        tree.scroll_by(if k % 2 == 0 { 40 } else { -39 }, 30);
        let _ = tree.render(30);
        v.push(ms(t));
    }
    println!("défilement + fenêtre : {}", stats(&mut v));
    let mut v = Vec::new();
    for k in 0..50 {
        let t = Instant::now();
        let _ = tree.click(2 + (k % 3)); // déplier / replier un sous-dossier
        let _ = tree.render(30);
        v.push(ms(t));
    }
    println!(
        "clic (dépli ou repli d'un dossier) + fenêtre : {}",
        stats(&mut v)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
