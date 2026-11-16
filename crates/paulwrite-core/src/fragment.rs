//! Fragment de texte possédé, optimisé pour le chemin de saisie.
//!
//! Une frappe produit un fragment de quelques octets par curseur : on les stocke **en ligne**
//! (sans allocation). Les gros fragments (collages, suppressions de blocs) basculent sur un
//! [`Rope`], dont le clonage est en O(1).

use ropey::{Rope, RopeSlice};

const INLINE_CAP: usize = 22;
/// Au-delà, le fragment est stocké dans un rope plutôt que dans une `String`.
const ROPE_THRESHOLD: usize = 64 * 1024;

/// Texte possédé (immuable).
#[derive(Clone, Debug)]
pub enum Fragment {
    /// Texte vide.
    Empty,
    /// Petit texte, stocké sans allocation.
    Inline { len: u8, buf: [u8; INLINE_CAP] },
    /// Texte de taille moyenne.
    Heap(Box<str>),
    /// Gros texte.
    Rope(Box<Rope>),
}

impl Fragment {
    /// Fragment à partir d'un `&str`.
    #[must_use]
    pub fn from_text(s: &str) -> Self {
        if s.is_empty() {
            Self::Empty
        } else if s.len() <= INLINE_CAP {
            let mut buf = [0u8; INLINE_CAP];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            #[allow(clippy::cast_possible_truncation)] // len <= 22
            Self::Inline {
                len: s.len() as u8,
                buf,
            }
        } else if s.len() <= ROPE_THRESHOLD {
            Self::Heap(s.into())
        } else {
            Self::Rope(Box::new(Rope::from_str(s)))
        }
    }

    /// Fragment copié depuis une tranche de rope (O(log n) pour les gros fragments).
    #[must_use]
    pub fn from_slice(slice: RopeSlice<'_>) -> Self {
        let bytes = slice.len_bytes();
        if bytes == 0 {
            Self::Empty
        } else if bytes <= INLINE_CAP {
            let mut buf = [0u8; INLINE_CAP];
            let mut at = 0;
            for chunk in slice.chunks() {
                buf[at..at + chunk.len()].copy_from_slice(chunk.as_bytes());
                at += chunk.len();
            }
            #[allow(clippy::cast_possible_truncation)] // bytes <= 22
            Self::Inline {
                len: bytes as u8,
                buf,
            }
        } else if bytes <= ROPE_THRESHOLD {
            let mut s = String::with_capacity(bytes);
            for chunk in slice.chunks() {
                s.push_str(chunk);
            }
            Self::Heap(s.into_boxed_str())
        } else {
            Self::Rope(Box::new(Rope::from(slice)))
        }
    }

    /// Vrai si le fragment est vide.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Nombre de caractères.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Inline { .. } | Self::Heap(_) => self.small_str().chars().count(),
            Self::Rope(r) => r.len_chars(),
        }
    }

    /// Nombre d'octets UTF-8.
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Inline { len, .. } => usize::from(*len),
            Self::Heap(s) => s.len(),
            Self::Rope(r) => r.len_bytes(),
        }
    }

    fn small_str(&self) -> &str {
        match self {
            Self::Empty => "",
            Self::Inline { len, buf } => {
                std::str::from_utf8(&buf[..usize::from(*len)]).expect("inline fragment is UTF-8")
            }
            Self::Heap(s) => s,
            Self::Rope(_) => unreachable!("rope fragments have no contiguous str"),
        }
    }

    /// Appelle `f` sur chaque morceau contigu du texte, dans l'ordre.
    pub fn for_each_chunk(&self, mut f: impl FnMut(&str)) {
        match self {
            Self::Rope(r) => {
                for c in r.chunks() {
                    f(c);
                }
            }
            _ => {
                let s = self.small_str();
                if !s.is_empty() {
                    f(s);
                }
            }
        }
    }

    /// Concatène deux fragments.
    #[must_use]
    pub fn concat(&self, other: &Self) -> Self {
        if self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        let total = self.len_bytes() + other.len_bytes();
        if total <= ROPE_THRESHOLD {
            let mut s = String::with_capacity(total);
            self.for_each_chunk(|c| s.push_str(c));
            other.for_each_chunk(|c| s.push_str(c));
            Self::from_text(&s)
        } else {
            let mut r = self.to_rope();
            r.append(other.to_rope());
            Self::Rope(Box::new(r))
        }
    }

    fn to_rope(&self) -> Rope {
        match self {
            Self::Rope(r) => (**r).clone(),
            _ => Rope::from_str(self.small_str()),
        }
    }
}

impl std::fmt::Display for Fragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut res = Ok(());
        self.for_each_chunk(|c| {
            if res.is_ok() {
                res = f.write_str(c);
            }
        });
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_pick_the_right_representation() {
        assert!(matches!(Fragment::from_text(""), Fragment::Empty));
        assert!(matches!(
            Fragment::from_text("abc"),
            Fragment::Inline { .. }
        ));
        assert!(matches!(
            Fragment::from_text(&"x".repeat(100)),
            Fragment::Heap(_)
        ));
        assert!(matches!(
            Fragment::from_text(&"x".repeat(100_000)),
            Fragment::Rope(_)
        ));
    }

    #[test]
    fn roundtrip_and_concat() {
        let a = Fragment::from_text("héllo ");
        let b = Fragment::from_text("wörld");
        let c = a.concat(&b);
        assert_eq!(c.to_string(), "héllo wörld");
        assert_eq!(c.len_chars(), 11);
        assert_eq!(c.len_bytes(), 13);
        let big = Fragment::from_text(&"é".repeat(40_000)).concat(&Fragment::from_text("z"));
        assert!(matches!(big, Fragment::Rope(_)));
        assert_eq!(big.len_chars(), 40_001);
    }

    #[test]
    fn from_slice_matches_text() {
        let rope = Rope::from_str("a😀b and some longer text to leave the inline range");
        for (a, b) in [(0, 0), (0, 3), (1, 2), (0, 40)] {
            let f = Fragment::from_slice(rope.slice(a..b));
            assert_eq!(f.to_string(), rope.slice(a..b).to_string());
        }
    }
}
