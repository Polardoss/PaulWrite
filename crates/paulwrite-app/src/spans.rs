//! Plages colorées d'un document, tenues à jour entre deux réponses du thread de coloration.
//!
//! Quand l'utilisateur tape, la réponse du worker arrive quelques millisecondes plus tard : en
//! attendant, on **décale** les plages existantes selon les modifications, pour que les couleurs
//! ne « sautent » pas d'une image à l'autre.

use std::ops::Range;

use paulwrite_core::TextEdit;
use paulwrite_syntax::Span;

#[derive(Debug, Default)]
pub struct SpanCache {
    range: Range<usize>,
    spans: Vec<Span>,
}

/// Position d'un point après une modification.
fn adjust_point(p: usize, e: &TextEdit) -> usize {
    if p <= e.start_byte {
        p
    } else if p >= e.old_end_byte {
        p + e.new_end_byte - e.old_end_byte
    } else {
        // Le point était dans la zone remplacée : on le ramène au début de la modification.
        e.start_byte
    }
}

impl SpanCache {
    /// Remplace le contenu : `range` est la plage d'octets couverte par `spans`.
    pub fn set(&mut self, range: Range<usize>, spans: Vec<Span>) {
        self.range = range;
        self.spans = spans;
    }

    pub fn clear(&mut self) {
        self.range = 0..0;
        self.spans.clear();
    }

    /// Plage d'octets couverte.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// La plage `r` est entièrement couverte.
    #[must_use]
    pub fn covers(&self, r: &Range<usize>) -> bool {
        self.range.start <= r.start && r.end <= self.range.end
    }

    /// Plages colorées qui rencontrent `[start, end)` (vide hors de la zone couverte).
    #[must_use]
    pub fn overlapping(&self, start: usize, end: usize) -> &[Span] {
        if start >= end || start < self.range.start || end > self.range.end {
            return &[];
        }
        let first = self.spans.partition_point(|s| s.end <= start);
        let last = self.spans.partition_point(|s| s.start < end);
        if first < last {
            &self.spans[first..last]
        } else {
            &[]
        }
    }

    /// Décale les plages selon une modification (à appliquer dans l'ordre des modifications).
    pub fn apply_edit(&mut self, e: &TextEdit) {
        self.range = adjust_point(self.range.start, e)..adjust_point(self.range.end, e);
        let delta = e.new_end_byte as isize - e.old_end_byte as isize;
        self.spans.retain_mut(|s| {
            if s.end <= e.start_byte {
                true
            } else if s.start >= e.old_end_byte {
                s.start = (s.start as isize + delta) as usize;
                s.end = (s.end as isize + delta) as usize;
                true
            } else if s.start <= e.start_byte && s.end >= e.old_end_byte {
                // la modification est à l'intérieur de la plage : elle grandit ou rétrécit
                s.end = (s.end as isize + delta) as usize;
                s.end > s.start
            } else {
                false // chevauchement partiel : on attend la réponse du worker
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, old_end: usize, new_end: usize) -> TextEdit {
        TextEdit {
            start_byte: start,
            old_end_byte: old_end,
            new_end_byte: new_end,
            start_point: (0, start),
            old_end_point: (0, old_end),
            new_end_point: (0, new_end),
        }
    }

    fn span(start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            kind: 4,
        }
    }

    #[test]
    fn recherche_par_chevauchement() {
        let mut c = SpanCache::default();
        c.set(0..100, vec![span(0, 2), span(10, 15), span(40, 50)]);
        assert_eq!(c.overlapping(0, 100).len(), 3);
        assert_eq!(c.overlapping(12, 20), &[span(10, 15)]);
        assert_eq!(c.overlapping(15, 40), &[] as &[Span]);
        assert_eq!(c.overlapping(49, 60), &[span(40, 50)]);
        // hors de la zone couverte : rien (on ne devine pas)
        assert!(c.overlapping(90, 120).is_empty());
        assert!(c.covers(&(10..90)) && !c.covers(&(10..120)));
    }

    #[test]
    fn frappe_avant_dans_et_apres_une_plage() {
        let mut c = SpanCache::default();
        c.set(0..100, vec![span(10, 15), span(40, 50)]);
        // insertion de 3 octets avant les deux plages
        c.apply_edit(&edit(5, 5, 8));
        assert_eq!(c.overlapping(0, 100), &[span(13, 18), span(43, 53)]);
        assert_eq!(c.range(), 0..103);
        // insertion à l'intérieur de la première : elle grandit
        c.apply_edit(&edit(15, 15, 17));
        assert_eq!(c.overlapping(0, 103), &[span(13, 20), span(45, 55)]);
        // suppression qui englobe partiellement la première : elle disparaît
        c.apply_edit(&edit(18, 25, 18));
        assert_eq!(c.overlapping(0, 90), &[span(38, 48)]);
        // suppression de toute la plage restante : elle disparaît aussi
        c.apply_edit(&edit(30, 60, 30));
        assert!(c.overlapping(0, 60).is_empty());
    }

    #[test]
    fn insertion_exactement_au_debut_d_une_plage_la_repousse() {
        let mut c = SpanCache::default();
        c.set(0..50, vec![span(10, 15)]);
        c.apply_edit(&edit(10, 10, 11));
        assert_eq!(c.overlapping(0, 50), &[span(11, 16)]);
    }
}
