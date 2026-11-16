//! Cohérence éditeur -> journal de modifications -> analyse incrémentale : après des séquences
//! aléatoires de commandes réelles (frappe, Entrée, retour arrière, suppression, annuler, rétablir,
//! multi-curseurs, collage, sélections), les couleurs de l'analyse incrémentale doivent être
//! identiques à celles d'une analyse complète du même texte, et rien ne doit paniquer ni
//! interrompre le processus (tree-sitter interrompt le processus sur certaines incohérences).

use paulwrite_app::command::Command;
use paulwrite_app::editor::Editor;
use paulwrite_core::{Document, Motion};
use paulwrite_syntax::{Highlighter, Language};

const START: &str = "use std::fmt;\r\n\r\nfn add(a: i32, b: i32) -> i32 {\r\n    // somme é😀\r\n    a + b\r\n}\r\n\r\nstruct P { x: f64 }\r\n";

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn run(seed: u64, lf: bool, steps: usize) {
    let text = if lf {
        START.replace("\r\n", "\n")
    } else {
        START.to_owned()
    };
    let mut e = Editor::from_document(Document::from_text(&text), None);
    e.set_viewport_rows(20);
    e.enable_syntax(true);
    let mut inc = Highlighter::new(Language::Rust).unwrap();
    inc.update(e.document().buffer().rope(), &[]);
    let mut full = Highlighter::new(Language::Rust).unwrap();

    let motions = [
        Motion::Left,
        Motion::Right,
        Motion::Up,
        Motion::Down,
        Motion::WordLeft,
        Motion::WordRight,
        Motion::LineStart,
        Motion::LineEnd,
        Motion::DocStart,
        Motion::DocEnd,
    ];
    let snippets = [
        "a",
        "fn ",
        "{",
        "}",
        "(",
        ")",
        "\"",
        "é",
        "😀",
        "// c",
        "let x = 1;",
        "\n",
        "\r\n",
        "x\ny\nz",
        "struct S;",
        "/*",
        "*/",
    ];
    let mut rng = Rng(seed);
    for step in 0..steps {
        let now = step as u64 * 5;
        match rng.below(14) {
            0..=3 => e.insert_text(snippets[rng.below(snippets.len())], now),
            4 => {
                e.execute(Command::InsertNewline, now);
            }
            5 | 6 => {
                e.execute(Command::DeleteBackward, now);
            }
            7 => {
                e.execute(Command::DeleteForward, now);
            }
            8 => {
                e.execute(Command::Undo, now);
            }
            9 => {
                e.execute(Command::Redo, now);
            }
            10 => {
                e.execute(Command::Move(motions[rng.below(motions.len())]), now);
            }
            11 => {
                e.execute(Command::Extend(motions[rng.below(motions.len())]), now);
            }
            12 => {
                e.execute(
                    if rng.below(2) == 0 {
                        Command::AddCursorAbove
                    } else {
                        Command::AddCursorBelow
                    },
                    now,
                );
            }
            _ => {
                e.execute(Command::CollapseSelections, now);
            }
        }
        // l'application envoie les modifications une fois par image, pas à chaque commande
        if rng.below(3) != 0 {
            let edits = e.take_edits();
            let rope = e.document().buffer().rope();
            inc.update(rope, &edits);
            let len = rope.len_bytes();
            let got = inc.highlights(rope, 0..len);
            full.reset();
            full.update(rope, &[]);
            let want = full.highlights(rope, 0..len);
            assert_eq!(
                got, want,
                "graine {seed}, lf={lf}, étape {step} : couleurs incrémentales différentes"
            );
        }
    }
}

#[test]
fn incremental_egale_complet_avec_de_vraies_commandes() {
    for seed in 1..=12u64 {
        run(
            seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            seed % 2 == 0,
            250,
        );
    }
}
