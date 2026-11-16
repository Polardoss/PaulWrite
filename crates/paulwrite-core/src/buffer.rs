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

/// Contenu texte d'un document.
#[derive(Clone, Default)]
pub struct Buffer {
    rope: Rope,
    revision: u64,
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
        }
    }

    /// Buffer construit à partir d'un rope existant (ex. chargement de fichier).
    #[must_use]
    pub fn from_rope(rope: Rope) -> Self {
        Self { rope, revision: 0 }
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

    /// Insère `text` à `char_idx`.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.rope.insert(char_idx, text);
        self.revision += 1;
    }

    /// Supprime la plage de caractères.
    pub fn delete(&mut self, range: Range<usize>) {
        if range.start < range.end {
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
            self.rope.insert(at, chunk);
            at += chunk.chars().count();
        });
        if !frag.is_empty() {
            self.revision += 1;
        }
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
}
