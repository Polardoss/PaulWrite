//! Buffer de texte basé sur [`ropey::Rope`].
//!
//! Toutes les positions sont des **offsets de caractères** (natif ropey), sauf mention contraire.
//! Des conversions byte / char / ligne-colonne / UTF-16 sont fournies pour tree-sitter (octets) et
//! le futur client LSP (UTF-16).
//!
//! Sauts de ligne reconnus : `\n` et `\r\n` uniquement. Un `\r` isolé est un caractère ordinaire.
//!
//! Les méthodes paniquent si une position dépasse la fin du buffer (comportement de ropey) : la
//! validité des positions est de la responsabilité de l'appelant (le [`crate::Document`] garantit
//! les invariants pour ses propres sélections).

use std::fmt;
use std::ops::Range;

use ropey::{Rope, RopeSlice};

use crate::fragment::Fragment;

/// Une modification du texte, exprimée en octets et en (ligne, colonne en octets) : exactement ce
/// qu'attend un analyseur incrémental comme tree-sitter (`InputEdit`). Le core n'en dépend pas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    /// (ligne, colonne en octets).
    pub start_point: (usize, usize),
    pub old_end_point: (usize, usize),
    pub new_end_point: (usize, usize),
}

/// Une modification du texte au format du protocole LSP : plage `(ligne, colonne en unités
/// UTF-16)` **dans le texte d'avant la modification**, et texte inséré à la place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChange {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub text: String,
}

/// Une modification primitive à rejouer sur un autre buffer : sert à garder à jour les **vues
/// jumelles** d'un même fichier (chacune a son propre buffer, tenu identique par rejeu).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorOp {
    /// Insertion de `text` à l'offset de caractères `at`.
    Insert { at: usize, text: String },
    /// Suppression des caractères `[start, end)`.
    Delete { start: usize, end: usize },
}

/// Contenu texte d'un document.
#[derive(Clone, Default)]
pub struct Buffer {
    rope: Rope,
    revision: u64,
    /// Journal des modifications à rejouer sur les vues jumelles, tenu seulement si
    /// [`Buffer::track_mirror`] l'a activé (aucun coût sinon).
    mirror_log: Option<Vec<MirrorOp>>,
    /// Journal des modifications au format LSP, tenu seulement si [`Buffer::track_changes`]
    /// l'a activé (aucun coût sinon).
    change_log: Option<Vec<TextChange>>,
    /// Journal des modifications, tenu seulement si [`Buffer::track_edits`] l'a activé (aucun coût
    /// sinon).
    edit_log: Option<Vec<TextEdit>>,
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Ne jamais afficher le contenu : il peut faire 500 Mo.
        f.debug_struct("Buffer")
            .field("len_chars", &self.rope.len_chars())
            .field("len_lines", &self.rope.len_lines())
            .field("revision", &self.revision)
            .finish()
    }
}

impl Buffer {
    /// Buffer vide.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Buffer initialisé avec ce texte.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            revision: 0,
            mirror_log: None,
            edit_log: None,
            change_log: None,
        }
    }

    /// Buffer construit à partir d'un rope existant (ex. chargement de fichier).
    #[must_use]
    pub fn from_rope(rope: Rope) -> Self {
        Self {
            rope,
            revision: 0,
            mirror_log: None,
            edit_log: None,
            change_log: None,
        }
    }

    /// Copie du texte et de la révision, sans aucun journal : le point de départ d'une vue
    /// jumelle (le rope se copie en temps constant).
    #[must_use]
    pub fn snapshot(&self) -> Self {
        Self {
            rope: self.rope.clone(),
            revision: self.revision,
            mirror_log: None,
            edit_log: None,
            change_log: None,
        }
    }

    /// Accès en lecture seule au rope (itération par chunks, tranches, etc.).
    #[must_use]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Numéro de révision : incrémenté à chaque modification (invalidation de caches).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Nombre de caractères.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Nombre d'octets UTF-8.
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Nombre de lignes (au moins 1 ; une dernière ligne vide compte après un saut final).
    #[must_use]
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Vrai si le buffer est vide.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    // --- Édition -----------------------------------------------------------------------------

    /// Active ou désactive le journal des modifications (pour la coloration incrémentale).
    /// Désactiver vide le journal.
    pub fn track_edits(&mut self, on: bool) {
        self.edit_log = on.then(Vec::new);
    }

    /// Retire et renvoie les modifications faites depuis le dernier appel, dans l'ordre où elles
    /// doivent être appliquées à un analyseur. Vide si le suivi est désactivé.
    pub fn take_edits(&mut self) -> Vec<TextEdit> {
        self.edit_log
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Active ou désactive le journal des modifications à rejouer sur les vues jumelles.
    /// Désactiver vide le journal.
    pub fn track_mirror(&mut self, on: bool) {
        self.mirror_log = on.then(Vec::new);
    }

    /// Retire et renvoie les modifications faites depuis le dernier appel, dans l'ordre. Vide si
    /// le suivi est désactivé.
    pub fn take_mirror(&mut self) -> Vec<MirrorOp> {
        self.mirror_log
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Rejoue des modifications venues d'une vue jumelle. Elles ne sont **pas** remises dans le
    /// journal des jumelles (sinon elles repartiraient en écho) ; les journaux de coloration, de
    /// repli et LSP, eux, les voient comme n'importe quelle modification. Une opération qui sort
    /// des bornes (texte rechargé entre-temps) est ignorée. `each` est appelé après chaque
    /// opération appliquée.
    pub(crate) fn apply_mirror(&mut self, ops: &[MirrorOp], mut each: impl FnMut(&MirrorOp)) {
        let log = self.mirror_log.take();
        for op in ops {
            match op {
                MirrorOp::Insert { at, text } if *at <= self.rope.len_chars() => {
                    self.insert(*at, text);
                    each(op);
                }
                MirrorOp::Delete { start, end } if start < end && *end <= self.rope.len_chars() => {
                    self.delete(*start..*end);
                    each(op);
                }
                _ => {}
            }
        }
        self.mirror_log = log;
    }

    /// Active ou désactive le journal des modifications au format LSP (synchronisation d'un
    /// serveur de langage). Désactiver vide le journal.
    pub fn track_changes(&mut self, on: bool) {
        self.change_log = on.then(Vec::new);
    }

    /// Retire et renvoie les modifications faites depuis le dernier appel, dans l'ordre.
    pub fn take_changes(&mut self) -> Vec<TextChange> {
        self.change_log
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// (ligne, colonne en unités UTF-16) d'un offset de caractères : le format des positions LSP.
    #[must_use]
    pub fn lsp_position(&self, char_idx: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(char_idx);
        let line_start = self.rope.line_to_char(line);
        (
            line,
            self.rope.char_to_utf16_cu(char_idx) - self.rope.char_to_utf16_cu(line_start),
        )
    }

    /// Offset de caractères d'une position LSP `(ligne, colonne UTF-16)`, ramenée dans le texte.
    #[must_use]
    pub fn char_at_lsp_position(&self, line: usize, col16: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        let base = self.rope.char_to_utf16_cu(start);
        let line_end = self.line_end(line);
        let target = (base + col16).min(self.rope.char_to_utf16_cu(line_end));
        // `utf16_cu_to_char` arrondit vers le bas si la position tombe au milieu d'une paire.
        self.rope.utf16_cu_to_char(target).clamp(start, line_end)
    }

    /// (octet, (ligne, colonne en octets)) d'un offset de caractères.
    fn byte_and_point(&self, char_idx: usize) -> (usize, (usize, usize)) {
        let byte = self.rope.char_to_byte(char_idx);
        let line = self.rope.char_to_line(char_idx);
        (byte, (line, byte - self.rope.line_to_byte(line)))
    }

    /// Insère `text` à `char_idx`.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        if let Some(log) = self.mirror_log.as_mut() {
            log.push(MirrorOp::Insert {
                at: char_idx,
                text: text.to_owned(),
            });
        }
        if self.change_log.is_some() && !text.is_empty() {
            let p = self.lsp_position(char_idx);
            if let Some(log) = self.change_log.as_mut() {
                log.push(TextChange {
                    start: p,
                    end: p,
                    text: text.to_owned(),
                });
            }
        }
        if self.edit_log.is_some() && !text.is_empty() {
            let (start_byte, start_point) = self.byte_and_point(char_idx);
            let rows = text.matches('\n').count();
            let new_end_point = match text.rfind('\n') {
                Some(i) => (start_point.0 + rows, text.len() - (i + 1)),
                None => (start_point.0, start_point.1 + text.len()),
            };
            let edit = TextEdit {
                start_byte,
                old_end_byte: start_byte,
                new_end_byte: start_byte + text.len(),
                start_point,
                old_end_point: start_point,
                new_end_point,
            };
            if let Some(log) = self.edit_log.as_mut() {
                log.push(edit);
            }
        }
        self.rope.insert(char_idx, text);
        self.revision += 1;
    }

    /// Supprime la plage de caractères.
    pub fn delete(&mut self, range: Range<usize>) {
        if range.start < range.end {
            if let Some(log) = self.mirror_log.as_mut() {
                log.push(MirrorOp::Delete {
                    start: range.start,
                    end: range.end,
                });
            }
            if self.change_log.is_some() {
                let (start, end) = (self.lsp_position(range.start), self.lsp_position(range.end));
                if let Some(log) = self.change_log.as_mut() {
                    log.push(TextChange {
                        start,
                        end,
                        text: String::new(),
                    });
                }
            }
            if self.edit_log.is_some() {
                let (start_byte, start_point) = self.byte_and_point(range.start);
                let (old_end_byte, old_end_point) = self.byte_and_point(range.end);
                let edit = TextEdit {
                    start_byte,
                    old_end_byte,
                    new_end_byte: start_byte,
                    start_point,
                    old_end_point,
                    new_end_point: start_point,
                };
                if let Some(log) = self.edit_log.as_mut() {
                    log.push(edit);
                }
            }
            self.rope.remove(range);
            self.revision += 1;
        }
    }

    /// Remplace la plage par `text`.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let start = range.start;
        self.delete(range);
        if !text.is_empty() {
            self.insert(start, text);
        }
    }

    /// Remplace `[start, end)` par un fragment (chemin de l'historique et des multi-curseurs).
    pub(crate) fn replace_fragment(&mut self, start: usize, end: usize, frag: &Fragment) {
        self.delete(start..end);
        let mut at = start;
        frag.for_each_chunk(|chunk| {
            // `insert` tient le journal des modifications et la révision.
            if !chunk.is_empty() {
                self.insert(at, chunk);
                at += chunk.chars().count();
            }
        });
    }

    // --- Lecture -----------------------------------------------------------------------------

    /// Tranche de texte (sans copie).
    #[must_use]
    pub fn slice(&self, range: Range<usize>) -> RopeSlice<'_> {
        self.rope.slice(range)
    }

    /// Caractère à `char_idx`.
    #[must_use]
    pub fn char(&self, char_idx: usize) -> char {
        self.rope.char(char_idx)
    }

    /// Ligne `idx`, **saut de ligne inclus**.
    #[must_use]
    pub fn line(&self, idx: usize) -> RopeSlice<'_> {
        self.rope.line(idx)
    }

    /// Offset du premier caractère de la ligne.
    #[must_use]
    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line)
    }

    /// Offset de fin de ligne, **avant** le saut de ligne.
    #[must_use]
    pub fn line_end(&self, line: usize) -> usize {
        let start = self.rope.line_to_char(line);
        let slice = self.rope.line(line);
        let mut n = slice.len_chars();
        if n > 0 && slice.char(n - 1) == '\n' {
            n -= 1;
            if n > 0 && slice.char(n - 1) == '\r' {
                n -= 1;
            }
        }
        start + n
    }

    /// Longueur de la ligne en caractères, sans le saut de ligne.
    #[must_use]
    pub fn line_len(&self, line: usize) -> usize {
        self.line_end(line) - self.line_start(line)
    }

    // --- Conversions -------------------------------------------------------------------------

    /// Offset en octets d'un offset de caractères.
    #[must_use]
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.rope.char_to_byte(char_idx)
    }

    /// Offset de caractères d'un offset en octets (arrondi au caractère contenant).
    #[must_use]
    pub fn byte_to_char(&self, byte_idx: usize) -> usize {
        self.rope.byte_to_char(byte_idx)
    }

    /// Ligne contenant ce caractère.
    #[must_use]
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx)
    }

    /// (ligne, colonne) d'un offset ; la colonne est en caractères.
    #[must_use]
    pub fn char_to_line_col(&self, char_idx: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(char_idx);
        (line, char_idx - self.rope.line_to_char(line))
    }

    /// Offset d'une (ligne, colonne), la ligne et la colonne étant bornées à leur maximum valide
    /// (la colonne ne dépasse jamais la fin de ligne, saut de ligne exclu).
    #[must_use]
    pub fn line_col_to_char(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.len_lines() - 1);
        let start = self.line_start(line);
        start + col.min(self.line_len(line))
    }

    /// Offset en unités UTF-16 (LSP) d'un offset de caractères.
    #[must_use]
    pub fn char_to_utf16(&self, char_idx: usize) -> usize {
        self.rope.char_to_utf16_cu(char_idx)
    }

    /// Offset de caractères d'un offset en unités UTF-16.
    #[must_use]
    pub fn utf16_to_char(&self, utf16_idx: usize) -> usize {
        self.rope.utf16_cu_to_char(utf16_idx)
    }
}

impl fmt::Display for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_delete_replace() {
        let mut b = Buffer::from_text("hello world");
        b.insert(5, ",");
        assert_eq!(b.to_string(), "hello, world");
        b.delete(5..6);
        b.replace(0..5, "salut");
        assert_eq!(b.to_string(), "salut world");
        assert!(b.revision() >= 3);
    }

    #[test]
    fn lines_lf_and_crlf() {
        let b = Buffer::from_text("ab\r\ncd\nef");
        assert_eq!(b.len_lines(), 3);
        assert_eq!(b.line_start(1), 4);
        assert_eq!(b.line_end(0), 2);
        assert_eq!(b.line_end(1), 6);
        assert_eq!(b.line_end(2), 9);
        assert_eq!(b.line_len(0), 2);
        assert_eq!(b.line(0).to_string(), "ab\r\n");
    }

    #[test]
    fn trailing_newline_gives_empty_last_line() {
        let b = Buffer::from_text("a\n");
        assert_eq!(b.len_lines(), 2);
        assert_eq!(b.line_len(1), 0);
        assert_eq!(Buffer::new().len_lines(), 1);
    }

    #[test]
    fn lone_cr_and_unicode_breaks_are_not_lines() {
        let b = Buffer::from_text("a\rb\u{2028}c\u{0B}d");
        assert_eq!(b.len_lines(), 1);
    }

    #[test]
    fn conversions() {
        let b = Buffer::from_text("é😀\nxy");
        assert_eq!(b.len_chars(), 5);
        assert_eq!(b.len_bytes(), 2 + 4 + 1 + 2);
        assert_eq!(b.char_to_byte(2), 6);
        assert_eq!(b.byte_to_char(6), 2);
        assert_eq!(b.char_to_line_col(4), (1, 1));
        assert_eq!(
            b.line_col_to_char(1, 99),
            5,
            "colonne bornée à la fin de ligne"
        );
        assert_eq!(b.line_col_to_char(99, 0), 3, "ligne bornée");
        assert_eq!(b.char_to_utf16(2), 3, "l'emoji vaut 2 unités UTF-16");
        assert_eq!(b.utf16_to_char(3), 2);
    }

    #[test]
    fn journal_des_modifications_octets_et_points() {
        let mut b = Buffer::from_text(
            "héllo
wörld
",
        );
        // désactivé par défaut : rien n'est journalisé
        b.insert(0, "x");
        assert!(b.take_edits().is_empty());

        b.track_edits(true);
        // insertion sans saut de ligne, après « xhé » (1 + 1 + 2 octets) sur la ligne 0
        b.insert(3, "ab");
        let e = b.take_edits();
        assert_eq!(
            e,
            vec![TextEdit {
                start_byte: 4,
                old_end_byte: 4,
                new_end_byte: 6,
                start_point: (0, 4),
                old_end_point: (0, 4),
                new_end_point: (0, 6),
            }]
        );
        // insertion avec deux sauts de ligne : la colonne finale compte depuis le dernier
        b.insert(
            0, "a
bb
cc",
        );
        let e = b.take_edits();
        assert_eq!(e[0].new_end_point, (2, 2));
        assert_eq!(e[0].new_end_byte, 7);
        // suppression à cheval sur deux lignes
        let text = b.to_string();
        let start = text.chars().position(|c| c == 'w').unwrap() - 2;
        let end = start + 5;
        b.delete(start..end);
        let e = b.take_edits();
        assert_eq!(e.len(), 1);
        assert!(e[0].old_end_point.0 > e[0].start_point.0 || e[0].old_end_byte > e[0].start_byte);
        assert_eq!(e[0].new_end_byte, e[0].start_byte);
        assert_eq!(e[0].new_end_point, e[0].start_point);
        // désactiver vide le journal
        b.insert(0, "z");
        b.track_edits(false);
        assert!(b.take_edits().is_empty());
    }

    #[test]
    fn journal_au_format_lsp_en_utf16() {
        // « é » = 1 unité UTF-16, « 😀 » = 2 : les colonnes LSP ne sont ni des octets ni des
        // caractères.
        let mut b = Buffer::from_text("aé😀b\nsuite\n");
        b.track_changes(true);
        // insertion après « aé😀 » (colonne UTF-16 4), à la ligne 0
        b.insert(3, "XY");
        // suppression de « 😀 » (caractères 2..3 -> colonnes UTF-16 2..4)
        b.delete(2..3);
        // suppression à cheval sur deux lignes : de la fin de la ligne 0 au début de « sui »
        let text = b.to_string();
        let start = text.chars().position(|c| c == 'b').unwrap() + 1;
        b.delete(start..start + 4);
        let changes = b.take_changes();
        assert_eq!(
            changes,
            vec![
                TextChange {
                    start: (0, 4),
                    end: (0, 4),
                    text: "XY".into()
                },
                TextChange {
                    start: (0, 2),
                    end: (0, 4),
                    text: String::new()
                },
                TextChange {
                    start: (0, 5),
                    end: (1, 3),
                    text: String::new()
                },
            ]
        );
        // désactivé par défaut et après désactivation : rien
        b.insert(0, "z");
        b.track_changes(false);
        b.insert(0, "z");
        assert!(b.take_changes().is_empty());
    }

    #[test]
    fn positions_lsp_dans_les_deux_sens() {
        let b = Buffer::from_text("aé😀b\nsuite");
        // caractères : a0 é1 😀2 b3 saut4 s5… ; colonnes UTF-16 de la ligne 0 : 0 1 2 4 5
        assert_eq!(b.lsp_position(0), (0, 0));
        assert_eq!(b.lsp_position(2), (0, 2));
        assert_eq!(b.lsp_position(3), (0, 4), "« 😀 » compte pour 2 unités");
        assert_eq!(b.lsp_position(4), (0, 5), "fin de ligne, avant le saut");
        assert_eq!(b.lsp_position(5), (1, 0));
        for c in 0..=b.len_chars() {
            let (l, col) = b.lsp_position(c);
            assert_eq!(
                b.char_at_lsp_position(l, col),
                c,
                "aller-retour du caractère {c}"
            );
        }
        // hors du texte : ramené à la fin de la ligne / à la dernière ligne
        assert_eq!(b.char_at_lsp_position(0, 99), 4);
        assert_eq!(b.char_at_lsp_position(99, 0), 5);
        // au milieu d'une paire de substitution : arrondi vers le début du caractère
        assert_eq!(b.char_at_lsp_position(0, 3), 2);
    }

    #[test]
    fn la_somme_des_deltas_du_journal_egale_la_variation_de_taille() {
        let mut b = Buffer::from_text(
            "une ligne
avec é et 😀
",
        );
        b.track_edits(true);
        let before = b.len_bytes() as isize;
        let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for _ in 0..300 {
            let n = b.len_chars();
            if next() % 2 == 0 || n == 0 {
                let at = (next() % (n as u64 + 1)) as usize;
                let text = [
                    "a", "é", "😀", "
", "xy
z", "
",
                ][(next() % 6) as usize];
                b.insert(at, text);
            } else {
                let s = (next() % n as u64) as usize;
                let e = (s + (next() % 5) as usize).min(n);
                b.delete(s..e);
            }
        }
        let delta: isize = b
            .take_edits()
            .iter()
            .map(|e| e.new_end_byte as isize - e.old_end_byte as isize)
            .sum();
        assert_eq!(b.len_bytes() as isize - before, delta);
    }
}
