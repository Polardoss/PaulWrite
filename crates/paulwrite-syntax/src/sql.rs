//! Les `\'` du SQL.
//!
//! La grammaire SQL suit le standard : une quote s'échappe en la doublant (`'it''s'`), et seules
//! les chaînes `E'…'` de PostgreSQL acceptent `\'`. MySQL et MariaDB, eux, écrivent `'it\'s'` : pour
//! la grammaire, la chaîne se ferme au `\'` et tout le reste du fichier est mal coloré.
//!
//! Comme on ne peut pas changer la grammaire, on change ce qu'elle lit : [`escaped_quotes`] repère
//! les quotes échappées (celles qui suivent un `\` dans une chaîne), et l'analyseur reçoit à leur
//! place un octet neutre **de même longueur** ([`NEUTRAL`]) : la chaîne ne se ferme plus, et
//! aucune position du document ne bouge.

use ropey::Rope;

/// Octet donné à l'analyseur à la place d'une quote échappée.
pub(crate) const NEUTRAL: u8 = b'_';

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    /// Dans une chaîne ou un identifiant guillemeté ; l'octet est son délimiteur.
    Quoted(u8),
    LineComment,
    BlockComment,
}

/// Décalages (en octets, triés) des quotes échappées par un `\` à l'intérieur d'une chaîne `'…'`
/// ou `"…"`. Les commentaires (`-- …`, `/* … */`) et les identifiants `` `…` `` (où le `\` n'a pas
/// de sens spécial) sont reconnus pour ne pas confondre une apostrophe de commentaire avec une
/// chaîne. Un seul passage, sans allocation autre que le résultat : de l'ordre de la milliseconde
/// par mégaoctet.
pub(crate) fn escaped_quotes(rope: &Rope) -> Vec<usize> {
    let mut out = Vec::new();
    let mut state = State::Normal;
    let mut escaped = false;
    let mut prev = 0u8;
    let mut pos = 0usize;
    for chunk in rope.chunks() {
        for &b in chunk.as_bytes() {
            match state {
                State::Normal => match b {
                    b'\'' | b'"' | b'`' => {
                        state = State::Quoted(b);
                        escaped = false;
                        prev = 0;
                    }
                    b'-' if prev == b'-' => {
                        state = State::LineComment;
                        prev = 0;
                    }
                    b'*' if prev == b'/' => {
                        state = State::BlockComment;
                        prev = 0;
                    }
                    _ => prev = b,
                },
                State::Quoted(q) => {
                    if escaped {
                        escaped = false;
                        if b == q {
                            out.push(pos);
                        }
                    } else if b == b'\\' && q != b'`' {
                        escaped = true;
                    } else if b == q {
                        // fermeture (une quote doublée `''` referme puis rouvre : même effet)
                        state = State::Normal;
                        prev = 0;
                    }
                }
                State::LineComment => {
                    if b == b'\n' {
                        state = State::Normal;
                        prev = 0;
                    }
                }
                State::BlockComment => {
                    if b == b'/' && prev == b'*' {
                        state = State::Normal;
                        prev = 0;
                    } else {
                        prev = b;
                    }
                }
            }
            pos += 1;
        }
    }
    out
}

/// Positions de `old` après les modifications `edits` (appliquées dans l'ordre) ; celles qui
/// tombent à l'intérieur d'une zone modifiée disparaissent.
pub(crate) fn shift_through(old: &[usize], edits: &[paulwrite_core::TextEdit]) -> Vec<usize> {
    let mut out = old.to_vec();
    for e in edits {
        let delta = e.new_end_byte as isize - e.old_end_byte as isize;
        out.retain_mut(|o| {
            if *o < e.start_byte {
                true
            } else if *o >= e.old_end_byte {
                *o = (*o as isize + delta) as usize;
                true
            } else {
                false
            }
        });
    }
    out
}

/// Éléments de `a` ou de `b` (listes triées) qui ne sont pas dans les deux.
pub(crate) fn symmetric_difference(a: &[usize], b: &[usize]) -> Vec<usize> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() || j < b.len() {
        match (a.get(i), b.get(j)) {
            (Some(&x), Some(&y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(&x), Some(&y)) if x < y => {
                out.push(x);
                i += 1;
            }
            (Some(&x), None) => {
                out.push(x);
                i += 1;
            }
            (_, Some(&y)) => {
                out.push(y);
                j += 1;
            }
            (None, None) => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Vec<usize> {
        escaped_quotes(&Rope::from_str(text))
    }

    #[test]
    fn quotes_echappees() {
        // 'it\'s' : la quote en 5e position (octet 5) est échappée
        assert_eq!(at(r"'it\'s'"), [4]);
        assert_eq!(at(r#""a\"b""#), [3]);
        // pas dans une chaîne : rien ; quote doublée : rien ; antislash doublé : la quote ferme
        assert_eq!(at(r"SELECT \' FROM"), Vec::<usize>::new());
        assert_eq!(at("'it''s'"), Vec::<usize>::new());
        assert_eq!(at(r"'a\\' 'b'"), Vec::<usize>::new());
        // un `\'` après un antislash doublé est échappé
        assert_eq!(at(r"'a\\\'b'"), [5]);
        // deux chaînes, une échappée chacune
        assert_eq!(at(r"'a\'' , 'b\''"), [3, 11]);
    }

    #[test]
    fn commentaires_et_identifiants() {
        // l'apostrophe d'un commentaire n'ouvre pas de chaîne
        assert_eq!(at("-- it's\n'a\\'b'"), [11]);
        assert_eq!(at("/* it's */ 'a\\'b'"), [14]);
        // dans des backticks le `\` n'échappe rien
        assert_eq!(at(r"`a\` 'b\'c'"), [8]);
        // un seul tiret n'est pas un commentaire
        assert_eq!(at("a - 'b\\'c'"), [7]);
    }

    #[test]
    fn plusieurs_morceaux_de_rope() {
        // l'état traverse les frontières entre morceaux de la rope
        let mut text = String::from("'");
        text.push_str(&"x".repeat(5000));
        text.push_str("\\'");
        text.push_str(&"y".repeat(5000));
        text.push('\'');
        assert_eq!(at(&text), [5002]);
    }

    #[test]
    fn decalage_et_difference() {
        use paulwrite_core::TextEdit;
        let edit = |s, oe, ne| TextEdit {
            start_byte: s,
            old_end_byte: oe,
            new_end_byte: ne,
            start_point: (0, s),
            old_end_point: (0, oe),
            new_end_point: (0, ne),
        };
        // insertion avant : décalage ; modification qui recouvre : disparaît ; après : inchangé
        assert_eq!(shift_through(&[10, 20, 30], &[edit(5, 5, 8)]), [13, 23, 33]);
        assert_eq!(shift_through(&[10, 20, 30], &[edit(18, 22, 19)]), [10, 27]);
        assert_eq!(shift_through(&[10, 20], &[edit(25, 25, 30)]), [10, 20]);
        assert_eq!(symmetric_difference(&[1, 3, 5], &[3, 4, 5, 9]), [1, 4, 9]);
        assert!(symmetric_difference(&[], &[]).is_empty());
    }
}
