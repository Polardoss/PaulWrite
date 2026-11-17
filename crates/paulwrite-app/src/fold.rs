//! Repli de code. Les régions repliables se déduisent de l'**indentation** (indépendant du
//! langage, sans analyse syntaxique) : une ligne est repliable si la prochaine ligne non vide est
//! plus indentée ; sa région va jusqu'à la dernière ligne plus indentée qui la suit. La ligne de
//! fermeture (`}`) reste visible, comme dans VS Code.
//!
//! Une région repliée est mémorisée en **octets** (de la fin de la ligne d'en-tête à la fin de sa
//! dernière ligne masquée) et suit les modifications du texte.

use paulwrite_core::{Buffer, TextEdit};

/// Nombre de lignes vides franchies au plus pour trouver la ligne suivante.
const BLANK_LOOKAHEAD: usize = 64;
/// Nombre de lignes examinées au plus pour trouver la fin d'une région.
const SCAN_LIMIT: usize = 200_000;
/// Nombre de caractères examinés au plus pour mesurer l'indentation d'une ligne.
const INDENT_SCAN: usize = 512;

/// Indentation (en colonnes, tabulation = 4) de `line`, `None` si la ligne est vide ou blanche.
#[must_use]
pub fn indent_of(buf: &Buffer, line: usize) -> Option<usize> {
    // octets : seuls l'espace, la tabulation et les fins de ligne (ASCII) comptent
    let mut col = 0;
    for b in buf.line(line).bytes().take(INDENT_SCAN) {
        match b {
            b' ' => col += 1,
            b'\t' => col = (col / 4 + 1) * 4,
            b'\n' | b'\r' => return None,
            _ => return Some(col),
        }
    }
    None
}

/// La ligne est le début d'une région repliable.
#[must_use]
pub fn foldable(buf: &Buffer, line: usize) -> bool {
    let Some(indent) = indent_of(buf, line) else {
        return false;
    };
    let total = buf.len_lines();
    (line + 1..total.min(line + 2 + BLANK_LOOKAHEAD))
        .find_map(|l| indent_of(buf, l))
        .is_some_and(|next| next > indent)
}

/// Dernière ligne (comprise) de la région qui commence à `line`, `None` si elle n'est pas
/// repliable.
#[must_use]
pub fn region_end(buf: &Buffer, line: usize) -> Option<usize> {
    let indent = indent_of(buf, line)?;
    let total = buf.len_lines();
    let mut last = None;
    for l in line + 1..total.min(line + 1 + SCAN_LIMIT) {
        match indent_of(buf, l) {
            None => {}
            Some(i) if i > indent => last = Some(l),
            Some(_) => break,
        }
    }
    last
}

/// Régions repliées d'un document : plages d'octets `[début, fin)` triées et disjointes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folds {
    ranges: Vec<(usize, usize)>,
}

impl Folds {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn clear(&mut self) {
        self.ranges.clear();
    }

    /// Replie la région qui commence à la ligne `header` et finit à `last` (comprise). Les
    /// régions déjà repliées à l'intérieur sont absorbées ; sans effet si elle est déjà masquée.
    pub fn fold(&mut self, buf: &Buffer, header: usize, last: usize) {
        let start = buf.char_to_byte(buf.line_end(header));
        let end = buf.char_to_byte(buf.line_end(last));
        if start >= end || self.ranges.iter().any(|&(s, e)| s <= start && end <= e) {
            return;
        }
        self.ranges.retain(|&(s, e)| !(start <= s && e <= end));
        let at = self.ranges.partition_point(|&(s, _)| s < start);
        self.ranges.insert(at, (start, end));
    }

    /// Remplace toutes les régions par `regions` : (ligne d'en-tête, dernière ligne masquée),
    /// triées et disjointes. Linéaire, contrairement à `fold` appelé en boucle (Fold All).
    pub fn replace_all(&mut self, buf: &Buffer, regions: &[(usize, usize)]) {
        self.ranges.clear();
        self.ranges.extend(regions.iter().filter_map(|&(h, last)| {
            let start = buf.char_to_byte(buf.line_end(h));
            let end = buf.char_to_byte(buf.line_end(last));
            (start < end).then_some((start, end))
        }));
    }

    /// Déplie la région dont l'en-tête est la ligne `header` (ou qui masque cette ligne).
    /// Renvoie `true` si une région a été dépliée.
    pub fn unfold_line(&mut self, buf: &Buffer, header: usize) -> bool {
        let before = self.ranges.len();
        self.ranges.retain(|&(s, e)| {
            let (h, last) = lines_of(buf, s, e);
            !(h == header || (h < header && header <= last))
        });
        self.ranges.len() != before
    }

    /// Déplie les régions qui masquent le caractère `pos` (le curseur y est entré).
    pub fn reveal_char(&mut self, buf: &Buffer, pos: usize) -> bool {
        let byte = buf.char_to_byte(pos);
        let before = self.ranges.len();
        // la fin d'une région masquée est la fin de sa dernière ligne : le curseur peut s'y
        // trouver (fin de ligne masquée), donc bornes comprises
        self.ranges.retain(|&(s, e)| !(s < byte && byte <= e));
        self.ranges.len() != before
    }

    pub fn unfold_all(&mut self) -> bool {
        let any = !self.ranges.is_empty();
        self.ranges.clear();
        any
    }

    /// Suit une modification du texte : les régions situées après se décalent, celles qui
    /// chevauchent la modification sont dépliées.
    pub fn apply_edit(&mut self, e: &TextEdit) {
        if self.ranges.is_empty() {
            return;
        }
        let delta = e.new_end_byte as isize - e.old_end_byte as isize;
        self.ranges.retain_mut(|(s, end)| {
            if e.old_end_byte <= *s {
                *s = (*s as isize + delta) as usize;
                *end = (*end as isize + delta) as usize;
                true
            } else {
                e.start_byte >= *end
            }
        });
    }

    /// Les régions sous forme de (ligne d'en-tête, dernière ligne masquée), triées.
    pub fn lines(&self, buf: &Buffer, out: &mut Vec<(usize, usize)>) {
        out.clear();
        out.extend(self.ranges.iter().map(|&(s, e)| lines_of(buf, s, e)));
    }
}

fn lines_of(buf: &Buffer, start: usize, end: usize) -> (usize, usize) {
    let rope = buf.rope();
    let max = rope.len_bytes();
    (
        rope.byte_to_line(start.min(max)),
        rope.byte_to_line(end.min(max)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "fn main() {\n    let a = 1;\n    if a {\n        b();\n\n        c();\n    }\n}\nfn other() {}\n";

    fn buf() -> Buffer {
        Buffer::from_text(CODE)
    }

    #[test]
    fn indentation_des_lignes() {
        let b = buf();
        assert_eq!(indent_of(&b, 0), Some(0));
        assert_eq!(indent_of(&b, 1), Some(4));
        assert_eq!(indent_of(&b, 3), Some(8));
        assert_eq!(indent_of(&b, 4), None, "ligne vide");
        assert_eq!(indent_of(&Buffer::from_text("\t x"), 0), Some(5));
        assert_eq!(indent_of(&Buffer::from_text("   \n"), 0), None, "blanche");
    }

    #[test]
    fn lignes_repliables() {
        let b = buf();
        let f: Vec<bool> = (0..b.len_lines()).map(|l| foldable(&b, l)).collect();
        assert_eq!(
            f,
            [true, false, true, false, false, false, false, false, false, false]
        );
    }

    #[test]
    fn fin_d_une_region() {
        let b = buf();
        // main : jusqu'à la dernière ligne indentée (la `}` reste visible)
        assert_eq!(region_end(&b, 0), Some(6));
        // if : jusqu'à `c();` (la ligne vide au milieu est dans la région)
        assert_eq!(region_end(&b, 2), Some(5));
        assert_eq!(region_end(&b, 1), None);
        assert_eq!(region_end(&b, 8), None);
    }

    #[test]
    fn region_jusqu_a_la_fin_du_fichier() {
        let b = Buffer::from_text("a:\n  b\n  c");
        assert_eq!(region_end(&b, 0), Some(2));
        let b = Buffer::from_text("a:\n  b\n\n\n");
        assert_eq!(
            region_end(&b, 0),
            Some(1),
            "les lignes vides de fin sont exclues"
        );
    }

    #[test]
    fn replier_et_lister_les_lignes() {
        let b = buf();
        let mut f = Folds::default();
        f.fold(&b, 2, 5);
        let mut v = Vec::new();
        f.lines(&b, &mut v);
        assert_eq!(v, [(2, 5)]);
        // replier main absorbe la région interne
        f.fold(&b, 0, 6);
        f.lines(&b, &mut v);
        assert_eq!(v, [(0, 6)]);
        assert_eq!(f.len(), 1);
        // replier une région déjà masquée ne fait rien
        f.fold(&b, 2, 5);
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn tout_replier_d_un_coup() {
        let b = buf();
        let mut f = Folds::default();
        f.fold(&b, 2, 5);
        f.replace_all(&b, &[(0, 6)]);
        let mut v = Vec::new();
        f.lines(&b, &mut v);
        assert_eq!(v, [(0, 6)]);
        // une région vide est ignorée
        f.replace_all(&b, &[(8, 8)]);
        assert!(f.is_empty());
    }

    #[test]
    fn deplier() {
        let b = buf();
        let mut f = Folds::default();
        f.fold(&b, 0, 6);
        assert!(!f.unfold_line(&b, 8), "autre ligne : rien");
        assert!(f.unfold_line(&b, 3), "une ligne masquée déplie sa région");
        assert!(f.is_empty());
        f.fold(&b, 0, 6);
        assert!(f.unfold_line(&b, 0), "l'en-tête déplie");
        f.fold(&b, 2, 5);
        f.fold(&b, 0, 1);
        assert!(f.unfold_all());
        assert!(!f.unfold_all());
    }

    #[test]
    fn le_curseur_dans_une_region_la_deplie() {
        let b = buf();
        let mut f = Folds::default();
        f.fold(&b, 2, 5);
        // fin de l'en-tête (ligne 2) : visible, la région reste repliée
        let header_end = b.line_end(2);
        assert!(!f.reveal_char(&b, header_end));
        assert_eq!(f.len(), 1);
        // dans le texte masqué
        assert!(f.reveal_char(&b, b.line_start(3) + 2));
        assert!(f.is_empty());
        // fin de la dernière ligne masquée
        f.fold(&b, 2, 5);
        assert!(f.reveal_char(&b, b.line_end(5)));
    }

    #[test]
    fn les_regions_suivent_les_modifications() {
        let mut b = buf();
        let mut f = Folds::default();
        b.track_edits(true);
        f.fold(&b, 2, 5);
        let mut v = Vec::new();
        // insertion d'une ligne avant : la région descend d'une ligne
        b.insert(0, "// titre\n");
        for e in b.take_edits() {
            f.apply_edit(&e);
        }
        f.lines(&b, &mut v);
        assert_eq!(v, [(3, 6)]);
        // frappe à la fin de l'en-tête : la région reste repliée
        let end = b.line_end(3);
        b.insert(end, " // x");
        for e in b.take_edits() {
            f.apply_edit(&e);
        }
        f.lines(&b, &mut v);
        assert_eq!(v, [(3, 6)]);
        // frappe dans le texte masqué : la région est dépliée
        let inside = b.line_start(5) + 1;
        b.insert(inside, "z");
        for e in b.take_edits() {
            f.apply_edit(&e);
        }
        assert!(f.is_empty());
        // modification après la région : elle ne bouge pas
        f.fold(&b, 3, 6);
        let after = b.len_chars();
        b.insert(after, "fin\n");
        for e in b.take_edits() {
            f.apply_edit(&e);
        }
        f.lines(&b, &mut v);
        assert_eq!(v, [(3, 6)]);
    }
}
