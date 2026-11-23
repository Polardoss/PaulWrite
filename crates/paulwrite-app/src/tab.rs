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
    /// Volet où l'onglet s'affiche (0 : gauche ou seul, 1 : droite en écran partagé).
    pub pane: usize,
    /// Les onglets qui portent le même numéro (non nul) sont des **vues jumelles** d'un même
    /// fichier : leurs textes sont tenus identiques (voir `app/links.rs`). 0 : onglet seul.
    pub link: u64,
    /// Parmi les vues jumelles, celle qui porte l'historique d'annulation commun (celle qui reçoit
    /// la saisie).
    pub holds_history: bool,
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
    /// État Git du fichier au moment de la dernière demande des marqueurs de la gouttière (un
    /// fichier qui devient suivi, ou qui cesse de l'être, change ses marqueurs).
    pub diff_state: Option<crate::git::FileState>,
    /// Ligne (à partir de 1) où placer le curseur dès que le fichier est chargé.
    pub pending_goto: Option<usize>,
    /// Curseur et vue à rétablir dès que le fichier est chargé (reprise de session) : ligne et
    /// colonne à partir de 1, première ligne visible.
    pub pending_restore: Option<(usize, usize, usize)>,
    /// Empreinte du fichier sur le disque quand on l'a lu ou enregistré (détecte les
    /// modifications faites par un autre programme).
    pub disk_stamp: Option<crate::worker::Stamp>,
    /// Le fichier est en cours de rechargement (changé sur le disque) : pas de nouvelle vérification.
    pub reloading: bool,
    /// Chemin sous lequel le document est ouvert dans un serveur de langage (`None` : pas suivi).
    pub lsp_path: Option<std::path::PathBuf>,
    /// Version du document envoyée au serveur de langage (augmente à chaque modification).
    pub lsp_version: i32,
    /// Derniers diagnostics reçus du serveur de langage.
    pub diagnostics: Vec<paulwrite_lsp::Diagnostic>,
}

impl Tab {
    #[must_use]
    pub fn new(id: TabId) -> Self {
        Self {
            id,
            pane: 0,
            link: 0,
            holds_history: false,
            editor: Editor::new(),
            language: None,
            syntax_on: false,
            requested: None,
            loading: None,
            shown_modified: false,
            diff_state: None,
            pending_goto: None,
            pending_restore: None,
            disk_stamp: None,
            reloading: false,
            lsp_path: None,
            lsp_version: 0,
            diagnostics: Vec::new(),
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
        assert_eq!(t.label(), "untitled");
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
