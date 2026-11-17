//! Commandes nommées. Raccourcis, palette de commandes (étape 4) et futurs plugins appellent
//! les mêmes commandes : aucune fonctionnalité n'existe en dehors de ce registre.

use paulwrite_core::Motion;

use crate::palette::PaletteMode;

/// Une fonctionnalité de l'éditeur, identifiée par un nom stable (`cursor.left`, `file.save`…).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Déplace tous les curseurs.
    Move(Motion),
    /// Étend toutes les sélections.
    Extend(Motion),
    /// Une page vers le haut / le bas (`bool` : étendre la sélection).
    PageUp(bool),
    PageDown(bool),
    /// Fait défiler la vue sans bouger le curseur.
    ScrollUp,
    ScrollDown,
    InsertNewline,
    InsertTab,
    DeleteBackward,
    DeleteForward,
    Undo,
    Redo,
    SelectAll,
    /// Réduit à un seul curseur (le principal), sans sélection.
    CollapseSelections,
    AddCursorAbove,
    AddCursorBelow,
    Copy,
    Cut,
    Paste,
    Save,
    SaveAs,
    Open,
    /// Nouveau document vide dans un nouvel onglet.
    NewFile,
    /// Ferme l'onglet courant.
    CloseTab,
    NextTab,
    PrevTab,
    /// Goto Anything : ouvrir un fichier du projet.
    GotoFile,
    /// Palette de commandes.
    CommandPalette,
    /// Aller à une ligne du document courant.
    GotoLine,
    /// Rechercher du texte dans le projet.
    SearchProject,
    /// Recharger les plugins Lua depuis le disque.
    ReloadPlugins,
    /// Complétion du serveur de langage à la position du curseur.
    LspCompletion,
    /// Aller à la définition du symbole sous le curseur.
    LspDefinition,
    /// Texte de survol (documentation, type) du symbole sous le curseur.
    LspHover,
    /// Liste des diagnostics (erreurs, avertissements) du document.
    LspDiagnostics,
    Quit,
}

const MOTIONS: [(Motion, &str); 12] = [
    (Motion::Left, "left"),
    (Motion::Right, "right"),
    (Motion::Up, "up"),
    (Motion::Down, "down"),
    (Motion::WordLeft, "word_left"),
    (Motion::WordRight, "word_right"),
    (Motion::LineStart, "line_start"),
    (Motion::LineEnd, "line_end"),
    (Motion::ParagraphUp, "paragraph_up"),
    (Motion::ParagraphDown, "paragraph_down"),
    (Motion::DocStart, "doc_start"),
    (Motion::DocEnd, "doc_end"),
];

#[cfg(test)]
fn motion_name(m: Motion) -> &'static str {
    MOTIONS
        .iter()
        .find(|(x, _)| *x == m)
        .map_or("?", |(_, n)| n)
}

impl Command {
    /// Nom stable de la commande (utilisé par la palette, la configuration et les plugins).
    #[must_use]
    pub fn name(self) -> &'static str {
        // Les noms des déplacements sont assemblés à la demande : table statique ci-dessous.
        match self {
            Command::Move(m) => MOVE_NAMES[index_of(m)],
            Command::Extend(m) => EXTEND_NAMES[index_of(m)],
            Command::PageUp(false) => "cursor.page_up",
            Command::PageUp(true) => "select.page_up",
            Command::PageDown(false) => "cursor.page_down",
            Command::PageDown(true) => "select.page_down",
            Command::ScrollUp => "view.scroll_up",
            Command::ScrollDown => "view.scroll_down",
            Command::InsertNewline => "edit.newline",
            Command::InsertTab => "edit.tab",
            Command::DeleteBackward => "edit.delete_backward",
            Command::DeleteForward => "edit.delete_forward",
            Command::Undo => "edit.undo",
            Command::Redo => "edit.redo",
            Command::SelectAll => "select.all",
            Command::CollapseSelections => "select.collapse",
            Command::AddCursorAbove => "cursor.add_above",
            Command::AddCursorBelow => "cursor.add_below",
            Command::Copy => "edit.copy",
            Command::Cut => "edit.cut",
            Command::Paste => "edit.paste",
            Command::Save => "file.save",
            Command::SaveAs => "file.save_as",
            Command::Open => "file.open",
            Command::NewFile => "file.new",
            Command::CloseTab => "tab.close",
            Command::NextTab => "tab.next",
            Command::PrevTab => "tab.prev",
            Command::GotoFile => "palette.files",
            Command::CommandPalette => "palette.commands",
            Command::GotoLine => "goto.line",
            Command::SearchProject => "search.project",
            Command::ReloadPlugins => "plugins.reload",
            Command::LspCompletion => "lsp.completion",
            Command::LspDefinition => "lsp.definition",
            Command::LspHover => "lsp.hover",
            Command::LspDiagnostics => "lsp.diagnostics",
            Command::Quit => "app.quit",
        }
    }

    /// Retrouve une commande par son nom.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().into_iter().find(|c| c.name() == name)
    }

    /// Toutes les commandes (palette de commandes, tests de cohérence).
    #[must_use]
    pub fn all() -> Vec<Command> {
        let mut v: Vec<Command> = MOTIONS.iter().map(|(m, _)| Command::Move(*m)).collect();
        v.extend(MOTIONS.iter().map(|(m, _)| Command::Extend(*m)));
        v.extend([
            Command::PageUp(false),
            Command::PageUp(true),
            Command::PageDown(false),
            Command::PageDown(true),
            Command::ScrollUp,
            Command::ScrollDown,
            Command::InsertNewline,
            Command::InsertTab,
            Command::DeleteBackward,
            Command::DeleteForward,
            Command::Undo,
            Command::Redo,
            Command::SelectAll,
            Command::CollapseSelections,
            Command::AddCursorAbove,
            Command::AddCursorBelow,
            Command::Copy,
            Command::Cut,
            Command::Paste,
            Command::Save,
            Command::SaveAs,
            Command::Open,
            Command::NewFile,
            Command::CloseTab,
            Command::NextTab,
            Command::PrevTab,
            Command::GotoFile,
            Command::CommandPalette,
            Command::GotoLine,
            Command::SearchProject,
            Command::ReloadPlugins,
            Command::LspCompletion,
            Command::LspDefinition,
            Command::LspHover,
            Command::LspDiagnostics,
            Command::Quit,
        ]);
        v
    }

    /// Intitulé affiché dans la palette de commandes.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Command::Move(m) => MOVE_TITLES[index_of(m)],
            Command::Extend(m) => EXTEND_TITLES[index_of(m)],
            Command::PageUp(false) => "Curseur : page précédente",
            Command::PageUp(true) => "Sélection : page précédente",
            Command::PageDown(false) => "Curseur : page suivante",
            Command::PageDown(true) => "Sélection : page suivante",
            Command::ScrollUp => "Affichage : défiler vers le haut",
            Command::ScrollDown => "Affichage : défiler vers le bas",
            Command::InsertNewline => "Édition : saut de ligne",
            Command::InsertTab => "Édition : tabulation",
            Command::DeleteBackward => "Édition : supprimer en arrière",
            Command::DeleteForward => "Édition : supprimer en avant",
            Command::Undo => "Édition : annuler",
            Command::Redo => "Édition : rétablir",
            Command::SelectAll => "Sélection : tout sélectionner",
            Command::CollapseSelections => "Sélection : un seul curseur",
            Command::AddCursorAbove => "Curseur : ajouter au-dessus",
            Command::AddCursorBelow => "Curseur : ajouter en dessous",
            Command::Copy => "Édition : copier",
            Command::Cut => "Édition : couper",
            Command::Paste => "Édition : coller",
            Command::Save => "Fichier : enregistrer",
            Command::SaveAs => "Fichier : enregistrer sous",
            Command::Open => "Fichier : ouvrir",
            Command::NewFile => "Fichier : nouveau",
            Command::CloseTab => "Onglet : fermer",
            Command::NextTab => "Onglet : suivant",
            Command::PrevTab => "Onglet : précédent",
            Command::GotoFile => "Aller à un fichier du projet",
            Command::CommandPalette => "Palette de commandes",
            Command::GotoLine => "Aller à la ligne",
            Command::SearchProject => "Rechercher dans le projet",
            Command::ReloadPlugins => "Plugins : recharger",
            Command::LspCompletion => "Langage : complétion",
            Command::LspDefinition => "Langage : aller à la définition",
            Command::LspHover => "Langage : survol (documentation)",
            Command::LspDiagnostics => "Langage : afficher les diagnostics",
            Command::Quit => "Application : quitter",
        }
    }
}

fn index_of(m: Motion) -> usize {
    MOTIONS.iter().position(|(x, _)| *x == m).unwrap_or(0)
}

const MOVE_TITLES: [&str; 12] = [
    "Curseur : à gauche",
    "Curseur : à droite",
    "Curseur : vers le haut",
    "Curseur : vers le bas",
    "Curseur : mot précédent",
    "Curseur : mot suivant",
    "Curseur : début de ligne",
    "Curseur : fin de ligne",
    "Curseur : paragraphe précédent",
    "Curseur : paragraphe suivant",
    "Curseur : début du document",
    "Curseur : fin du document",
];

const EXTEND_TITLES: [&str; 12] = [
    "Sélection : étendre à gauche",
    "Sélection : étendre à droite",
    "Sélection : étendre vers le haut",
    "Sélection : étendre vers le bas",
    "Sélection : étendre au mot précédent",
    "Sélection : étendre au mot suivant",
    "Sélection : étendre au début de ligne",
    "Sélection : étendre à la fin de ligne",
    "Sélection : étendre au paragraphe précédent",
    "Sélection : étendre au paragraphe suivant",
    "Sélection : étendre au début du document",
    "Sélection : étendre à la fin du document",
];

const MOVE_NAMES: [&str; 12] = [
    "cursor.left",
    "cursor.right",
    "cursor.up",
    "cursor.down",
    "cursor.word_left",
    "cursor.word_right",
    "cursor.line_start",
    "cursor.line_end",
    "cursor.paragraph_up",
    "cursor.paragraph_down",
    "cursor.doc_start",
    "cursor.doc_end",
];

const EXTEND_NAMES: [&str; 12] = [
    "select.left",
    "select.right",
    "select.up",
    "select.down",
    "select.word_left",
    "select.word_right",
    "select.line_start",
    "select.line_end",
    "select.paragraph_up",
    "select.paragraph_down",
    "select.doc_start",
    "select.doc_end",
];

/// Effet à exécuter par l'application (hors de portée de l'éditeur : presse-papiers, fichiers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Mettre ce texte dans le presse-papiers.
    SetClipboard(String),
    /// Demander le contenu du presse-papiers (puis l'insérer).
    RequestPaste,
    Save,
    SaveAs,
    Open,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    /// Ouvrir le panneau flottant dans ce mode.
    OpenPalette(PaletteMode),
    /// Recharger les plugins Lua.
    ReloadPlugins,
    /// Demande au serveur de langage (voir [`LspAction`]).
    Lsp(LspAction),
    Quit,
}

/// Ce qu'on demande au serveur de langage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LspAction {
    Completion,
    Definition,
    Hover,
    Diagnostics,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_noms_sont_uniques_et_reversibles() {
        let all = Command::all();
        let mut names: Vec<_> = all.iter().map(|c| c.name()).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "noms de commande en double");
        for c in all {
            assert_eq!(Command::from_name(c.name()), Some(c));
        }
        assert_eq!(Command::from_name("n'existe.pas"), None);
    }

    #[test]
    fn chaque_commande_a_un_intitule_unique() {
        let mut titles: Vec<_> = Command::all().iter().map(|c| c.title()).collect();
        assert!(titles.iter().all(|t| !t.is_empty()));
        let n = titles.len();
        titles.sort_unstable();
        titles.dedup();
        assert_eq!(titles.len(), n, "intitulés en double");
    }

    #[test]
    fn suite_des_noms() {
        // cohérence de la table des déplacements
        assert_eq!(motion_name(Motion::WordLeft), "word_left");
        assert_eq!(Command::Move(Motion::WordLeft).name(), "cursor.word_left");
        assert_eq!(Command::Extend(Motion::DocEnd).name(), "select.doc_end");
    }
}
