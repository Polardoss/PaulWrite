//! Éditions groupées (multi-curseurs) et transactions.
//!
//! Une [`Transaction`] est un lot de [`Change`] appliqués *simultanément* : leurs positions sont
//! exprimées dans le document **avant** la transaction, triées et sans chevauchement. On les applique
//! de la fin vers le début, donc les positions restent valides sans recalcul.

use crate::buffer::Buffer;
use crate::fragment::Fragment;
use crate::selection::SelectionSet;

/// Remplacement de `[start, start + removed.len_chars())` par `inserted`.
#[derive(Clone, Debug)]
pub struct Change {
    /// Position dans le document d'avant la transaction.
    pub start: usize,
    pub removed: Fragment,
    pub inserted: Fragment,
}

/// Lot de changements simultanés, avec les sélections avant et après (pour undo/redo).
#[derive(Clone, Debug)]
pub struct Transaction {
    pub changes: Vec<Change>,
    pub sel_before: SelectionSet,
    pub sel_after: SelectionSet,
}

/// Nature d'une édition, pour décider du groupement des frappes dans l'historique.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// Frappe d'un court texte sans saut de ligne : groupable.
    Typing,
    /// Retour arrière répété : groupable.
    DeleteBackward,
    /// Suppression avant répétée : groupable.
    DeleteForward,
    /// Tout le reste (collage, saut de ligne, suppression de sélection...) : jamais groupé.
    Other,
}

/// Applique les changements (sens normal / redo).
pub(crate) fn apply(buf: &mut Buffer, changes: &[Change]) {
    for c in changes.iter().rev() {
        let end = c.start + c.removed.len_chars();
        buf.replace_fragment(c.start, end, &c.inserted);
    }
}

/// Annule les changements (undo) : chaque plage insérée est remplacée par le texte retiré.
pub(crate) fn revert(buf: &mut Buffer, changes: &[Change]) {
    // Décalage cumulé des changements précédents ; calculé de la fin vers le début.
    let mut total: isize = 0;
    for c in changes {
        total += delta(c);
    }
    let mut shift_before_last = total;
    for c in changes.iter().rev() {
        shift_before_last -= delta(c);
        let new_start = (c.start as isize + shift_before_last) as usize;
        let new_end = new_start + c.inserted.len_chars();
        buf.replace_fragment(new_start, new_end, &c.removed);
    }
}

fn delta(c: &Change) -> isize {
    c.inserted.len_chars() as isize - c.removed.len_chars() as isize
}

/// Tente de fusionner `next` dans `prev` (groupement des frappes). Rend `next` si impossible.
pub(crate) fn try_merge(
    prev: &mut Transaction,
    next: Transaction,
    kind: EditKind,
) -> Result<(), Transaction> {
    if prev.changes.len() != next.changes.len() || kind == EditKind::Other {
        return Err(next);
    }
    let mut merged: Vec<Change> = Vec::with_capacity(prev.changes.len());
    let mut shift: isize = 0; // décalage des changements précédents de `prev`
    for (p, n) in prev.changes.iter().zip(&next.changes) {
        let p_start_after = (p.start as isize + shift) as usize;
        let p_ins = p.inserted.len_chars();
        let ok = match kind {
            EditKind::Typing => {
                n.removed.is_empty() && n.start == p_start_after + p_ins && !n.inserted.is_empty()
            }
            EditKind::DeleteBackward => {
                p.inserted.is_empty()
                    && n.inserted.is_empty()
                    && !n.removed.is_empty()
                    && n.start + n.removed.len_chars() == p_start_after
            }
            EditKind::DeleteForward => {
                p.inserted.is_empty()
                    && n.inserted.is_empty()
                    && !n.removed.is_empty()
                    && n.start == p_start_after
            }
            EditKind::Other => false,
        };
        if !ok {
            return Err(next);
        }
        let change = match kind {
            EditKind::Typing => Change {
                start: p.start,
                removed: p.removed.clone(),
                inserted: p.inserted.concat(&n.inserted),
            },
            EditKind::DeleteBackward => Change {
                start: p.start - n.removed.len_chars(),
                removed: n.removed.concat(&p.removed),
                inserted: Fragment::Empty,
            },
            EditKind::DeleteForward => Change {
                start: p.start,
                removed: p.removed.concat(&n.removed),
                inserted: Fragment::Empty,
            },
            EditKind::Other => unreachable!(),
        };
        // Les changements fusionnés doivent rester triés et disjoints.
        if let Some(last) = merged.last() {
            if change.start < last.start + last.removed.len_chars() {
                return Err(next);
            }
        }
        merged.push(change);
        shift += delta(p);
    }
    prev.changes = merged;
    prev.sel_after = next.sel_after;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::{Selection, SelectionSet};

    fn ch(start: usize, removed: &str, inserted: &str) -> Change {
        Change {
            start,
            removed: Fragment::from_text(removed),
            inserted: Fragment::from_text(inserted),
        }
    }

    fn tx(changes: Vec<Change>) -> Transaction {
        let s = SelectionSet::single(Selection::caret(0));
        Transaction {
            changes,
            sel_before: s.clone(),
            sel_after: s,
        }
    }

    #[test]
    fn apply_then_revert_is_identity() {
        let original = "0123456789";
        let changes = vec![ch(1, "1", "ab"), ch(4, "45", ""), ch(8, "", "XYZ")];
        let mut b = Buffer::from_text(original);
        apply(&mut b, &changes);
        assert_eq!(b.to_string(), "0ab23".to_owned() + "6" + "7" + "XYZ" + "89");
        revert(&mut b, &changes);
        assert_eq!(b.to_string(), original);
    }

    #[test]
    fn merge_typing_run() {
        let mut a = tx(vec![ch(2, "", "a"), ch(5, "", "a")]);
        // après le 1er tx : curseurs en 3 et 7 ; le 2e tx insère 'b' à ces positions
        let b = tx(vec![ch(3, "", "b"), ch(7, "", "b")]);
        assert!(try_merge(&mut a, b, EditKind::Typing).is_ok());
        assert_eq!(a.changes[0].inserted.to_string(), "ab");
        assert_eq!(a.changes[1].start, 5);
    }

    #[test]
    fn merge_refuses_non_adjacent() {
        let mut a = tx(vec![ch(2, "", "a")]);
        let b = tx(vec![ch(9, "", "b")]);
        assert!(try_merge(&mut a, b, EditKind::Typing).is_err());
    }

    #[test]
    fn merge_backspace_run() {
        // "abcdef" : on supprime 'c' (2..3) puis 'b' (1..2)
        let mut a = tx(vec![ch(2, "c", "")]);
        let b = tx(vec![ch(1, "b", "")]);
        assert!(try_merge(&mut a, b, EditKind::DeleteBackward).is_ok());
        assert_eq!(a.changes[0].start, 1);
        assert_eq!(a.changes[0].removed.to_string(), "bc");
        let mut buf = Buffer::from_text("abcdef");
        apply(&mut buf, &a.changes);
        assert_eq!(buf.to_string(), "adef");
        revert(&mut buf, &a.changes);
        assert_eq!(buf.to_string(), "abcdef");
    }

    #[test]
    fn merge_delete_forward_run() {
        let mut a = tx(vec![ch(1, "b", "")]);
        let b = tx(vec![ch(1, "c", "")]);
        assert!(try_merge(&mut a, b, EditKind::DeleteForward).is_ok());
        assert_eq!(a.changes[0].removed.to_string(), "bc");
    }
}
