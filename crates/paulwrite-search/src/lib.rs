//! Recherche dans un projet : liste de fichiers, recherche de texte (moteur de ripgrep) et fuzzy
//! finder (nucleo).
//!
//! Toutes les fonctions sont **bloquantes** et prévues pour tourner sur un worker : le thread UI
//! ne parcourt jamais un dossier. Les résultats sont envoyés par lots dans un canal pour que les
//! premiers apparaissent avant la fin de la recherche, et chaque recherche est annulable.

mod fuzzy;
mod text;
mod walk;

pub use fuzzy::{FuzzyFinder, FuzzyInjector};
pub use text::{search_text, Event, Match, SearchOptions, Summary, TextQuery};
pub use walk::{list_files, IndexOptions};
