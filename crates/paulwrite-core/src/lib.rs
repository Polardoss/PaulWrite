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

pub mod buffer;
pub mod document;
pub mod edit;
pub mod encoding;
pub mod error;
pub mod fragment;
pub mod history;
pub mod io;
pub mod line_ending;
pub mod motion;
pub mod selection;

pub use buffer::{Buffer, MirrorOp, TextChange, TextEdit};
pub use document::{Document, Replacement};
pub use edit::EditKind;
pub use encoding::{Encoding, EncodingKind};
pub use error::{Error, Result};
pub use history::History;
pub use line_ending::LineEnding;
pub use motion::Motion;
pub use selection::{Selection, SelectionSet};
