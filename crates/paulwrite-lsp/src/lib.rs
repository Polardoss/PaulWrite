//! Client LSP (Language Server Protocol).
//!
//! Un thread dédié ([`spawn`]) lance les serveurs de langage (`rust-analyzer`, `pylsp`…) et parle
//! JSON-RPC avec eux par leurs tuyaux standard. L'application lui envoie des [`Request`]s (document
//! ouvert, modifié, fermé ; complétion, survol, définition) et reçoit des [`Event`]s (diagnostics,
//! résultats). Rien de tout cela ne passe par le thread d'interface : un serveur lent ou en panne
//! ne bloque jamais l'éditeur.
//!
//! Positions : le protocole compte les colonnes en **unités UTF-16** ; le journal
//! [`paulwrite_core::TextChange`] du core les fournit directement.

mod client;
mod frame;
mod uri;

pub use client::{
    spawn, CompletionItem, Diagnostic, Event, Location, Request, ServerConfig, Severity,
};
pub use frame::{read_message, write_message};
pub use uri::{path_key, path_to_uri, uri_to_path};
