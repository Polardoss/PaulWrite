//! Déplacements du curseur : caractère (grapheme), mot, ligne, paragraphe, document.
//!
//! Fonctions pures sur un [`Buffer`] : elles prennent une position et en renvoient une autre.
//! Les colonnes sont comptées en caractères ; la largeur visuelle (tabulations, caractères larges)
//! relève du rendu.

use ropey::RopeSlice;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

use crate::buffer::Buffer;

/// Déplacement nommé (utilisé par les commandes de l'éditeur).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    ParagraphUp,
    ParagraphDown,
    DocStart,
    DocEnd,
}

/// Fin du grapheme qui commence à `pos` (ou `len` si `pos` est à la fin).
#[must_use]
pub fn next_grapheme(buf: &Buffer, pos: usize) -> usize {
    let len = buf.len_chars();
    if pos >= len {
        return len;
    }
    // Chemin rapide : deux caractères < U+0300 (et pas CR) ne forment jamais un seul grapheme.
    let c = buf.char(pos);
    if c < '\u{300}' && c != '\r' && (pos + 1 == len || buf.char(pos + 1) < '\u{300}') {
        return pos + 1;
    }
    next_boundary(buf.rope().slice(..), pos)
}

/// Début du grapheme qui précède `pos` (0 si `pos` est au début).
#[must_use]
pub fn prev_grapheme(buf: &Buffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let c = buf.char(pos - 1);
    if c < '\u{300}' && c != '\n' && (pos == 1 || buf.char(pos - 2) < '\u{300}') {
        return pos - 1;
    }
    prev_boundary(buf.rope().slice(..), pos)
}

// Recette officielle de la documentation de ropey pour GraphemeCursor sur des chunks.
fn next_boundary(slice: RopeSlice<'_>, char_idx: usize) -> usize {
    let byte_idx = slice.char_to_byte(char_idx);
    let mut gc = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    let (mut chunk, mut chunk_start, _, _) = slice.chunk_at_byte(byte_idx);
    loop {
        match gc.next_boundary(chunk, chunk_start) {
            Ok(None) => return slice.len_chars(),
            Ok(Some(n)) => return slice.byte_to_char(n),
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_start += chunk.len();
                let (c, s, _, _) = slice.chunk_at_byte(chunk_start);
                chunk = c;
                chunk_start = s;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (ctx, ctx_start, _, _) = slice.chunk_at_byte(n - 1);
                gc.provide_context(ctx, ctx_start);
            }
            Err(_) => unreachable!("offsets are valid by construction"),
        }
    }
}

fn prev_boundary(slice: RopeSlice<'_>, char_idx: usize) -> usize {
    let byte_idx = slice.char_to_byte(char_idx);
    let mut gc = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    let (mut chunk, mut chunk_start, _, _) = slice.chunk_at_byte(byte_idx.saturating_sub(1));
    loop {
        match gc.prev_boundary(chunk, chunk_start) {
            Ok(None) => return 0,
            Ok(Some(n)) => return slice.byte_to_char(n),
            Err(GraphemeIncomplete::PrevChunk) => {
                let (c, s, _, _) = slice.chunk_at_byte(chunk_start - 1);
                chunk = c;
                chunk_start = s;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (ctx, ctx_start, _, _) = slice.chunk_at_byte(n - 1);
                gc.provide_context(ctx, ctx_start);
            }
            Err(_) => unreachable!("offsets are valid by construction"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Punct,
    Space,
    Newline,
}

fn class(c: char) -> Class {
    if c == '\n' || c == '\r' {
        Class::Newline
    } else if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// Début du mot suivant : saute la séquence courante (mot ou ponctuation) puis les espaces ;
/// depuis un saut de ligne, s'arrête au début de la ligne suivante.
#[must_use]
pub fn next_word_start(buf: &Buffer, pos: usize) -> usize {
    let len = buf.len_chars();
    if pos >= len {
        return len;
    }
    let mut it = buf.rope().chars_at(pos).peekable();
    let first = class(*it.peek().expect("pos < len"));
    let mut i = pos;
    match first {
        Class::Newline => {
            // "\r\n" compte pour un seul saut de ligne.
            let c = it.next();
            i += 1;
            if c == Some('\r') && it.peek() == Some(&'\n') {
                i += 1;
            }
            return i;
        }
        Class::Word | Class::Punct => {
            while let Some(&c) = it.peek() {
                if class(c) != first {
                    break;
                }
                it.next();
                i += 1;
            }
        }
        Class::Space => {}
    }
    while let Some(&c) = it.peek() {
        if class(c) != Class::Space {
            break;
        }
        it.next();
        i += 1;
    }
    i
}

/// Début du mot précédent : saute les espaces vers l'arrière puis la séquence de même classe.
#[must_use]
pub fn prev_word_start(buf: &Buffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut chars = buf.rope().chars_at(pos);
    chars.reverse();
    let mut i = pos;
    let mut cur = None;
    for c in chars.by_ref() {
        if class(c) == Class::Space {
            i -= 1;
        } else {
            cur = Some(c);
            break;
        }
    }
    let Some(c) = cur else { return 0 };
    let first = class(c);
    if first == Class::Newline {
        // Saute un seul saut de ligne ("\r\n" compte pour un).
        i -= 1;
        if c == '\n' && i > 0 && buf.char(i - 1) == '\r' {
            i -= 1;
        }
        return i;
    }
    i -= 1;
    for c in chars {
        if class(c) != first {
            break;
        }
        i -= 1;
    }
    i
}

/// Début de la ligne (colonne 0).
#[must_use]
pub fn line_start(buf: &Buffer, pos: usize) -> usize {
    buf.line_start(buf.char_to_line(pos))
}

/// Fin de la ligne, avant le saut de ligne.
#[must_use]
pub fn line_end(buf: &Buffer, pos: usize) -> usize {
    buf.line_end(buf.char_to_line(pos))
}

/// Vrai si la ligne ne contient que des espaces.
fn is_blank_line(buf: &Buffer, line: usize) -> bool {
    let (s, e) = (buf.line_start(line), buf.line_end(line));
    buf.slice(s..e).chars().all(char::is_whitespace)
}

/// Prochaine ligne vide après le paragraphe courant (ou la fin du document).
#[must_use]
pub fn next_paragraph(buf: &Buffer, pos: usize) -> usize {
    let last = buf.len_lines() - 1;
    let mut l = buf.char_to_line(pos);
    while l < last && is_blank_line(buf, l) {
        l += 1;
    }
    while l < last && !is_blank_line(buf, l) {
        l += 1;
    }
    if is_blank_line(buf, l) && buf.line_start(l) > pos {
        buf.line_start(l)
    } else {
        buf.len_chars()
    }
}

/// Ligne vide précédant le paragraphe courant (ou le début du document).
#[must_use]
pub fn prev_paragraph(buf: &Buffer, pos: usize) -> usize {
    let mut l = buf.char_to_line(pos);
    if buf.line_start(l) == pos {
        l = l.saturating_sub(1);
    }
    while l > 0 && is_blank_line(buf, l) {
        l -= 1;
    }
    while l > 0 && !is_blank_line(buf, l) {
        l -= 1;
    }
    buf.line_start(l)
}

/// Déplacement vertical conservant la colonne visée. Au-delà de la première/dernière ligne, va au
/// début/à la fin du document.
#[must_use]
pub fn vertical(buf: &Buffer, pos: usize, goal_col: usize, down: bool) -> usize {
    let line = buf.char_to_line(pos);
    if down {
        if line + 1 >= buf.len_lines() {
            return buf.len_chars();
        }
        buf.line_col_to_char(line + 1, goal_col)
    } else {
        if line == 0 {
            return 0;
        }
        buf.line_col_to_char(line - 1, goal_col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Buffer {
        Buffer::from_text(s)
    }

    #[test]
    fn graphemes() {
        let buf = b("a\u{0301}b😀🇫🇷\r\nz");
        // a + combinant = 1 grapheme
        assert_eq!(next_grapheme(&buf, 0), 2);
        assert_eq!(next_grapheme(&buf, 2), 3);
        assert_eq!(next_grapheme(&buf, 3), 4);
        // drapeau = 2 caractères régionaux = 1 grapheme
        assert_eq!(next_grapheme(&buf, 4), 6);
        // CRLF = 1 grapheme
        assert_eq!(next_grapheme(&buf, 6), 8);
        assert_eq!(prev_grapheme(&buf, 8), 6);
        assert_eq!(prev_grapheme(&buf, 6), 4);
        assert_eq!(prev_grapheme(&buf, 2), 0);
        assert_eq!(next_grapheme(&buf, buf.len_chars()), buf.len_chars());
        assert_eq!(prev_grapheme(&buf, 0), 0);
    }

    #[test]
    fn word_motions() {
        let buf = b("foo  bar_baz, qux\nnext");
        assert_eq!(next_word_start(&buf, 0), 5);
        assert_eq!(next_word_start(&buf, 5), 12, "bar_baz est un seul mot");
        assert_eq!(
            next_word_start(&buf, 12),
            14,
            "la ponctuation est une séquence"
        );
        assert_eq!(next_word_start(&buf, 14), 17, "s'arrête à la fin de ligne");
        assert_eq!(next_word_start(&buf, 17), 18, "début de ligne suivante");
        assert_eq!(next_word_start(&buf, buf.len_chars()), buf.len_chars());
        assert_eq!(prev_word_start(&buf, 5), 0);
        assert_eq!(prev_word_start(&buf, 12), 5);
        assert_eq!(prev_word_start(&buf, 14), 12);
        assert_eq!(prev_word_start(&buf, 18), 17);
        assert_eq!(prev_word_start(&buf, 0), 0);
    }

    #[test]
    fn word_motion_crlf() {
        let buf = b("ab\r\ncd");
        assert_eq!(next_word_start(&buf, 2), 4);
        assert_eq!(prev_word_start(&buf, 4), 2);
    }

    #[test]
    fn line_motions() {
        let buf = b("abc\r\nde\n\nfgh");
        assert_eq!(line_start(&buf, 2), 0);
        assert_eq!(line_end(&buf, 2), 3);
        assert_eq!(line_start(&buf, 6), 5);
        assert_eq!(vertical(&buf, 2, 2, true), 7, "colonne bornée par 'de'");
        assert_eq!(vertical(&buf, 7, 2, false), 2);
        assert_eq!(vertical(&buf, 0, 0, false), 0);
        assert_eq!(vertical(&buf, 11, 3, true), buf.len_chars());
    }

    #[test]
    fn paragraph_motions() {
        let buf = b("a\nb\n\nc\nd\n\n\ne");
        assert_eq!(next_paragraph(&buf, 0), 4, "première ligne vide");
        assert_eq!(
            next_paragraph(&buf, 4),
            9,
            "saute les vides puis atteint le suivant"
        );
        assert_eq!(next_paragraph(&buf, 9), buf.len_chars());
        assert_eq!(prev_paragraph(&buf, buf.len_chars()), 10);
        assert_eq!(prev_paragraph(&buf, 10), 4);
        assert_eq!(prev_paragraph(&buf, 4), 0);
        assert_eq!(prev_paragraph(&buf, 0), 0);
    }
}
