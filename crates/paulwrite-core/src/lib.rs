//! Cœur de PaulWrite : buffer, sélections multiples, undo/redo, encodages, I/O fichiers.
//!
//! Aucune dépendance UI. Rien ici ne s'exécute sur le thread UI de l'application : les fonctions
//! d'I/O de [`io`] sont bloquantes et destinées à des workers.
//!
//! Vue d'ensemble :
//! - [`Buffer`] : texte (rope), conversions byte/char/ligne-colonne/UTF-16 ;
//! - [`SelectionSet`] : curseurs et sélections multiples, toujours normalisés ;
//! - [`Document`] : façade d'édition (multi-curseurs, déplacements, undo/redo transactionnel) ;
//! - [`io`] : chargement mmap/flux, sauvegarde atomique ; [`Encoding`], [`LineEnding`].

pub mod error;
pub mod line_ending;
pub mod encoding;
pub mod fragment;
pub mod buffer;
pub mod selection;
pub mod motion;

pub use buffer::Buffer;
pub use encoding::{Encoding, EncodingKind};
pub use error::{Error, Result};
pub use line_ending::LineEnding;
pub use motion::Motion;
pub use selection::{Selection, SelectionSet};
