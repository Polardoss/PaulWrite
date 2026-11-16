//! Coloration syntaxique incrémentale (tree-sitter), sur thread dédié.
//!
//! Principe :
//! - un arbre syntaxique par document, mis à jour **incrémentalement** à chaque modification
//!   (les `TextEdit` du core sont transmis à tree-sitter, qui ne reparse que le nécessaire) ;
//! - la coloration n'est calculée que pour la **fenêtre visible** (plus une marge), jamais pour
//!   tout le fichier ;
//! - tout se passe sur un thread dédié : le thread UI envoie des requêtes et reçoit des plages
//!   colorées ([`Response`]), sans jamais analyser lui-même.

mod highlight;
mod language;
mod worker;

pub use highlight::{kind_name, Highlighter, Span, KIND_COUNT};
pub use language::Language;
pub use worker::{spawn, Request, Response};

/// Index de couleur d'un [`Span`] ; 0 = texte ordinaire (pas de coloration).
pub type Kind = u8;

/// Catégories de coloration (indices de palette).
pub mod kind {
    use super::Kind;
    pub const COMMENT: Kind = 1;
    pub const STRING: Kind = 2;
    pub const NUMBER: Kind = 3;
    pub const KEYWORD: Kind = 4;
    pub const FUNCTION: Kind = 5;
    pub const TYPE: Kind = 6;
    pub const CONSTANT: Kind = 7;
    pub const PROPERTY: Kind = 8;
    pub const OPERATOR: Kind = 9;
    pub const PUNCTUATION: Kind = 10;
    pub const ATTRIBUTE: Kind = 11;
    pub const TAG: Kind = 12;
    pub const ESCAPE: Kind = 13;
    pub const BUILTIN: Kind = 14;
}
