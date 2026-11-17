//! Carte d'affichage : quelles lignes (et quels segments de ligne) occupent les rangées de la
//! vue, compte tenu du retour à la ligne automatique et des régions repliées.
//!
//! Une **rangée** est un segment d'une ligne : la ligne entière sans retour à la ligne, ou un
//! morceau de ligne avec. Les lignes masquées par un repli n'ont aucune rangée.

use paulwrite_core::Buffer;
use paulwrite_render::FoldMark;

use crate::{fold, wrap};

/// Au-delà de cette longueur (en caractères), une ligne n'est plus coupée : la découper coûterait
/// un parcours complet à chaque image. Elle reste lisible par défilement horizontal.
pub const WRAP_LINE_MAX: usize = 200_000;

/// Position dans la vue : une ligne du document et l'indice de son segment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub sub: usize,
}

/// Ce qu'une rangée affiche.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowInfo {
    /// Ligne du document ; égale au nombre de lignes pour une rangée au-delà de la fin.
    pub line: usize,
    /// Offset (en caractères) du début de la ligne dans le document.
    pub line_start: usize,
    /// Plage de caractères de la ligne affichée dans la rangée.
    pub start: usize,
    pub end: usize,
    /// Premier segment de la ligne (porte le numéro et la flèche de repli).
    pub first: bool,
    /// Dernier segment de la ligne (porte la fin de ligne et le repère « … »).
    pub last: bool,
    /// Flèche de la gouttière.
    pub fold: FoldMark,
    /// La ligne est l'en-tête d'une région repliée : « … » est ajouté à son dernier segment.
    pub folded: bool,
}

/// Parcours de la vue. Emprunte le texte, les régions repliées et deux tampons réutilisables.
#[derive(Debug)]
pub struct Walker<'a> {
    pub buf: &'a Buffer,
    /// Largeur de retour à la ligne en colonnes ; `None` : lignes non coupées.
    pub cols: Option<usize>,
    /// Régions repliées : (ligne d'en-tête, dernière ligne masquée), triées.
    pub folds: &'a [(usize, usize)],
    pub scratch: &'a mut String,
    pub breaks: &'a mut Vec<usize>,
    /// Calculer les flèches ▼ des lignes repliables (coûteux : seulement pendant le survol de la
    /// gouttière). Les flèches ▶ des lignes repliées sont toujours calculées.
    pub fold_marks: bool,
}

impl Walker<'_> {
    /// Dernière ligne masquée si `line` est l'en-tête d'une région repliée.
    #[must_use]
    pub fn fold_end(&self, line: usize) -> Option<usize> {
        let i = self.folds.partition_point(|&(h, _)| h < line);
        self.folds
            .get(i)
            .filter(|&&(h, _)| h == line)
            .map(|&(_, e)| e)
    }

    /// En-tête de la région repliée qui masque `line`, s'il y en a une.
    #[must_use]
    pub fn hidden_by(&self, line: usize) -> Option<usize> {
        let i = self.folds.partition_point(|&(h, _)| h < line);
        i.checked_sub(1)
            .map(|k| self.folds[k])
            .filter(|&(_, e)| line <= e)
            .map(|(h, _)| h)
    }

    /// Ligne affichée qui suit `line` (peut valoir le nombre de lignes : plus rien à afficher).
    #[must_use]
    pub fn next_line(&self, line: usize) -> usize {
        self.fold_end(line).map_or(line + 1, |e| e + 1)
    }

    /// Ligne affichée qui précède `line`.
    #[must_use]
    pub fn prev_line(&self, line: usize) -> Option<usize> {
        let p = line.checked_sub(1)?;
        Some(self.hidden_by(p).unwrap_or(p))
    }

    /// Calcule les segments de `line` : `self.breaks` reçoit le début (en caractères) de chacun,
    /// le premier valant 0.
    pub fn segments(&mut self, line: usize) {
        let len = if self.cols.is_some() {
            self.geometry(line).1
        } else {
            0
        };
        self.segments_of_len(line, len);
    }

    /// Offset du début de `line` et sa longueur en caractères sans fin de ligne (une seule
    /// lecture du texte : cette fonction est appelée pour chaque rangée de chaque image).
    #[must_use]
    pub fn geometry(&self, line: usize) -> (usize, usize) {
        let rope = self.buf.rope();
        let start = rope.line_to_char(line);
        let slice = rope.line(line);
        let mut n = slice.len_chars();
        if n > 0 && slice.char(n - 1) == '\n' {
            n -= 1;
            if n > 0 && slice.char(n - 1) == '\r' {
                n -= 1;
            }
        }
        (start, n)
    }

    /// Comme [`segments`](Self::segments), la longueur de la ligne étant déjà connue.
    fn segments_of_len(&mut self, line: usize, len: usize) {
        self.breaks.clear();
        self.breaks.push(0);
        let Some(cols) = self.cols else {
            return;
        };
        if len <= 1 || len > WRAP_LINE_MAX {
            return;
        }
        self.scratch.clear();
        for chunk in self.buf.line(line).chunks() {
            self.scratch.push_str(chunk);
        }
        while self.scratch.ends_with(['\n', '\r']) {
            self.scratch.pop();
        }
        wrap::breaks(self.scratch, cols, self.breaks);
    }

    /// Nombre de rangées de `line`.
    pub fn seg_count(&mut self, line: usize) -> usize {
        self.segments(line);
        self.breaks.len()
    }

    /// Segment de `line` qui contient la colonne `col` (un caractère de coupure appartient au
    /// segment qu'il commence).
    pub fn segment_of(&mut self, line: usize, col: usize) -> usize {
        self.segments(line);
        self.breaks.partition_point(|&s| s <= col) - 1
    }

    /// Avance de `n` rangées au plus ; renvoie la position atteinte et le nombre de rangées
    /// parcourues.
    pub fn advance(&mut self, mut pos: Pos, n: usize) -> (Pos, usize) {
        let total = self.buf.len_lines();
        let mut moved = 0;
        while moved < n {
            let segs = self.seg_count(pos.line);
            if pos.sub + 1 < segs {
                pos.sub += 1;
            } else {
                let next = self.next_line(pos.line);
                if next >= total {
                    break;
                }
                pos = Pos { line: next, sub: 0 };
            }
            moved += 1;
        }
        (pos, moved)
    }

    /// Recule de `n` rangées au plus.
    pub fn retreat(&mut self, mut pos: Pos, n: usize) -> (Pos, usize) {
        let mut moved = 0;
        while moved < n {
            if pos.sub > 0 {
                pos.sub -= 1;
            } else {
                let Some(prev) = self.prev_line(pos.line) else {
                    break;
                };
                let segs = self.seg_count(prev);
                pos = Pos {
                    line: prev,
                    sub: segs - 1,
                };
            }
            moved += 1;
        }
        (pos, moved)
    }

    /// Ramène une position valide : ligne visible et segment existant.
    pub fn normalize(&mut self, pos: Pos) -> Pos {
        let total = self.buf.len_lines();
        let line = pos.line.min(total.saturating_sub(1));
        let line = self.hidden_by(line).unwrap_or(line);
        let segs = self.seg_count(line);
        let sub = if line == pos.line {
            pos.sub.min(segs - 1)
        } else {
            0
        };
        Pos { line, sub }
    }

    /// Remplit `out` avec les `rows` rangées qui suivent `first`. Sans retour à la ligne, chaque
    /// ligne occupe une rangée qui montre `window` caractères à partir de `hscroll`.
    pub fn build(
        &mut self,
        first: Pos,
        rows: usize,
        hscroll: usize,
        window: usize,
        out: &mut Vec<RowInfo>,
    ) {
        out.clear();
        let total = self.buf.len_lines();
        let (mut line, mut sub) = (first.line, first.sub);
        while out.len() < rows {
            if line >= total {
                out.push(RowInfo {
                    line: total,
                    first: true,
                    last: true,
                    ..RowInfo::default()
                });
                continue;
            }
            let (line_start, len) = self.geometry(line);
            self.segments_of_len(line, len);
            let end_of_fold = self.fold_end(line);
            let mark = if end_of_fold.is_some() {
                FoldMark::Closed
            } else if self.fold_marks && fold::foldable(self.buf, line) {
                FoldMark::Open
            } else {
                FoldMark::None
            };
            let n = self.breaks.len();
            if self.cols.is_some() {
                for s in sub.min(n - 1)..n {
                    if out.len() >= rows {
                        break;
                    }
                    out.push(RowInfo {
                        line,
                        line_start,
                        start: self.breaks[s],
                        end: self.breaks.get(s + 1).copied().unwrap_or(len),
                        first: s == 0,
                        last: s + 1 == n,
                        fold: if s == 0 { mark } else { FoldMark::None },
                        folded: end_of_fold.is_some() && s + 1 == n,
                    });
                }
            } else {
                let a = hscroll.min(len);
                let b = len.min(a + window);
                out.push(RowInfo {
                    line,
                    line_start,
                    start: a,
                    end: b,
                    first: true,
                    last: b == len,
                    fold: mark,
                    folded: end_of_fold.is_some() && b == len,
                });
            }
            sub = 0;
            line = end_of_fold.map_or(line + 1, |e| e + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ctx {
        buf: Buffer,
        folds: Vec<(usize, usize)>,
        scratch: String,
        breaks: Vec<usize>,
    }

    impl Ctx {
        fn new(text: &str) -> Self {
            Self {
                buf: Buffer::from_text(text),
                folds: Vec::new(),
                scratch: String::new(),
                breaks: Vec::new(),
            }
        }
        fn walker(&mut self, cols: Option<usize>) -> Walker<'_> {
            Walker {
                buf: &self.buf,
                cols,
                folds: &self.folds,
                scratch: &mut self.scratch,
                breaks: &mut self.breaks,
                fold_marks: true,
            }
        }
    }

    fn p(line: usize, sub: usize) -> Pos {
        Pos { line, sub }
    }

    #[test]
    fn sans_retour_a_la_ligne_une_rangee_par_ligne() {
        let mut c = Ctx::new("aaaa bbbb cccc\nx\n");
        let mut w = c.walker(None);
        assert_eq!(w.seg_count(0), 1);
        assert_eq!(w.advance(p(0, 0), 1), (p(1, 0), 1));
    }

    #[test]
    fn avancer_et_reculer_a_travers_les_segments() {
        // 3 segments pour la ligne 0, 1 pour la ligne 1, 2 pour la ligne 2
        let mut c = Ctx::new("aaaa bbbb cccc\nx\naaaa bbbb\n");
        let mut w = c.walker(Some(5));
        assert_eq!(w.seg_count(0), 3);
        assert_eq!(w.seg_count(1), 1);
        assert_eq!(w.seg_count(2), 2);
        assert_eq!(w.advance(p(0, 0), 2), (p(0, 2), 2));
        assert_eq!(w.advance(p(0, 0), 3), (p(1, 0), 3));
        assert_eq!(w.advance(p(0, 0), 4), (p(2, 0), 4));
        assert_eq!(w.advance(p(0, 0), 5), (p(2, 1), 5));
        // fin du document : on s'arrête sur la dernière rangée de la dernière ligne (la ligne
        // vide finale compte, elle suit le dernier saut de ligne)
        let (end, moved) = w.advance(p(0, 0), 100);
        assert_eq!(end, p(3, 0));
        assert_eq!(moved, 6);
        // retour en arrière : exactement l'inverse
        assert_eq!(w.retreat(end, 6), (p(0, 0), 6));
        assert_eq!(w.retreat(p(2, 0), 1), (p(1, 0), 1));
        assert_eq!(w.retreat(p(1, 0), 1), (p(0, 2), 1));
        assert_eq!(w.retreat(p(0, 0), 3), (p(0, 0), 0));
    }

    #[test]
    fn les_lignes_repliees_sont_sautees() {
        let mut c = Ctx::new("a\n b\n c\nd\ne\n");
        c.folds = vec![(0, 2)];
        let mut w = c.walker(None);
        assert_eq!(w.fold_end(0), Some(2));
        assert_eq!(w.fold_end(1), None);
        assert_eq!(w.hidden_by(1), Some(0));
        assert_eq!(w.hidden_by(2), Some(0));
        assert_eq!(w.hidden_by(3), None);
        assert_eq!(w.hidden_by(0), None);
        assert_eq!(w.next_line(0), 3);
        assert_eq!(w.prev_line(3), Some(0));
        assert_eq!(w.prev_line(0), None);
        assert_eq!(w.advance(p(0, 0), 2), (p(4, 0), 2));
        assert_eq!(w.retreat(p(4, 0), 2), (p(0, 0), 2));
        // une position sur une ligne masquée revient à l'en-tête
        assert_eq!(w.normalize(p(2, 5)), p(0, 0));
        assert_eq!(w.normalize(p(99, 0)), p(5, 0));
    }

    #[test]
    fn construction_des_rangees() {
        let mut c = Ctx::new("fn a() {\n    x();\n}\nbbbb cccc dddd\n");
        c.folds = vec![(0, 1)];
        let mut rows = Vec::new();
        c.walker(Some(5)).build(p(0, 0), 6, 0, 100, &mut rows);
        // ligne 0 repliée : deux segments (« fn » puis « a() { »), puis les lignes 2 et 3
        let lines: Vec<_> = rows.iter().map(|r| (r.line, r.start, r.end)).collect();
        assert_eq!(
            lines,
            [
                (0, 0, 3),
                (0, 3, 8),
                (2, 0, 1),
                (3, 0, 5),
                (3, 5, 10),
                (3, 10, 14)
            ]
        );
        // flèche et repère de repli : premier / dernier segment de la ligne repliée
        assert_eq!(rows[0].fold, FoldMark::Closed);
        assert!(!rows[0].folded && rows[1].folded);
        assert_eq!(rows[1].fold, FoldMark::None);
        assert!(rows[0].first && !rows[0].last && rows[1].last);
        // la ligne 2 (« } ») n'est pas repliable ; la 3 non plus
        assert_eq!(rows[2].fold, FoldMark::None);
    }

    #[test]
    fn rangees_au_dela_de_la_fin_et_depart_au_milieu_d_une_ligne() {
        let mut c = Ctx::new("aaaa bbbb cccc\n");
        let mut rows = Vec::new();
        c.walker(Some(5)).build(p(0, 1), 5, 0, 100, &mut rows);
        let lines: Vec<_> = rows.iter().map(|r| (r.line, r.start, r.end)).collect();
        // ligne 0 : segments 1 et 2, puis la ligne vide finale (ligne 1), puis le vide
        assert_eq!(lines[0], (0, 5, 10));
        assert_eq!(lines[1], (0, 10, 14));
        assert_eq!(lines[2].0, 1);
        assert_eq!(
            rows[3].line, 2,
            "au-delà de la fin : égal au nombre de lignes"
        );
        assert_eq!(rows[4].line, 2);
    }

    #[test]
    fn sans_retour_a_la_ligne_la_fenetre_suit_le_defilement_horizontal() {
        let mut c = Ctx::new("0123456789abcdef\nxy\n");
        let mut rows = Vec::new();
        c.walker(None).build(p(0, 0), 2, 4, 6, &mut rows);
        assert_eq!((rows[0].start, rows[0].end, rows[0].last), (4, 10, false));
        // ligne courte : fenêtre vide, au bout de la ligne
        assert_eq!((rows[1].start, rows[1].end, rows[1].last), (2, 2, true));
        c.walker(None).build(p(0, 0), 1, 0, 100, &mut rows);
        assert_eq!((rows[0].start, rows[0].end, rows[0].last), (0, 16, true));
    }

    #[test]
    fn segment_d_une_colonne() {
        let mut c = Ctx::new("aaaa bbbb cccc\n");
        let mut w = c.walker(Some(5));
        assert_eq!(w.segment_of(0, 0), 0);
        assert_eq!(w.segment_of(0, 4), 0);
        assert_eq!(
            w.segment_of(0, 5),
            1,
            "le début d'un segment lui appartient"
        );
        assert_eq!(w.segment_of(0, 14), 2, "fin de ligne : dernier segment");
    }

    #[test]
    fn lignes_geantes_non_coupees() {
        let long = "x ".repeat(WRAP_LINE_MAX);
        let mut c = Ctx::new(&format!("{long}\nb\n"));
        let mut w = c.walker(Some(10));
        assert_eq!(w.seg_count(0), 1, "au-delà de la limite : une seule rangée");
    }
}
