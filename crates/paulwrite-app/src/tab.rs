//! Un onglet : un éditeur, son état de chargement et de coloration syntaxique.

use std::ops::Range;

use paulwrite_syntax::Language;

use crate::editor::Editor;
use crate::worker::TabId;

/// Lignes de marge de part et d'autre des lignes visibles pour la fenêtre de coloration : un
/// défilement modéré ne demande alors rien de plus au worker.
pub const SYNTAX_MARGIN_LINES: usize = 100;

#[derive(Debug)]
pub struct Tab {
    pub id: TabId,
    pub editor: Editor,
    /// Langage reconnu d'après l'extension du fichier.
    pub language: Option<Language>,
    /// Le worker de coloration suit ce document.
    pub syntax_on: bool,
    /// Fenêtre d'octets demandée au worker de coloration (évite de la redemander en boucle).
    pub requested: Option<Range<usize>>,
    /// Progression du chargement `(octets lus, total)` tant que le fichier se lit.
    pub loading: Option<(u64, u64)>,
    /// État « modifié » affiché dans la barre d'onglets (pour détecter un changement).
    pub shown_modified: bool,
}

impl Tab {
    #[must_use]
    pub fn new(id: TabId) -> Self {
        Self {
            id,
            editor: Editor::new(),
            language: None,
            syntax_on: false,
            requested: None,
            loading: None,
            shown_modified: false,
        }
    }

    /// Onglet vierge : sans fichier, sans modification, rien en cours de chargement. Ouvrir un
    /// fichier le remplace au lieu d'ajouter un onglet.
    #[must_use]
    pub fn is_pristine(&self) -> bool {
        self.editor.path().is_none()
            && !self.editor.is_modified()
            && self.loading.is_none()
            && self.editor.document().buffer().is_empty()
    }

    /// Libellé affiché dans la barre d'onglets.
    #[must_use]
    pub fn label(&self) -> String {
        let name = self.editor.display_name();
        if self.loading.is_some() {
            format!("{name} …")
        } else {
            name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paulwrite_core::Document;
    use std::path::PathBuf;

    #[test]
    fn onglet_vierge() {
        let mut t = Tab::new(1);
        assert!(t.is_pristine());
        assert_eq!(t.label(), "Sans titre");
        t.editor.insert_text("a", 0);
        assert!(!t.is_pristine());

        let mut t = Tab::new(2);
        t.editor
            .load(Document::from_text("x"), Some(PathBuf::from("d/a.rs")));
        assert!(!t.is_pristine());
        assert_eq!(t.label(), "a.rs");
        t.loading = Some((0, 0));
        assert_eq!(t.label(), "a.rs …");
    }
}
