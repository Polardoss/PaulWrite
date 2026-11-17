//! Document éditable : buffer + sélections multiples + historique + métadonnées de fichier.
//!
//! C'est la façade utilisée par les commandes de l'éditeur. Chaque opération agit sur **toutes**
//! les sélections et produit **une seule transaction** d'historique.

use crate::buffer::Buffer;
use crate::edit::{self, Change, EditKind, Transaction};
use crate::encoding::Encoding;
use crate::fragment::Fragment;
use crate::history::History;
use crate::line_ending::LineEnding;
use crate::motion::{self, Motion};
use crate::selection::{Selection, SelectionSet};

/// Taille maximale (octets) d'un texte inséré pour être considéré comme une « frappe » groupable.
const TYPING_MAX_BYTES: usize = 16;

/// Document en cours d'édition.
#[derive(Debug)]
pub struct Document {
    buffer: Buffer,
    selections: SelectionSet,
    history: History,
    encoding: Encoding,
    line_ending: LineEnding,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// Document vide : UTF-8, fins de ligne LF.
    #[must_use]
    pub fn new() -> Self {
        Self::from_buffer(Buffer::new(), Encoding::UTF8, LineEnding::Lf)
    }

    /// Document initialisé avec ce texte (UTF-8, LF), curseur au début.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::from_buffer(Buffer::from_text(text), Encoding::UTF8, LineEnding::Lf)
    }

    /// Document à partir d'un buffer et de ses métadonnées de fichier.
    #[must_use]
    pub fn from_buffer(buffer: Buffer, encoding: Encoding, line_ending: LineEnding) -> Self {
        Self {
            buffer,
            selections: SelectionSet::single(Selection::caret(0)),
            history: History::new(),
            encoding,
            line_ending,
        }
    }

    // --- Accès -------------------------------------------------------------------------------

    /// Le buffer (lecture seule : toute modification passe par le document).
    #[must_use]
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Les sélections.
    #[must_use]
    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    /// L'historique (lecture seule).
    /// Active le journal des modifications du buffer (coloration incrémentale).
    pub fn track_edits(&mut self, on: bool) {
        self.buffer.track_edits(on);
    }

    /// Active le journal des modifications au format LSP (synchronisation d'un serveur de langage).
    pub fn track_changes(&mut self, on: bool) {
        self.buffer.track_changes(on);
    }

    /// Modifications au format LSP (plages UTF-16 d'avant modification) faites depuis le dernier
    /// appel, dans l'ordre. Vide si le suivi est désactivé.
    pub fn take_changes(&mut self) -> Vec<crate::buffer::TextChange> {
        self.buffer.take_changes()
    }

    /// Modifications (octets, points) faites depuis le dernier appel : frappe, collage,
    /// undo/redo, multi-curseurs. Vide si le suivi est désactivé.
    pub fn take_edits(&mut self) -> Vec<crate::buffer::TextEdit> {
        self.buffer.take_edits()
    }

    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Accès mutable à l'historique (réglage du délai de groupement...).
    pub fn history_mut(&mut self) -> &mut History {
        &mut self.history
    }

    /// Encodage du fichier.
    #[must_use]
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Change l'encodage utilisé à la sauvegarde.
    pub fn set_encoding(&mut self, encoding: Encoding) {
        self.encoding = encoding;
    }

    /// Style de fin de ligne pour les nouvelles insertions.
    #[must_use]
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Change le style de fin de ligne des nouvelles insertions.
    pub fn set_line_ending(&mut self, le: LineEnding) {
        self.line_ending = le;
    }

    /// Remplace les sélections (positions bornées à la taille du buffer).
    pub fn set_selections(&mut self, sels: SelectionSet) {
        let max = self.buffer.len_chars();
        let mut sels = sels;
        sels.transform(|s| Selection::new(s.anchor.min(max), s.head.min(max)));
        self.selections = sels;
        self.history.break_group();
    }

    /// Ajoute un curseur (devient le principal).
    pub fn add_cursor(&mut self, pos: usize) {
        self.selections
            .push(Selection::caret(pos.min(self.buffer.len_chars())));
        self.history.break_group();
    }

    /// Sélectionne tout le document.
    pub fn select_all(&mut self) {
        self.selections = SelectionSet::single(Selection::new(0, self.buffer.len_chars()));
        self.history.break_group();
    }

    // --- Déplacements ------------------------------------------------------------------------

    /// Déplace (ou étend, si `extend`) toutes les sélections.
    pub fn move_selections(&mut self, motion: Motion, extend: bool) {
        let buf = &self.buffer;
        self.selections.transform(|s| {
            // Gauche/droite sur une sélection non vide : on replie sur le bord, sans bouger.
            if !extend && !s.is_empty() {
                match motion {
                    Motion::Left => return Selection::caret(s.start()),
                    Motion::Right => return Selection::caret(s.end()),
                    _ => {}
                }
            }
            let head = s.head;
            let (new_head, goal) = match motion {
                Motion::Left => (motion::prev_grapheme(buf, head), None),
                Motion::Right => (motion::next_grapheme(buf, head), None),
                Motion::WordLeft => (motion::prev_word_start(buf, head), None),
                Motion::WordRight => (motion::next_word_start(buf, head), None),
                Motion::LineStart => (motion::line_start(buf, head), None),
                Motion::LineEnd => (motion::line_end(buf, head), None),
                Motion::ParagraphUp => (motion::prev_paragraph(buf, head), None),
                Motion::ParagraphDown => (motion::next_paragraph(buf, head), None),
                Motion::DocStart => (0, None),
                Motion::DocEnd => (buf.len_chars(), None),
                Motion::Up | Motion::Down => {
                    let col = s.goal_col.unwrap_or_else(|| buf.char_to_line_col(head).1);
                    (
                        motion::vertical(buf, head, col, motion == Motion::Down),
                        Some(col),
                    )
                }
            };
            let anchor = if extend { s.anchor } else { new_head };
            Selection {
                anchor,
                head: new_head,
                goal_col: goal,
            }
        });
        self.history.break_group();
    }

    /// Place la tête de la sélection unique à `head` (ou l'étend jusque-là) en mémorisant la
    /// colonne visée `goal_col` : déplacements verticaux par rangées affichées (retour à la
    /// ligne), où la colonne se compte dans la rangée et non dans la ligne.
    pub fn move_head_to(&mut self, head: usize, extend: bool, goal_col: usize) {
        let head = head.min(self.buffer.len_chars());
        self.selections.transform(|s| Selection {
            anchor: if extend { s.anchor } else { head },
            head,
            goal_col: Some(goal_col),
        });
        self.history.break_group();
    }

    // --- Édition -----------------------------------------------------------------------------

    /// Remplace chaque sélection par `text` (frappe, collage). `now_ms` sert au groupement des
    /// frappes. Rend `false` si rien n'a changé.
    pub fn insert_text(&mut self, text: &str, now_ms: u64) -> bool {
        let kind = if text.len() <= TYPING_MAX_BYTES && !text.contains(['\n', '\r']) {
            EditKind::Typing
        } else {
            EditKind::Other
        };
        self.edit(kind, now_ms, text, |_, s| (s.start(), s.end()))
    }

    /// Insère un saut de ligne (selon le style du document) à chaque sélection.
    pub fn insert_newline(&mut self, now_ms: u64) -> bool {
        let nl = self.line_ending.as_str();
        self.edit(EditKind::Other, now_ms, nl, |_, s| (s.start(), s.end()))
    }

    /// Retour arrière : supprime la sélection, ou le grapheme précédent.
    pub fn delete_backward(&mut self, now_ms: u64) -> bool {
        let kind = if self.selections.iter().all(Selection::is_empty) {
            EditKind::DeleteBackward
        } else {
            EditKind::Other
        };
        self.edit(kind, now_ms, "", |buf, s| {
            if s.is_empty() {
                (motion::prev_grapheme(buf, s.head), s.head)
            } else {
                (s.start(), s.end())
            }
        })
    }

    /// Suppression avant : supprime la sélection, ou le grapheme suivant.
    pub fn delete_forward(&mut self, now_ms: u64) -> bool {
        let kind = if self.selections.iter().all(Selection::is_empty) {
            EditKind::DeleteForward
        } else {
            EditKind::Other
        };
        self.edit(kind, now_ms, "", |buf, s| {
            if s.is_empty() {
                (s.head, motion::next_grapheme(buf, s.head))
            } else {
                (s.start(), s.end())
            }
        })
    }

    /// Cœur du chemin d'édition : pour chaque sélection, remplace `range_of(sel)` par `text`.
    fn edit(
        &mut self,
        kind: EditKind,
        now_ms: u64,
        text: &str,
        range_of: impl Fn(&Buffer, &Selection) -> (usize, usize),
    ) -> bool {
        let n = self.selections.len();
        let ins_chars = text.chars().count();
        let inserted = Fragment::from_text(text);
        let mut changes: Vec<Change> = Vec::with_capacity(n);
        let mut carets: Vec<Selection> = Vec::with_capacity(n);
        let mut shift: isize = 0;
        let mut last_end = 0usize;

        for sel in &self.selections {
            let (s, e) = range_of(&self.buffer, sel);
            // Deux plages ne doivent jamais se chevaucher (ex. curseurs dans un même grapheme).
            let s = s.max(last_end);
            let e = e.max(s);
            let removed_chars = e - s;
            if removed_chars == 0 && ins_chars == 0 {
                // Un curseur tombé dans une plage déjà supprimée (ex. deux curseurs dans un même
                // grapheme) est ramené au bord de cette suppression.
                let p = (sel.head.max(last_end) as isize + shift) as usize;
                carets.push(Selection::caret(p));
                last_end = last_end.max(e);
                continue;
            }
            changes.push(Change {
                start: s,
                removed: Fragment::from_slice(self.buffer.slice(s..e)),
                inserted: inserted.clone(),
            });
            carets.push(Selection::caret((s as isize + shift) as usize + ins_chars));
            shift += ins_chars as isize - removed_chars as isize;
            last_end = e;
        }
        if changes.is_empty() {
            return false;
        }

        let sel_before = self.selections.clone();
        edit::apply(&mut self.buffer, &changes);
        self.selections = SelectionSet::from_vec(carets, self.selections.primary_index());
        let tx = Transaction {
            changes,
            sel_before,
            sel_after: self.selections.clone(),
        };
        self.history.record(tx, kind, now_ms);
        true
    }

    /// Annule la dernière transaction. Rend `false` s'il n'y a rien à annuler.
    pub fn undo(&mut self) -> bool {
        let Some(tx) = self.history.take_undo() else {
            return false;
        };
        edit::revert(&mut self.buffer, &tx.changes);
        self.selections = tx.sel_before.clone();
        self.history.push_redo(tx);
        true
    }

    /// Rétablit la dernière transaction annulée.
    pub fn redo(&mut self) -> bool {
        let Some(tx) = self.history.take_redo() else {
            return false;
        };
        edit::apply(&mut self.buffer, &tx.changes);
        self.selections = tx.sel_after.clone();
        self.history.push_undo(tx);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str, carets: &[usize]) -> Document {
        let mut d = Document::from_text(text);
        d.set_selections(SelectionSet::from_vec(
            carets.iter().map(|&p| Selection::caret(p)).collect(),
            0,
        ));
        d
    }

    fn text(d: &Document) -> String {
        d.buffer().to_string()
    }

    fn heads(d: &Document) -> Vec<usize> {
        d.selections().iter().map(|s| s.head).collect()
    }

    #[test]
    fn multi_cursor_typing_is_one_undo_step() {
        let mut d = doc("a b c", &[1, 3, 5]);
        assert!(d.insert_text("X", 0));
        assert_eq!(text(&d), "aX bX cX");
        assert_eq!(heads(&d), vec![2, 5, 8]);
        assert_eq!(d.history().undo_depth(), 1);
        assert!(d.undo());
        assert_eq!(text(&d), "a b c");
        assert_eq!(heads(&d), vec![1, 3, 5]);
        assert!(d.redo());
        assert_eq!(text(&d), "aX bX cX");
        assert_eq!(heads(&d), vec![2, 5, 8]);
    }

    #[test]
    fn typing_is_grouped_within_interval_and_not_after() {
        let mut d = doc("", &[0]);
        for (i, c) in ["a", "b", "c"].iter().enumerate() {
            d.insert_text(c, i as u64 * 100);
        }
        assert_eq!(d.history().undo_depth(), 1);
        d.insert_text("d", 5000); // pause trop longue
        assert_eq!(d.history().undo_depth(), 2);
        d.undo();
        assert_eq!(text(&d), "abc");
        d.undo();
        assert_eq!(text(&d), "");
    }

    #[test]
    fn moving_the_cursor_breaks_the_group() {
        let mut d = doc("", &[0]);
        d.insert_text("a", 0);
        d.move_selections(Motion::Left, false);
        d.move_selections(Motion::Right, false);
        d.insert_text("b", 10);
        assert_eq!(d.history().undo_depth(), 2);
    }

    #[test]
    fn newline_and_paste_are_never_grouped() {
        let mut d = doc("", &[0]);
        d.insert_text("a", 0);
        d.insert_newline(1);
        d.insert_text("b", 2);
        assert_eq!(d.history().undo_depth(), 3);
    }

    #[test]
    fn newline_uses_document_line_ending() {
        let mut d = doc("ab", &[1]);
        d.set_line_ending(LineEnding::CrLf);
        d.insert_newline(0);
        assert_eq!(text(&d), "a\r\nb");
        assert_eq!(heads(&d), vec![3]);
    }

    #[test]
    fn backspace_run_groups_and_undoes_at_once() {
        let mut d = doc("abcdef", &[6]);
        for t in 0..3 {
            assert!(d.delete_backward(t * 10));
        }
        assert_eq!(text(&d), "abc");
        assert_eq!(d.history().undo_depth(), 1);
        d.undo();
        assert_eq!(text(&d), "abcdef");
        assert_eq!(heads(&d), vec![6]);
    }

    #[test]
    fn backspace_at_start_is_a_noop() {
        let mut d = doc("ab", &[0]);
        assert!(!d.delete_backward(0));
        assert!(!d.history().can_undo());
    }

    #[test]
    fn backspace_merges_cursors_that_collide() {
        let mut d = doc("abc", &[1, 2]);
        d.delete_backward(0);
        assert_eq!(text(&d), "c");
        assert_eq!(heads(&d), vec![0]);
        d.undo();
        assert_eq!(text(&d), "abc");
        assert_eq!(heads(&d), vec![1, 2]);
    }

    #[test]
    fn cursors_inside_one_grapheme_do_not_underflow() {
        // Régression proptest : le 2e curseur est dans la plage supprimée par le 1er.
        let mut d = doc("e\u{301}x", &[0, 1]);
        assert!(d.delete_forward(0));
        assert_eq!(text(&d), "x");
        assert_eq!(heads(&d), vec![0]);
        assert!(d.undo());
        assert_eq!(text(&d), "e\u{301}x");
        let mut d = doc("xe\u{301}", &[2, 3]);
        assert!(d.delete_backward(0));
        assert_eq!(text(&d), "x");
        assert_eq!(heads(&d), vec![1]);
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut d = doc("hello world", &[]);
        d.set_selections(SelectionSet::single(Selection::new(0, 5)));
        d.insert_text("bye", 0);
        assert_eq!(text(&d), "bye world");
        assert_eq!(heads(&d), vec![3]);
    }

    #[test]
    fn delete_grapheme_clusters_and_crlf() {
        let mut d = doc("e\u{301}x\r\ny", &[2]);
        d.delete_backward(0);
        assert_eq!(text(&d), "x\r\ny");
        let mut d = doc("a\r\nb", &[1]);
        d.delete_forward(0);
        assert_eq!(text(&d), "ab", "CRLF se supprime d'un bloc");
    }

    #[test]
    fn motions_collapse_and_extend() {
        let mut d = doc("hello\nworld", &[2]);
        d.move_selections(Motion::Right, true);
        d.move_selections(Motion::Right, true);
        assert_eq!(d.selections().primary(), Selection::new(2, 4));
        d.move_selections(Motion::Left, false);
        assert_eq!(
            d.selections().primary(),
            Selection::caret(2),
            "replie au début"
        );
        d.move_selections(Motion::Down, false);
        assert_eq!(heads(&d), vec![8]);
        d.move_selections(Motion::DocEnd, false);
        assert_eq!(heads(&d), vec![11]);
    }

    #[test]
    fn goal_column_is_remembered_over_short_lines() {
        let mut d = doc("abcdef\nab\nabcdef", &[5]);
        d.move_selections(Motion::Down, false);
        assert_eq!(heads(&d), vec![9], "ligne courte : fin de ligne");
        d.move_selections(Motion::Down, false);
        assert_eq!(heads(&d), vec![15], "retrouve la colonne 5");
    }

    #[test]
    fn cursors_moving_onto_each_other_merge() {
        let mut d = doc("ab", &[0, 1]);
        d.move_selections(Motion::Left, false);
        assert_eq!(d.selections().len(), 1);
    }
}
