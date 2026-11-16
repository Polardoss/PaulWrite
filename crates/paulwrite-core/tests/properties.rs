//! Tests de propriétés (proptest) : invariants du buffer, des sélections, de l'historique et de
//! l'I/O, vérifiés sur des entrées aléatoires.

use paulwrite_core::io::{load_file, save_file, LoadOptions};
use paulwrite_core::{Buffer, Document, Encoding, Motion, Selection, SelectionSet};
use proptest::prelude::*;

// --- Générateurs ---------------------------------------------------------------------------

fn any_text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..max).prop_map(String::from_iter)
}

/// Texte aux sauts de ligne variés (LF, CRLF, CR isolé) et caractères multi-octets.
fn code_like_text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            4 => Just("a".to_string()),
            2 => Just(" ".to_string()),
            2 => Just("\n".to_string()),
            1 => Just("\r\n".to_string()),
            1 => Just("\r".to_string()),
            1 => Just("é".to_string()),
            1 => Just("€".to_string()),
            1 => Just("😀".to_string()),
            1 => Just("e\u{301}".to_string()),
            1 => Just("_".to_string()),
            1 => Just(".".to_string()),
        ],
        0..max,
    )
    .prop_map(|v| v.concat())
}

#[derive(Clone, Debug)]
enum Op {
    Insert(String),
    Newline,
    Backspace,
    Delete,
    Move(Motion, bool),
    AddCursor(usize),
    SelectAll,
    Undo,
    Redo,
}

fn any_motion() -> impl Strategy<Value = Motion> {
    prop::sample::select(vec![
        Motion::Left,
        Motion::Right,
        Motion::Up,
        Motion::Down,
        Motion::WordLeft,
        Motion::WordRight,
        Motion::LineStart,
        Motion::LineEnd,
        Motion::ParagraphUp,
        Motion::ParagraphDown,
        Motion::DocStart,
        Motion::DocEnd,
    ])
}

fn any_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => code_like_text(6).prop_map(Op::Insert),
        1 => Just(Op::Newline),
        3 => Just(Op::Backspace),
        2 => Just(Op::Delete),
        4 => (any_motion(), any::<bool>()).prop_map(|(m, e)| Op::Move(m, e)),
        1 => (0usize..60).prop_map(Op::AddCursor),
        1 => Just(Op::SelectAll),
        2 => Just(Op::Undo),
        1 => Just(Op::Redo),
    ]
}

// --- Invariants -----------------------------------------------------------------------------

fn assert_selection_invariants(set: &SelectionSet, len: usize) {
    let s = set.as_slice();
    assert!(!s.is_empty());
    assert!(set.primary_index() < s.len());
    for sel in s {
        assert!(
            sel.anchor <= len && sel.head <= len,
            "sélection hors bornes: {sel:?} > {len}"
        );
    }
    for w in s.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        assert!(a.end() <= b.start(), "chevauchement: {a:?} / {b:?}");
        assert!(
            !(a.end() == b.start() && (a.is_empty() || b.is_empty())),
            "curseur collé non fusionné: {a:?} / {b:?}"
        );
    }
}

fn assert_buffer_consistent(buf: &Buffer) {
    let s = buf.to_string();
    assert_eq!(s.chars().count(), buf.len_chars());
    assert_eq!(s.len(), buf.len_bytes());
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn selection_sets_are_always_normalized(
        raw in prop::collection::vec((0usize..60, 0usize..60), 0..12),
        primary in 0usize..16,
    ) {
        let input: Vec<Selection> = raw.iter().map(|&(a, h)| Selection::new(a, h)).collect();
        let set = SelectionSet::from_vec(input.clone(), primary);
        assert_selection_invariants(&set, 60);
        // Rien n'est perdu : chaque sélection d'entrée est couverte par une sélection de sortie.
        for i in &input {
            prop_assert!(
                set.iter().any(|o| o.start() <= i.start() && i.end() <= o.end()),
                "{i:?} non couverte par {set:?}"
            );
        }
        // Idempotence.
        let again = SelectionSet::from_vec(set.as_slice().to_vec(), set.primary_index());
        prop_assert_eq!(&again, &set);
    }

    /// Séquence aléatoire d'opérations multi-curseurs : invariants à chaque pas, puis
    /// « tout annuler = état initial » et « tout rétablir = état final ».
    #[test]
    fn random_editing_keeps_invariants_and_undo_redo_roundtrips(
        initial in code_like_text(40),
        carets in prop::collection::vec(0usize..45, 1..5),
        ops in prop::collection::vec((any_op(), 0u64..3000), 0..40),
    ) {
        let mut doc = Document::from_text(&initial);
        let len0 = doc.buffer().len_chars();
        doc.set_selections(SelectionSet::from_vec(
            carets.iter().map(|&c| Selection::caret(c.min(len0))).collect(), 0));

        let mut now = 0u64;
        for (op, dt) in ops {
            now += dt;
            match op {
                Op::Insert(t) => { doc.insert_text(&t, now); }
                Op::Newline => { doc.insert_newline(now); }
                Op::Backspace => { doc.delete_backward(now); }
                Op::Delete => { doc.delete_forward(now); }
                Op::Move(m, e) => doc.move_selections(m, e),
                Op::AddCursor(p) => doc.add_cursor(p),
                Op::SelectAll => doc.select_all(),
                Op::Undo => { doc.undo(); }
                Op::Redo => { doc.redo(); }
            }
            assert_selection_invariants(doc.selections(), doc.buffer().len_chars());
            assert_buffer_consistent(doc.buffer());
        }

        // Annuler tout ce qui reste, depuis l'état courant.
        // (Les sélections ne sont pas comparées ici : les déplacements entre deux éditions les
        // modifient légitimement ; `insert_then_undo_is_identity` couvre ce point.)
        let final_text = doc.buffer().to_string();
        let mut undone = 0;
        while doc.undo() { undone += 1; }
        // Après avoir tout annulé on retrouve exactement l'état initial.
        prop_assert_eq!(doc.buffer().to_string(), initial);
        assert_selection_invariants(doc.selections(), doc.buffer().len_chars());
        // Tout rétablir ramène à l'état de départ de cette phase.
        for _ in 0..undone { prop_assert!(doc.redo()); }
        prop_assert_eq!(doc.buffer().to_string(), final_text);
        assert_selection_invariants(doc.selections(), doc.buffer().len_chars());
    }

    /// Insérer puis annuler redonne l'état initial, pour n'importe quels curseurs et texte.
    #[test]
    fn insert_then_undo_is_identity(
        initial in any_text(60),
        text in any_text(8),
        carets in prop::collection::vec(0usize..70, 1..6),
    ) {
        let mut doc = Document::from_text(&initial);
        let len = doc.buffer().len_chars();
        doc.set_selections(SelectionSet::from_vec(
            carets.iter().map(|&c| Selection::caret(c.min(len))).collect(), 0));
        let sels = doc.selections().clone();
        if doc.insert_text(&text, 0) {
            prop_assert!(doc.undo());
        }
        prop_assert_eq!(doc.buffer().to_string(), initial);
        prop_assert_eq!(doc.selections(), &sels);
    }

    /// Modèle de référence à un curseur : texte sans grapheme composé ni CR, donc grapheme = char.
    #[test]
    fn single_cursor_matches_a_reference_model(
        ops in prop::collection::vec(
            prop_oneof![
                4 => prop::collection::vec(
                        prop::sample::select(vec!['a', 'b', ' ', 'é', '€', '😀', '\n']), 1..4)
                     .prop_map(|v| (0u8, v.into_iter().collect::<String>())),
                2 => Just((1u8, String::new())),
                2 => Just((2u8, String::new())),
                2 => Just((3u8, String::new())),
                2 => Just((4u8, String::new())),
            ],
            0..60),
    ) {
        let mut doc = Document::new();
        let mut model: Vec<char> = Vec::new();
        let mut cur = 0usize;
        for (i, (kind, text)) in ops.into_iter().enumerate() {
            let now = i as u64;
            match kind {
                0 => {
                    for (k, c) in text.chars().enumerate() { model.insert(cur + k, c); }
                    cur += text.chars().count();
                    doc.insert_text(&text, now);
                }
                1 => { if cur > 0 { model.remove(cur - 1); cur -= 1; } doc.delete_backward(now); }
                2 => { if cur < model.len() { model.remove(cur); } doc.delete_forward(now); }
                3 => { cur = cur.saturating_sub(1); doc.move_selections(Motion::Left, false); }
                _ => { cur = (cur + 1).min(model.len()); doc.move_selections(Motion::Right, false); }
            }
            prop_assert_eq!(doc.buffer().to_string(), model.iter().collect::<String>());
            prop_assert_eq!(doc.selections().primary().head, cur);
        }
    }

    #[test]
    fn position_conversions_are_consistent(text in code_like_text(80)) {
        let buf = Buffer::from_text(&text);
        for i in 0..=buf.len_chars() {
            prop_assert_eq!(buf.byte_to_char(buf.char_to_byte(i)), i);
            prop_assert_eq!(buf.utf16_to_char(buf.char_to_utf16(i)), i);
            let (line, col) = buf.char_to_line_col(i);
            prop_assert_eq!(buf.line_start(line) + col, i);
            // La colonne est bornée à la fin de ligne (entre \r et \n, on retombe sur la fin).
            let back = buf.line_col_to_char(line, col);
            prop_assert_eq!(back, i.min(buf.line_end(line)));
        }
        // Les lignes recollées redonnent le texte.
        let joined: String = (0..buf.len_lines()).map(|l| buf.line(l).to_string()).collect();
        prop_assert_eq!(joined, text);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// charge ∘ sauvegarde = identité, pour chaque encodage, avec des morceaux de taille quelconque.
    #[test]
    fn save_then_load_roundtrips(
        text in code_like_text(120),
        enc in prop::sample::select(vec![
            Encoding::UTF8, Encoding::UTF8_BOM, Encoding::UTF16_LE_BOM, Encoding::UTF16_BE_BOM,
        ]),
        chunk in 1usize..40,
        mmap in any::<bool>(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let doc = Document::from_text(&text);
        save_file(&path, doc.buffer().rope(), enc).unwrap();
        let opts = LoadOptions {
            mmap_threshold: if mmap { 0 } else { u64::MAX },
            chunk_size: chunk,
        };
        let loaded = load_file(&path, &opts, None).unwrap();
        prop_assert_eq!(loaded.rope.to_string(), text);
        prop_assert_eq!(loaded.encoding, enc);
    }
}
