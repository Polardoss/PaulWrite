//! Fins de ligne (LF / CRLF).
//!
//! Le contenu du buffer est conservé tel quel (un fichier CRLF reste CRLF dans le rope) : aucune
//! réécriture à l'ouverture ni à la sauvegarde. Le style détecté sert aux *nouvelles* insertions.

/// Nombre d'octets examinés pour détecter le style dominant.
const DETECT_SAMPLE: usize = 64 * 1024;

/// Style de fin de ligne.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n`
    #[default]
    Lf,
    /// `\r\n`
    CrLf,
}

impl LineEnding {
    /// Représentation textuelle.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }

    /// Détecte le style dominant dans les premiers 64 Ko d'un texte ASCII-compatible (UTF-8
    /// décodé). En cas d'égalité ou d'absence de saut de ligne : [`LineEnding::Lf`].
    #[must_use]
    pub fn detect(sample: &[u8]) -> Self {
        let mut s = Sniffer::default();
        s.feed(sample);
        s.result()
    }
}

/// Détecteur incrémental : donne le même résultat quel que soit le découpage du texte en morceaux
/// (un `\r\n` coupé entre deux morceaux est bien compté comme CRLF).
#[derive(Clone, Copy, Debug, Default)]
pub struct Sniffer {
    crlf: usize,
    lf: usize,
    prev: u8,
    seen: usize,
}

impl Sniffer {
    /// Examine un morceau de texte UTF-8 ; ignore ce qui dépasse les 64 premiers Ko cumulés.
    pub fn feed(&mut self, text: &[u8]) {
        let room = DETECT_SAMPLE.saturating_sub(self.seen);
        for &b in text.iter().take(room) {
            if b == b'\n' {
                if self.prev == b'\r' {
                    self.crlf += 1;
                } else {
                    self.lf += 1;
                }
            }
            self.prev = b;
        }
        self.seen += text.len().min(room);
    }

    /// Style dominant ; égalité ou absence de saut de ligne : [`LineEnding::Lf`].
    #[must_use]
    pub fn result(&self) -> LineEnding {
        if self.crlf > self.lf {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dominant_style() {
        assert_eq!(LineEnding::detect(b"a\r\nb\r\nc\n"), LineEnding::CrLf);
        assert_eq!(LineEnding::detect(b"a\nb\nc\r\n"), LineEnding::Lf);
        assert_eq!(LineEnding::detect(b""), LineEnding::Lf);
        assert_eq!(LineEnding::detect(b"no newline"), LineEnding::Lf);
        assert_eq!(
            LineEnding::detect(b"a\r\nb\n"),
            LineEnding::Lf,
            "égalité -> LF"
        );
    }

    #[test]
    fn sniffer_is_independent_of_chunking() {
        let text = b"a\r\nb\r\nc\r\nd\n";
        for chunk in 1..=text.len() {
            let mut s = Sniffer::default();
            for c in text.chunks(chunk) {
                s.feed(c);
            }
            assert_eq!(s.result(), LineEnding::CrLf, "chunk={chunk}");
        }
    }
}
