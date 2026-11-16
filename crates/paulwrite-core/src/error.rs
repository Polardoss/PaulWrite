//! Erreurs du core (sans dépendance externe).

use std::{fmt, io};

/// Erreur de chargement/sauvegarde ou de décodage.
#[derive(Debug)]
pub enum Error {
    /// Erreur d'entrée/sortie du système.
    Io(io::Error),
    /// Octets UTF-8 invalides à cet offset (en octets, dans le contenu décodé).
    InvalidUtf8 { offset: u64 },
    /// Séquence UTF-16 invalide (substitut isolé) à cet offset en octets.
    InvalidUtf16 { offset: u64 },
    /// Le flux se termine au milieu d'un caractère.
    TruncatedInput,
    /// Encodage non pris en charge (ni UTF-8 ni UTF-16 valide).
    UnsupportedEncoding,
    /// Opération annulée par le callback de progression.
    Cancelled,
}

/// Alias de résultat du crate.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "erreur d'E/S : {e}"),
            Self::InvalidUtf8 { offset } => write!(f, "UTF-8 invalide à l'octet {offset}"),
            Self::InvalidUtf16 { offset } => write!(f, "UTF-16 invalide à l'octet {offset}"),
            Self::TruncatedInput => write!(f, "fichier tronqué au milieu d'un caractère"),
            Self::UnsupportedEncoding => {
                write!(
                    f,
                    "encodage non pris en charge (seuls UTF-8 et UTF-16 le sont)"
                )
            }
            Self::Cancelled => write!(f, "opération annulée"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
