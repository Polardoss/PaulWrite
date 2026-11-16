//! Sélections et curseurs multiples.
//!
//! Invariants d'un [`SelectionSet`] (toujours vrais, vérifiés par proptest) :
//! - il contient au moins une sélection ;
//! - les sélections sont triées par position et **ne se chevauchent jamais** ;
//! - un curseur (sélection vide) collé à une autre sélection, ou deux curseurs identiques, sont
//!   fusionnés ; deux plages non vides qui ne font que se toucher restent distinctes ;
//! - l'index du curseur principal désigne toujours une sélection existante.

/// Une sélection : `anchor` est le point fixe, `head` le point mobile (le curseur affiché).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// Colonne visée pour les déplacements verticaux successifs (en caractères).
    pub goal_col: Option<usize>,
}

impl Selection {
    /// Curseur sans sélection.
    #[must_use]
    pub const fn caret(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
            goal_col: None,
        }
    }

    /// Sélection `anchor..head` (peut être inversée).
    #[must_use]
    pub const fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal_col: None,
        }
    }

    /// Début (le plus petit des deux points).
    #[must_use]
    pub const fn start(&self) -> usize {
        if self.anchor < self.head {
            self.anchor
        } else {
            self.head
        }
    }

    /// Fin (le plus grand des deux points).
    #[must_use]
    pub const fn end(&self) -> usize {
        if self.anchor > self.head {
            self.anchor
        } else {
            self.head
        }
    }

    /// Vrai pour un simple curseur.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Longueur en caractères.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end() - self.start()
    }

    /// Vrai si le point mobile est après le point fixe (ou si la sélection est vide).
    #[must_use]
    pub const fn is_forward(&self) -> bool {
        self.head >= self.anchor
    }
}

/// Deux sélections voisines (triées) doivent-elles être fusionnées ?
fn must_merge(a: &Selection, b: &Selection) -> bool {
    debug_assert!(a.start() <= b.start());
    b.start() < a.end() || (b.start() == a.end() && (a.is_empty() || b.is_empty()))
}

/// Ensemble trié et normalisé de sélections, avec un curseur principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSet {
    sels: Vec<Selection>,
    primary: usize,
}

impl SelectionSet {
    /// Ensemble d'un seul curseur.
    #[must_use]
    pub fn single(sel: Selection) -> Self {
        Self {
            sels: vec![sel],
            primary: 0,
        }
    }

    /// Construit un ensemble normalisé (tri + fusion). `primary` indexe `sels` *avant* tri ; un
    /// index hors bornes est ramené à la dernière sélection. Un vecteur vide donne un curseur en 0.
    #[must_use]
    pub fn from_vec(mut sels: Vec<Selection>, primary: usize) -> Self {
        if sels.is_empty() {
            return Self::single(Selection::caret(0));
        }
        let primary = primary.min(sels.len() - 1);
        let tag = sels[primary];
        let sorted = sels
            .windows(2)
            .all(|w| (w[0].start(), w[0].end()) <= (w[1].start(), w[1].end()));
        if !sorted {
            sels.sort_by_key(|s| (s.start(), s.end()));
        }
        let pidx = sels.iter().position(|s| *s == tag).unwrap_or(0);
        let mut set = Self {
            sels,
            primary: pidx,
        };
        set.merge_overlaps();
        set
    }

    fn merge_overlaps(&mut self) {
        if self.sels.len() < 2 {
            return;
        }
        let pidx = self.primary;
        let (mut w, mut new_primary) = (0usize, 0usize);
        let mut group_has_primary = pidx == 0;
        for i in 1..self.sels.len() {
            let cur = self.sels[i];
            if must_merge(&self.sels[w], &cur) {
                let prev = self.sels[w];
                // Orientation : celle du curseur principal s'il fait partie du groupe (le résultat
                // fusionné porte déjà son orientation si on l'a rencontré plus tôt).
                let forward = if i == pidx {
                    cur.is_forward()
                } else {
                    prev.is_forward()
                };
                let (s, e) = (prev.start().min(cur.start()), prev.end().max(cur.end()));
                self.sels[w] = if forward {
                    Selection::new(s, e)
                } else {
                    Selection::new(e, s)
                };
                if i == pidx {
                    group_has_primary = true;
                }
            } else {
                if group_has_primary {
                    new_primary = w;
                }
                w += 1;
                self.sels[w] = cur;
                group_has_primary = i == pidx;
            }
        }
        if group_has_primary {
            new_primary = w;
        }
        self.sels.truncate(w + 1);
        self.primary = new_primary;
    }

    /// Nombre de sélections (≥ 1).
    #[must_use]
    pub fn len(&self) -> usize {
        self.sels.len()
    }

    /// Toujours faux (un ensemble contient au moins un curseur) ; fourni pour la convention.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Les sélections, triées.
    #[must_use]
    pub fn as_slice(&self) -> &[Selection] {
        &self.sels
    }

    /// Itérateur sur les sélections, triées.
    pub fn iter(&self) -> std::slice::Iter<'_, Selection> {
        self.sels.iter()
    }

    /// Index du curseur principal.
    #[must_use]
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// Curseur principal.
    #[must_use]
    pub fn primary(&self) -> Selection {
        self.sels[self.primary]
    }

    /// Ajoute une sélection (fusionnée si elle en chevauche une) et la rend principale.
    pub fn push(&mut self, sel: Selection) {
        let mut v = std::mem::take(&mut self.sels);
        v.push(sel);
        let idx = v.len() - 1;
        *self = Self::from_vec(v, idx);
    }

    /// Applique `f` à chaque sélection puis renormalise.
    pub fn transform(&mut self, mut f: impl FnMut(Selection) -> Selection) {
        for s in &mut self.sels {
            *s = f(*s);
        }
        let v = std::mem::take(&mut self.sels);
        *self = Self::from_vec(v, self.primary);
    }

    /// Plus grand offset référencé (pour valider les bornes).
    #[must_use]
    pub fn max_pos(&self) -> usize {
        self.sels.last().map_or(0, Selection::end)
    }
}

impl<'a> IntoIterator for &'a SelectionSet {
    type Item = &'a Selection;
    type IntoIter = std::slice::Iter<'a, Selection>;
    fn into_iter(self) -> Self::IntoIter {
        self.sels.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[(usize, usize)]) -> SelectionSet {
        SelectionSet::from_vec(v.iter().map(|&(a, h)| Selection::new(a, h)).collect(), 0)
    }

    fn ranges(s: &SelectionSet) -> Vec<(usize, usize)> {
        s.iter().map(|x| (x.anchor, x.head)).collect()
    }

    #[test]
    fn sorts_input() {
        assert_eq!(ranges(&set(&[(10, 12), (0, 2)])), vec![(0, 2), (10, 12)]);
    }

    #[test]
    fn merges_overlaps_and_duplicates() {
        assert_eq!(ranges(&set(&[(0, 5), (3, 8)])), vec![(0, 8)]);
        assert_eq!(ranges(&set(&[(4, 4), (4, 4), (4, 4)])), vec![(4, 4)]);
        assert_eq!(ranges(&set(&[(0, 10), (2, 3), (4, 4)])), vec![(0, 10)]);
    }

    #[test]
    fn touching_ranges_stay_apart_but_caret_merges() {
        assert_eq!(ranges(&set(&[(0, 3), (3, 6)])), vec![(0, 3), (3, 6)]);
        assert_eq!(ranges(&set(&[(0, 3), (3, 3)])), vec![(0, 3)]);
        assert_eq!(ranges(&set(&[(3, 3), (3, 6)])), vec![(3, 6)]);
    }

    #[test]
    fn keeps_direction_of_primary() {
        // primaire = la 2e (inversée) : le résultat garde son orientation
        let s = SelectionSet::from_vec(vec![Selection::new(0, 4), Selection::new(6, 2)], 1);
        assert_eq!(ranges(&s), vec![(6, 0)]);
        assert_eq!(s.primary_index(), 0);
    }

    #[test]
    fn primary_follows_sorting_and_merging() {
        let s = SelectionSet::from_vec(
            vec![
                Selection::caret(20),
                Selection::caret(0),
                Selection::caret(10),
            ],
            0,
        );
        assert_eq!(s.primary().head, 20);
        assert_eq!(s.primary_index(), 2);
        let mut s = s;
        s.push(Selection::caret(10));
        assert_eq!(s.len(), 3);
        assert_eq!(s.primary().head, 10);
    }

    #[test]
    fn empty_input_gives_a_caret() {
        let s = SelectionSet::from_vec(vec![], 3);
        assert_eq!(s.len(), 1);
        assert_eq!(s.primary(), Selection::caret(0));
    }
}
