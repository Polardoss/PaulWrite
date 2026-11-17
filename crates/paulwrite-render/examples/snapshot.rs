//! Écrit une image PNG du rendu (sans ouvrir de fenêtre) : vérification visuelle du moteur.
//!
//! Usage : `cargo run -p paulwrite-render --example snapshot -- sortie.png [échelle]`

use paulwrite_render::{
    Chrome, Colors, MinimapSource, Overlay, OverlayRow, OverlayRowKind, Row, RowSource, Scroll,
    SoftRenderer, TabInfo,
};

/// Couleurs de la deuxième rangée de la démo : « let » mot-clé, la chaîne, le point-virgule.
const SPANS: [(usize, usize, u8); 3] = [(4, 7, 4), (18, 36, 2), (36, 37, 10)];

/// Soulignements de diagnostics de la démo : une erreur, un avertissement, une information.
const SQUIGGLES: [(usize, usize, u8); 3] = [(1, 12, 0), (0, 4, 1), (7, 12, 2)];

/// Texte, sélections, curseurs, soulignement.
type DemoRow = (
    &'static str,
    Vec<(usize, usize)>,
    Vec<usize>,
    Option<(usize, usize)>,
);

struct Demo {
    lines: Vec<DemoRow>,
}

/// Texte de la minimap : les rangées de la démo répétées.
struct Doc(Vec<String>);

impl MinimapSource for Doc {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn line(&self, index: usize, out: &mut String) {
        out.push_str(&self.0[index]);
    }
}

impl RowSource for Demo {
    fn len(&self) -> usize {
        self.lines.len()
    }
    fn row(&self, i: usize) -> Row<'_> {
        let (text, sels, carets, underline) = &self.lines[i];
        Row {
            text,
            selections: sels,
            carets,
            underline: *underline,
            spans: if i == 1 { &SPANS } else { &[] },
            squiggles: match i {
                2 => &SQUIGGLES[..1],
                5 => &SQUIGGLES[1..],
                _ => &[],
            },
            number: i as u32 + 1,
            current: i == 1,
            fold: match i {
                3 => paulwrite_render::FoldMark::Closed,
                5 => paulwrite_render::FoldMark::Open,
                _ => paulwrite_render::FoldMark::None,
            },
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .expect("usage : snapshot <sortie.png> [échelle]");
    let scale: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let big = std::env::args().any(|a| a == "--grand");
    let (w, h) = if big {
        ((1917.0 * scale) as u32, (988.0 * scale) as u32)
    } else {
        ((760.0 * scale) as u32, (300.0 * scale) as u32)
    };
    let mut r = SoftRenderer::new(scale, (w, h), Colors::default()).expect("rendu");
    let demo = Demo {
        lines: vec![
            ("fn main() {", vec![], vec![], None),
            (
                "    let message = \"héllo wörld 😀\";",
                vec![(4, 11)],
                vec![],
                None,
            ),
            ("\tune tabulation, puis du texte", vec![], vec![], None),
            (
                "    selection sur plusieurs lignes",
                vec![(8, 31)],
                vec![],
                None,
            ),
            ("saut de ligne sélectionné", vec![(0, 26)], vec![], None),
            ("deux curseurs ici", vec![], vec![3, 11], None),
            ("composition IME : かな", vec![], vec![21], Some((17, 19))),
            ("", vec![], vec![0], None),
            ("}", vec![], vec![], None),
        ],
    };
    r.set_tabs(&[
        TabInfo {
            label: "main.rs",
            modified: true,
            active: true,
        },
        TabInfo {
            label: "README.md",
            modified: false,
            active: false,
        },
        TabInfo {
            label: "un_fichier_au_nom_vraiment_tres_long.txt",
            modified: false,
            active: false,
        },
    ]);
    let doc = Doc(demo
        .lines
        .iter()
        .cycle()
        .take(if big { 400 } else { 140 })
        .map(|l| l.0.to_owned())
        .collect());
    r.set_chrome(&Chrome {
        line_digits: 3,
        status_left: "Line 2, Column 5",
        status_right: &["Tab Size: 4", "Rust"],
        scroll: Some(Scroll {
            first: 0,
            visible: r.metrics().rows_for(h),
            total: doc.len(),
        }),
    });
    r.set_minimap(&doc, 1).expect("minimap");
    let rows = r.metrics().rows_for(h);
    let demo = Demo {
        lines: demo.lines.into_iter().cycle().take(rows).collect(),
    };
    if std::env::args().any(|a| a == "--panneau") {
        let item = |kind, text: &str, hint: &str, hl: Vec<(usize, usize)>| OverlayRow {
            kind,
            text: text.into(),
            hint: hint.into(),
            caret: None,
            highlights: hl,
        };
        r.set_overlay(Some(&Overlay {
            rows: vec![
                OverlayRow {
                    kind: OverlayRowKind::Input,
                    text: "> mrs".into(),
                    caret: Some(5),
                    ..OverlayRow::default()
                },
                item(
                    OverlayRowKind::Selected,
                    "src/main.rs",
                    "",
                    vec![(4, 5), (8, 9), (10, 11)],
                ),
                item(OverlayRowKind::Item, "src/mod_rs/lib.rs", "", vec![(4, 5)]),
                item(OverlayRowKind::Item, "Annuler", "Ctrl+Z", vec![]),
                item(
                    OverlayRowKind::Note,
                    "3 résultats sur 100 000 fichiers",
                    "",
                    vec![],
                ),
            ],
            scroll: Some((0, 3, 30)),
        }));
    }
    r.draw(&demo, 0).expect("dessin");
    let (pw, ph) = r.size();
    paulwrite_render::write_png(std::path::Path::new(&out), pw, ph, r.pixels())
        .expect("écriture du PNG");
    println!("écrit {out} ({pw}x{ph}, échelle {scale})");
}
