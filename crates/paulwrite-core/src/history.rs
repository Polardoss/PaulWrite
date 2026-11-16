//! Historique undo/redo transactionnel avec groupement des frappes.
//!
//! Le core n'utilise pas l'horloge système : l'appelant fournit `now_ms` (millisecondes depuis un
//! point d'origine quelconque mais monotone), ce qui rend les tests déterministes.

use crate::edit::{try_merge, EditKind, Transaction};

/// Délai maximal (ms) entre deux frappes pour qu'elles forment un seul pas d'annulation.
pub const DEFAULT_GROUP_INTERVAL_MS: u64 = 1000;

/// Piles undo/redo.
#[derive(Debug)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// Nature et date de la dernière édition, si elle peut encore être étendue.
    last: Option<(EditKind, u64)>,
    group_interval_ms: u64,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// Historique vide, délai de groupement par défaut.
    #[must_use]
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            last: None,
            group_interval_ms: DEFAULT_GROUP_INTERVAL_MS,
        }
    }

    /// Change le délai de groupement des frappes.
    pub fn set_group_interval_ms(&mut self, ms: u64) {
        self.group_interval_ms = ms;
    }

    /// Enregistre une transaction déjà appliquée ; vide la pile redo. Fusionne avec la précédente
    /// quand c'est une frappe adjacente, de même nature, dans le délai imparti.
    pub fn record(&mut self, tx: Transaction, kind: EditKind, now_ms: u64) {
        self.redo.clear();
        let tx = match (self.last, self.undo.last_mut()) {
            (Some((last_kind, at)), Some(top))
                if kind == last_kind
                    && kind != EditKind::Other
                    && now_ms.saturating_sub(at) <= self.group_interval_ms =>
            {
                match try_merge(top, tx, kind) {
                    Ok(()) => {
                        self.last = Some((kind, now_ms));
                        return;
                    }
                    Err(tx) => tx,
                }
            }
            _ => tx,
        };
        self.undo.push(tx);
        self.last = Some((kind, now_ms));
    }

    /// Interdit de fusionner la prochaine édition avec la précédente (déplacement du curseur...).
    pub fn break_group(&mut self) {
        self.last = None;
    }

    pub(crate) fn take_undo(&mut self) -> Option<Transaction> {
        self.last = None;
        self.undo.pop()
    }

    pub(crate) fn take_redo(&mut self) -> Option<Transaction> {
        self.last = None;
        self.redo.pop()
    }

    pub(crate) fn push_redo(&mut self, tx: Transaction) {
        self.redo.push(tx);
    }

    pub(crate) fn push_undo(&mut self, tx: Transaction) {
        self.undo.push(tx);
    }

    /// Vrai s'il y a quelque chose à annuler.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Vrai s'il y a quelque chose à rétablir.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Nombre de pas d'annulation disponibles.
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }
}
