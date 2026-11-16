//! Encodages (UTF-8, UTF-16 LE/BE), détection et décodage en flux.
//!
//! Décodage/encodage écrits avec la seule `std` (pas de `encoding_rs`) : seuls UTF-8 et UTF-16 sont
//! requis. Un fichier qui n'est ni l'un ni l'autre donne [`Error::UnsupportedEncoding`] plutôt
//! qu'une conversion silencieusement destructive.

use crate::error::{Error, Result};

/// Nombre d'octets examinés pour détecter l'encodage.
pub const DETECT_SAMPLE: usize = 64 * 1024;

/// Famille d'encodage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodingKind {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Encodage d'un fichier, avec présence d'un BOM (conservé à la sauvegarde).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoding {
    pub kind: EncodingKind,
    pub bom: bool,
}

impl Encoding {
    /// UTF-8 sans BOM (défaut).
    pub const UTF8: Self = Self {
        kind: EncodingKind::Utf8,
        bom: false,
    };
    /// UTF-8 avec BOM.
    pub const UTF8_BOM: Self = Self {
        kind: EncodingKind::Utf8,
        bom: true,
    };
    /// UTF-16 little-endian avec BOM.
    pub const UTF16_LE_BOM: Self = Self {
        kind: EncodingKind::Utf16Le,
        bom: true,
    };
    /// UTF-16 big-endian avec BOM.
    pub const UTF16_BE_BOM: Self = Self {
        kind: EncodingKind::Utf16Be,
        bom: true,
    };

    /// Octets du BOM (vide si `bom == false`).
    #[must_use]
    pub const fn bom_bytes(self) -> &'static [u8] {
        if !self.bom {
            return &[];
        }
        match self.kind {
            EncodingKind::Utf8 => &[0xEF, 0xBB, 0xBF],
            EncodingKind::Utf16Le => &[0xFF, 0xFE],
            EncodingKind::Utf16Be => &[0xFE, 0xFF],
        }
    }

    /// Détecte l'encodage d'après le début du fichier (BOM, heuristique UTF-16, validité UTF-8).
    ///
    /// # Errors
    /// [`Error::UnsupportedEncoding`] si l'échantillon n'est ni UTF-16 plausible ni UTF-8 valide.
    pub fn detect(sample: &[u8]) -> Result<Self> {
        let sample = &sample[..sample.len().min(DETECT_SAMPLE)];
        if sample.starts_with(&[0xEF, 0xBB, 0xBF]) {
            return Ok(Self::UTF8_BOM);
        }
        if sample.starts_with(&[0xFF, 0xFE]) {
            return Ok(Self::UTF16_LE_BOM);
        }
        if sample.starts_with(&[0xFE, 0xFF]) {
            return Ok(Self::UTF16_BE_BOM);
        }
        if let Some(kind) = sniff_utf16_without_bom(sample) {
            return Ok(Self { kind, bom: false });
        }
        match std::str::from_utf8(sample) {
            Ok(_) => Ok(Self::UTF8),
            // Fin d'échantillon coupée au milieu d'un caractère : acceptable.
            Err(e) if e.error_len().is_none() => Ok(Self::UTF8),
            Err(_) => Err(Error::UnsupportedEncoding),
        }
    }

    /// Encode `text` et ajoute les octets à `out`.
    pub fn encode_into(self, text: &str, out: &mut Vec<u8>) {
        match self.kind {
            EncodingKind::Utf8 => out.extend_from_slice(text.as_bytes()),
            EncodingKind::Utf16Le => {
                out.reserve(text.len() * 2);
                for u in text.encode_utf16() {
                    out.extend_from_slice(&u.to_le_bytes());
                }
            }
            EncodingKind::Utf16Be => {
                out.reserve(text.len() * 2);
                for u in text.encode_utf16() {
                    out.extend_from_slice(&u.to_be_bytes());
                }
            }
        }
    }
}

/// Heuristique : du texte majoritairement ASCII en UTF-16 contient un octet sur deux nul.
fn sniff_utf16_without_bom(sample: &[u8]) -> Option<EncodingKind> {
    let pairs = sample.len() / 2;
    if pairs < 2 {
        return None;
    }
    let (mut zeros_even, mut zeros_odd) = (0usize, 0usize);
    for p in sample.chunks_exact(2) {
        zeros_even += usize::from(p[0] == 0);
        zeros_odd += usize::from(p[1] == 0);
    }
    if zeros_odd * 4 >= pairs && zeros_even * 8 < zeros_odd {
        Some(EncodingKind::Utf16Le)
    } else if zeros_even * 4 >= pairs && zeros_odd * 8 < zeros_even {
        Some(EncodingKind::Utf16Be)
    } else {
        None
    }
}

/// Décodeur incrémental : accepte des morceaux d'octets de taille arbitraire (coupés n'importe où,
/// même au milieu d'un caractère) et fournit du texte valide à un `sink`.
#[derive(Debug)]
pub struct Decoder {
    kind: EncodingKind,
    /// Octets d'un caractère UTF-8 incomplet en fin de morceau précédent.
    carry: [u8; 4],
    carry_len: usize,
    /// UTF-16 : octet impair en attente.
    odd_byte: Option<u8>,
    /// UTF-16 : substitut haut en attente de son substitut bas.
    pending_high: Option<u16>,
    units: Vec<u16>,
    text: String,
    /// Octets consommés (hors BOM), pour les messages d'erreur.
    consumed: u64,
}

impl Decoder {
    /// Nouveau décodeur pour cette famille d'encodage.
    #[must_use]
    pub fn new(kind: EncodingKind) -> Self {
        Self {
            kind,
            carry: [0; 4],
            carry_len: 0,
            odd_byte: None,
            pending_high: None,
            units: Vec::new(),
            text: String::new(),
            consumed: 0,
        }
    }

    /// Décode un morceau d'octets ; `sink` reçoit du texte valide (0, 1 ou plusieurs appels).
    ///
    /// # Errors
    /// Octets invalides pour l'encodage.
    pub fn feed(&mut self, input: &[u8], sink: &mut dyn FnMut(&str)) -> Result<()> {
        let base = self.consumed;
        self.consumed += input.len() as u64;
        match self.kind {
            EncodingKind::Utf8 => self.feed_utf8(input, base, sink),
            EncodingKind::Utf16Le => self.feed_utf16(input, false, base, sink),
            EncodingKind::Utf16Be => self.feed_utf16(input, true, base, sink),
        }
    }

    /// Termine le flux.
    ///
    /// # Errors
    /// [`Error::TruncatedInput`] si le flux s'arrête au milieu d'un caractère.
    pub fn finish(&self) -> Result<()> {
        if self.carry_len > 0 || self.odd_byte.is_some() || self.pending_high.is_some() {
            Err(Error::TruncatedInput)
        } else {
            Ok(())
        }
    }

    fn feed_utf8(&mut self, mut input: &[u8], base: u64, sink: &mut dyn FnMut(&str)) -> Result<()> {
        let orig_len = input.len();
        // 1. Compléter un caractère coupé par le morceau précédent.
        if self.carry_len > 0 {
            let width = utf8_width(self.carry[0]).ok_or(Error::InvalidUtf8 { offset: base })?;
            let need = width - self.carry_len;
            let take = need.min(input.len());
            self.carry[self.carry_len..self.carry_len + take].copy_from_slice(&input[..take]);
            self.carry_len += take;
            input = &input[take..];
            if self.carry_len < width {
                return Ok(()); // toujours incomplet, plus d'entrée
            }
            let s = std::str::from_utf8(&self.carry[..width])
                .map_err(|_| Error::InvalidUtf8 { offset: base })?;
            sink(s);
            self.carry_len = 0;
        }
        if input.is_empty() {
            return Ok(());
        }
        // 2. Cas courant : validation d'un bloc entier (chemin rapide de `std`).
        match std::str::from_utf8(input) {
            Ok(s) => {
                sink(s);
                Ok(())
            }
            Err(e) => {
                let valid = e.valid_up_to();
                if valid > 0 {
                    // Revalidation du préfixe : coût négligeable, uniquement quand un morceau
                    // se termine au milieu d'un caractère.
                    let s = std::str::from_utf8(&input[..valid]).expect("prefix is valid");
                    sink(s);
                }
                if e.error_len().is_none() {
                    let rest = &input[valid..];
                    self.carry[..rest.len()].copy_from_slice(rest);
                    self.carry_len = rest.len();
                    Ok(())
                } else {
                    let already = (orig_len - input.len()) as u64;
                    Err(Error::InvalidUtf8 {
                        offset: base + already + valid as u64,
                    })
                }
            }
        }
    }

    fn feed_utf16(
        &mut self,
        mut input: &[u8],
        big_endian: bool,
        base: u64,
        sink: &mut dyn FnMut(&str),
    ) -> Result<()> {
        self.units.clear();
        if let Some(h) = self.pending_high.take() {
            self.units.push(h);
        }
        let to_unit = |a: u8, b: u8| {
            if big_endian {
                u16::from_be_bytes([a, b])
            } else {
                u16::from_le_bytes([a, b])
            }
        };
        if let Some(first) = self.odd_byte.take() {
            if let Some((&second, rest)) = input.split_first() {
                self.units.push(to_unit(first, second));
                input = rest;
            } else {
                self.odd_byte = Some(first);
                return Ok(());
            }
        }
        let pairs = input.chunks_exact(2);
        if let [b] = pairs.remainder() {
            self.odd_byte = Some(*b);
        }
        self.units.extend(pairs.map(|p| to_unit(p[0], p[1])));
        // Un substitut haut en fin de morceau attend son substitut bas.
        if let Some(&last) = self.units.last() {
            if (0xD800..0xDC00).contains(&last) {
                self.pending_high = Some(last);
                self.units.pop();
            }
        }
        self.text.clear();
        for r in char::decode_utf16(self.units.iter().copied()) {
            match r {
                Ok(c) => self.text.push(c),
                Err(_) => return Err(Error::InvalidUtf16 { offset: base }),
            }
        }
        if !self.text.is_empty() {
            sink(&self.text);
        }
        Ok(())
    }
}

/// Largeur d'un caractère UTF-8 d'après son premier octet.
fn utf8_width(first: u8) -> Option<usize> {
    match first {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_chunked(kind: EncodingKind, bytes: &[u8], chunk: usize) -> Result<String> {
        let mut d = Decoder::new(kind);
        let mut out = String::new();
        for c in bytes.chunks(chunk) {
            d.feed(c, &mut |s| out.push_str(s))?;
        }
        d.finish()?;
        Ok(out)
    }

    #[test]
    fn detects_boms_and_plain() {
        assert_eq!(
            Encoding::detect(b"\xEF\xBB\xBFhi").unwrap(),
            Encoding::UTF8_BOM
        );
        assert_eq!(
            Encoding::detect(b"\xFF\xFEh\0").unwrap(),
            Encoding::UTF16_LE_BOM
        );
        assert_eq!(
            Encoding::detect(b"\xFE\xFF\0h").unwrap(),
            Encoding::UTF16_BE_BOM
        );
        assert_eq!(
            Encoding::detect("héllo".as_bytes()).unwrap(),
            Encoding::UTF8
        );
        assert_eq!(Encoding::detect(b"").unwrap(), Encoding::UTF8);
    }

    #[test]
    fn detects_utf16_without_bom() {
        let mut le = Vec::new();
        Encoding {
            kind: EncodingKind::Utf16Le,
            bom: false,
        }
        .encode_into("hello world", &mut le);
        assert_eq!(Encoding::detect(&le).unwrap().kind, EncodingKind::Utf16Le);
        let mut be = Vec::new();
        Encoding {
            kind: EncodingKind::Utf16Be,
            bom: false,
        }
        .encode_into("hello world", &mut be);
        assert_eq!(Encoding::detect(&be).unwrap().kind, EncodingKind::Utf16Be);
    }

    #[test]
    fn rejects_latin1() {
        assert!(matches!(
            Encoding::detect(b"caf\xE9 au lait"),
            Err(Error::UnsupportedEncoding)
        ));
    }

    #[test]
    fn utf8_split_at_every_boundary() {
        let text = "aé€😀z";
        for chunk in 1..=text.len() {
            assert_eq!(
                decode_chunked(EncodingKind::Utf8, text.as_bytes(), chunk).unwrap(),
                text
            );
        }
    }

    #[test]
    fn utf8_errors() {
        assert!(matches!(
            decode_chunked(EncodingKind::Utf8, b"ab\xFFcd", 2),
            Err(Error::InvalidUtf8 { .. })
        ));
        assert!(matches!(
            decode_chunked(EncodingKind::Utf8, b"ab\xE2\x82", 100),
            Err(Error::TruncatedInput)
        ));
    }

    #[test]
    fn utf16_split_at_every_boundary() {
        let text = "aé€😀z";
        for kind in [EncodingKind::Utf16Le, EncodingKind::Utf16Be] {
            let mut bytes = Vec::new();
            Encoding { kind, bom: false }.encode_into(text, &mut bytes);
            for chunk in 1..=bytes.len() {
                assert_eq!(
                    decode_chunked(kind, &bytes, chunk).unwrap(),
                    text,
                    "{kind:?}/{chunk}"
                );
            }
        }
    }

    #[test]
    fn utf16_lone_surrogate_is_an_error() {
        let bytes = [0x00, 0xD8, 0x41, 0x00]; // U+D800 puis 'A' (LE)
        assert!(matches!(
            decode_chunked(EncodingKind::Utf16Le, &bytes, 100),
            Err(Error::InvalidUtf16 { .. })
        ));
    }
}
