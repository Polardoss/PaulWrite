//! Vues jumelles d'un même fichier : deux documents tenus identiques par rejeu des modifications,
//! avec un historique d'annulation commun qui suit la vue qui reçoit la saisie.

use paulwrite_core::{Document, EditKind, Selection, SelectionSet};

fn text(d: &Document) -> String {
    d.buffer().to_string()
}

/// Les deux textes et leurs révisions sont identiques, et les sélections restent dans les bornes.
fn assert_twins(a: &Document, b: &Document, ctx: &str) {
    assert_eq!(text(a), text(b), "texte différent : {ctx}");
    assert_eq!(
        a.buffer().revision(),
        b.buffer().revision(),
        "révision différente : {ctx}"
    );
    for d in [a, b] {
        assert!(d.selections().max_pos() <= d.buffer().len_chars(), "{ctx}");
    }
}

/// Suite pseudo-aléatoire déterministe.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn les_jumelles_restent_identiques_apres_n_importe_quelle_edition() {
    let mut a = Document::from_text("fn main() {\n    let x = 1;\n}\n");
    a.track_mirror(true);
    let mut b = a.fork();
    b.track_mirror(true);
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);

    for step in 0..400 {
        let len = a.buffer().len_chars();
        match rng.below(7) {
            0 => {
                let p = rng.below(len + 1);
                a.set_selections(SelectionSet::single(Selection::caret(p)));
                a.insert_text("é𝄞x", step);
            }
            1 => {
                let p = rng.below(len + 1);
                a.add_cursor(p);
                a.insert_text("ab", step);
            }
            2 => {
                a.delete_backward(step);
            }
            3 => {
                a.delete_forward(step);
            }
            4 => {
                a.undo();
            }
            5 => {
                a.redo();
            }
            _ => {
                let s = rng.below(len + 1);
                let e = (s + rng.below(5)).min(len);
                let sels = SelectionSet::single(Selection::caret(s));
                a.apply_replacements(&[(s, e, "\n  ".to_owned())], sels, EditKind::Other, step);
            }
        }
        // les modifications de `b` (sélection propre) s'ajoutent de temps en temps : la vue qui
        // reçoit la saisie change
        b.apply_mirror(&a.take_mirror());
        assert_twins(&a, &b, &format!("étape {step}, a vers b"));
        if step % 5 == 0 {
            let p = rng.below(b.buffer().len_chars() + 1);
            b.set_selections(SelectionSet::single(Selection::caret(p)));
            b.insert_text("Z", step);
            a.apply_mirror(&b.take_mirror());
            assert_twins(&a, &b, &format!("étape {step}, b vers a"));
        }
    }
}

#[test]
fn l_historique_est_commun_et_suit_la_saisie() {
    let mut a = Document::from_text("abc");
    a.track_mirror(true);
    let mut b = a.fork();
    b.track_mirror(true);

    // saisie dans `a`
    a.set_selections(SelectionSet::single(Selection::caret(3)));
    a.insert_text("1", 0);
    a.history_mut().break_group();
    a.insert_text("2", 10_000);
    b.apply_mirror(&a.take_mirror());
    assert_eq!(text(&b), "abc12");

    // le focus passe à `b` : il reçoit l'historique (et `a` celui, vide, de `b`)
    b.swap_history(&mut a);
    assert!(b.history().undo_depth() >= 1);
    assert_eq!(a.history().undo_depth(), 0);

    // annuler dans `b` annule la saisie faite dans `a`, et `a` suit
    assert!(b.undo());
    a.apply_mirror(&b.take_mirror());
    assert_twins(&a, &b, "après annulation");
    assert_ne!(text(&b), "abc12");
}

#[test]
fn les_selections_de_la_jumelle_suivent_le_texte() {
    let mut a = Document::from_text("0123456789");
    a.track_mirror(true);
    let mut b = a.fork();
    b.track_mirror(true);
    b.set_selections(SelectionSet::single(Selection::new(6, 8)));

    // insertion avant : la sélection de `b` se décale
    a.set_selections(SelectionSet::single(Selection::caret(2)));
    a.insert_text("XX", 0);
    b.apply_mirror(&a.take_mirror());
    assert_eq!(b.selections().primary(), Selection::new(8, 10));

    // suppression qui recouvre le début de la sélection : elle se replie sur le bord
    a.set_selections(SelectionSet::single(Selection::new(5, 9)));
    a.delete_backward(1);
    b.apply_mirror(&a.take_mirror());
    let s = b.selections().primary();
    assert!(s.end() <= b.buffer().len_chars());
    assert_twins(&a, &b, "après suppression");

    // un curseur exactement au point d'insertion reste devant le texte inséré
    b.set_selections(SelectionSet::single(Selection::caret(3)));
    a.set_selections(SelectionSet::single(Selection::caret(3)));
    a.insert_text("QQ", 2);
    b.apply_mirror(&a.take_mirror());
    assert_eq!(b.selections().primary(), Selection::caret(3));
}

#[test]
fn des_operations_hors_bornes_sont_ignorees() {
    use paulwrite_core::MirrorOp;
    let mut d = Document::from_text("abc");
    d.apply_mirror(&[
        MirrorOp::Insert {
            at: 99,
            text: "x".into(),
        },
        MirrorOp::Delete { start: 2, end: 99 },
        MirrorOp::Delete { start: 3, end: 3 },
        MirrorOp::Insert {
            at: 3,
            text: "d".into(),
        },
    ]);
    assert_eq!(text(&d), "abcd");
}

#[test]
fn sans_suivi_aucun_journal() {
    let mut a = Document::from_text("abc");
    a.insert_text("x", 0);
    assert!(a.take_mirror().is_empty());
    // les opérations rejouées ne repartent pas en écho
    let mut b = a.fork();
    b.track_mirror(true);
    b.apply_mirror(&[paulwrite_core::MirrorOp::Insert {
        at: 0,
        text: "y".into(),
    }]);
    assert!(b.take_mirror().is_empty());
}
