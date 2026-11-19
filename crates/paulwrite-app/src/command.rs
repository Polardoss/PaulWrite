//! Commandes nommées. Raccourcis, palette de commandes (étape 4) et futurs plugins appellent
//! les mêmes commandes : aucune fonctionnalité n'existe en dehors de ce registre.

use paulwrite_core::Motion;

use crate::i18n::Lang;
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
    /// Nouvelle fenêtre PaulWrite (un nouveau processus, vide).
    NewWindow,
    /// Rouvre le dernier onglet fermé (avec son curseur et sa vue).
    ReopenClosedTab,
    /// Liste des fichiers ouverts récemment.
    OpenRecent,
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
    /// Choisir un dossier à ouvrir (il devient le projet).
    OpenFolder,
    /// Fermer le dossier ouvert.
    CloseFolder,
    /// Afficher ou masquer la barre latérale.
    ToggleSidebar,
    /// Écran partagé : deux volets côte à côte, chacun avec ses onglets (ou retour à un seul).
    ToggleSplit,
    /// Écran partagé : donne le focus à l'autre volet.
    FocusOtherPane,
    /// Écran partagé : envoie l'onglet courant dans l'autre volet (ouvre le partage au besoin).
    MoveToOtherPane,
    /// Ouvrir le fichier de configuration.
    OpenSettings,
    /// À propos de PaulWrite.
    About,
    /// Rechercher dans le document (panneau).
    Find,
    /// Occurrence suivante / précédente de la dernière recherche.
    FindNext,
    FindPrevious,
    /// Rechercher et remplacer dans le document (panneau).
    Replace,
    /// Remplacer dans tous les fichiers du projet (panneau, aperçu, confirmation).
    ReplaceInProject,
    /// Duplique la ou les lignes du curseur.
    DuplicateLine,
    /// Supprime la ou les lignes du curseur.
    DeleteLine,
    /// Déplace la ou les lignes d'une ligne vers le haut / le bas.
    MoveLineUp,
    MoveLineDown,
    /// Commente ou décommente les lignes (selon le langage).
    ToggleComment,
    /// Commentaire de bloc (`/* … */`) autour de la sélection ou de la ligne.
    ToggleBlockComment,
    /// Indente / désindente les lignes touchées par la sélection.
    Indent,
    Unindent,
    /// Sélectionne les lignes entières (une ligne de plus à chaque appui).
    SelectLine,
    /// Sans sélection : le mot ; avec une sélection : ajoute son occurrence suivante.
    SelectNextOccurrence,
    /// Replie la région du curseur.
    Fold,
    /// Déplie la région du curseur.
    Unfold,
    /// Replie toutes les régions de premier niveau.
    FoldAll,
    /// Déplie toutes les régions.
    UnfoldAll,
    /// Active ou désactive le retour à la ligne automatique.
    ToggleWordWrap,
    /// Agrandit / réduit / rétablit la taille du texte.
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// Choisit la langue de l'interface (enregistrée dans `config.toml`).
    SetLanguage(Lang),
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
            Command::NewWindow => "file.new_window",
            Command::ReopenClosedTab => "tab.reopen_closed",
            Command::OpenRecent => "file.open_recent",
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
            Command::OpenFolder => "project.open_folder",
            Command::CloseFolder => "project.close_folder",
            Command::ToggleSidebar => "view.toggle_sidebar",
            Command::ToggleSplit => "view.toggle_split",
            Command::FocusOtherPane => "view.focus_other_pane",
            Command::MoveToOtherPane => "view.move_to_other_pane",
            Command::OpenSettings => "preferences.open_settings",
            Command::About => "help.about",
            Command::Find => "find.find",
            Command::FindNext => "find.next",
            Command::FindPrevious => "find.previous",
            Command::Replace => "find.replace",
            Command::ReplaceInProject => "find.replace_in_project",
            Command::DuplicateLine => "edit.duplicate_line",
            Command::DeleteLine => "edit.delete_line",
            Command::MoveLineUp => "edit.move_line_up",
            Command::MoveLineDown => "edit.move_line_down",
            Command::ToggleComment => "edit.toggle_comment",
            Command::ToggleBlockComment => "edit.toggle_block_comment",
            Command::Indent => "edit.indent",
            Command::Unindent => "edit.unindent",
            Command::SelectLine => "select.line",
            Command::SelectNextOccurrence => "select.next_occurrence",
            Command::Fold => "fold.fold",
            Command::Unfold => "fold.unfold",
            Command::FoldAll => "fold.fold_all",
            Command::UnfoldAll => "fold.unfold_all",
            Command::ToggleWordWrap => "view.toggle_word_wrap",
            Command::ZoomIn => "view.zoom_in",
            Command::ZoomOut => "view.zoom_out",
            Command::ZoomReset => "view.zoom_reset",
            Command::SetLanguage(Lang::En) => "preferences.language_en",
            Command::SetLanguage(Lang::Fr) => "preferences.language_fr",
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
            Command::NewWindow,
            Command::ReopenClosedTab,
            Command::OpenRecent,
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
            Command::OpenFolder,
            Command::CloseFolder,
            Command::ToggleSidebar,
            Command::ToggleSplit,
            Command::FocusOtherPane,
            Command::MoveToOtherPane,
            Command::OpenSettings,
            Command::About,
            Command::Find,
            Command::FindNext,
            Command::FindPrevious,
            Command::Replace,
            Command::ReplaceInProject,
            Command::DuplicateLine,
            Command::DeleteLine,
            Command::MoveLineUp,
            Command::MoveLineDown,
            Command::ToggleComment,
            Command::ToggleBlockComment,
            Command::Indent,
            Command::Unindent,
            Command::SelectLine,
            Command::SelectNextOccurrence,
            Command::Fold,
            Command::Unfold,
            Command::FoldAll,
            Command::UnfoldAll,
            Command::ToggleWordWrap,
            Command::ZoomIn,
            Command::ZoomOut,
            Command::ZoomReset,
            Command::SetLanguage(Lang::En),
            Command::SetLanguage(Lang::Fr),
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
            Command::NewWindow => "Fichier : nouvelle fenêtre",
            Command::ReopenClosedTab => "Onglet : rouvrir l'onglet fermé",
            Command::OpenRecent => "Fichier : ouvrir un fichier récent",
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
            Command::OpenFolder => "Projet : ouvrir un dossier",
            Command::CloseFolder => "Projet : fermer le dossier",
            Command::ToggleSidebar => "Affichage : barre latérale",
            Command::ToggleSplit => "Affichage : écran partagé",
            Command::FocusOtherPane => "Affichage : aller à l'autre volet",
            Command::MoveToOtherPane => "Affichage : envoyer l'onglet dans l'autre volet",
            Command::OpenSettings => "Préférences : ouvrir la configuration",
            Command::About => "Aide : à propos de PaulWrite",
            Command::Find => "Recherche : chercher dans le document",
            Command::FindNext => "Recherche : occurrence suivante",
            Command::FindPrevious => "Recherche : occurrence précédente",
            Command::Replace => "Recherche : remplacer dans le document",
            Command::ReplaceInProject => "Recherche : remplacer dans le projet",
            Command::DuplicateLine => "Édition : dupliquer la ligne",
            Command::DeleteLine => "Édition : supprimer la ligne",
            Command::MoveLineUp => "Édition : déplacer la ligne vers le haut",
            Command::MoveLineDown => "Édition : déplacer la ligne vers le bas",
            Command::ToggleComment => "Édition : commenter / décommenter",
            Command::ToggleBlockComment => "Édition : commentaire de bloc",
            Command::Indent => "Édition : indenter",
            Command::Unindent => "Édition : désindenter",
            Command::SelectLine => "Sélection : la ligne",
            Command::SelectNextOccurrence => "Sélection : occurrence suivante",
            Command::Fold => "Repli : replier la région",
            Command::Unfold => "Repli : déplier la région",
            Command::FoldAll => "Repli : tout replier",
            Command::UnfoldAll => "Repli : tout déplier",
            Command::ToggleWordWrap => "Affichage : retour à la ligne automatique",
            Command::ZoomIn => "Affichage : agrandir le texte",
            Command::ZoomOut => "Affichage : réduire le texte",
            Command::ZoomReset => "Affichage : taille du texte par défaut",
            Command::SetLanguage(Lang::En) => "Préférences : langue English",
            Command::SetLanguage(Lang::Fr) => "Préférences : langue Français",
            Command::Quit => "Application : quitter",
        }
    }
}

impl Command {
    /// Intitulé dans la langue de l'interface (palette de commandes).
    #[must_use]
    pub fn title_in(self, lang: Lang) -> &'static str {
        match lang {
            Lang::Fr => self.title(),
            Lang::En => self.title_en(),
        }
    }

    fn title_en(self) -> &'static str {
        match self {
            Command::Move(m) => MOVE_TITLES_EN[index_of(m)],
            Command::Extend(m) => EXTEND_TITLES_EN[index_of(m)],
            Command::PageUp(false) => "Cursor: page up",
            Command::PageUp(true) => "Selection: page up",
            Command::PageDown(false) => "Cursor: page down",
            Command::PageDown(true) => "Selection: page down",
            Command::ScrollUp => "View: scroll up",
            Command::ScrollDown => "View: scroll down",
            Command::InsertNewline => "Edit: new line",
            Command::InsertTab => "Edit: tab",
            Command::DeleteBackward => "Edit: delete backward",
            Command::DeleteForward => "Edit: delete forward",
            Command::Undo => "Edit: undo",
            Command::Redo => "Edit: redo",
            Command::SelectAll => "Selection: select all",
            Command::CollapseSelections => "Selection: single selection",
            Command::AddCursorAbove => "Cursor: add above",
            Command::AddCursorBelow => "Cursor: add below",
            Command::Copy => "Edit: copy",
            Command::Cut => "Edit: cut",
            Command::Paste => "Edit: paste",
            Command::Save => "File: save",
            Command::SaveAs => "File: save as",
            Command::Open => "File: open",
            Command::NewFile => "File: new",
            Command::NewWindow => "File: new window",
            Command::ReopenClosedTab => "Tab: reopen closed tab",
            Command::OpenRecent => "File: open recent file",
            Command::CloseTab => "Tab: close",
            Command::NextTab => "Tab: next",
            Command::PrevTab => "Tab: previous",
            Command::GotoFile => "Goto project file",
            Command::CommandPalette => "Command palette",
            Command::GotoLine => "Goto line",
            Command::SearchProject => "Find in project",
            Command::ReloadPlugins => "Plugins: reload",
            Command::LspCompletion => "Language: auto complete",
            Command::LspDefinition => "Language: goto definition",
            Command::LspHover => "Language: show documentation",
            Command::LspDiagnostics => "Language: show diagnostics",
            Command::OpenFolder => "Project: open folder",
            Command::CloseFolder => "Project: close folder",
            Command::ToggleSidebar => "View: side bar",
            Command::ToggleSplit => "View: split screen",
            Command::FocusOtherPane => "View: focus the other pane",
            Command::MoveToOtherPane => "View: move the tab to the other pane",
            Command::OpenSettings => "Preferences: open settings",
            Command::About => "Help: about PaulWrite",
            Command::Find => "Find: find in document",
            Command::FindNext => "Find: next match",
            Command::FindPrevious => "Find: previous match",
            Command::Replace => "Find: replace in document",
            Command::ReplaceInProject => "Find: replace in project",
            Command::DuplicateLine => "Edit: duplicate line",
            Command::DeleteLine => "Edit: delete line",
            Command::MoveLineUp => "Edit: move line up",
            Command::MoveLineDown => "Edit: move line down",
            Command::ToggleComment => "Edit: toggle comment",
            Command::ToggleBlockComment => "Edit: toggle block comment",
            Command::Indent => "Edit: indent",
            Command::Unindent => "Edit: unindent",
            Command::SelectLine => "Selection: line",
            Command::SelectNextOccurrence => "Selection: next occurrence",
            Command::Fold => "Fold: fold region",
            Command::Unfold => "Fold: unfold region",
            Command::FoldAll => "Fold: fold all",
            Command::UnfoldAll => "Fold: unfold all",
            Command::ToggleWordWrap => "View: word wrap",
            Command::ZoomIn => "View: zoom in",
            Command::ZoomOut => "View: zoom out",
            Command::ZoomReset => "View: reset zoom",
            Command::SetLanguage(Lang::En) => "Preferences: language English",
            Command::SetLanguage(Lang::Fr) => "Preferences: language Français",
            Command::Quit => "Application: quit",
        }
    }
}

const MOVE_TITLES_EN: [&str; 12] = [
    "Cursor: left",
    "Cursor: right",
    "Cursor: up",
    "Cursor: down",
    "Cursor: previous word",
    "Cursor: next word",
    "Cursor: line start",
    "Cursor: line end",
    "Cursor: previous paragraph",
    "Cursor: next paragraph",
    "Cursor: document start",
    "Cursor: document end",
];

const EXTEND_TITLES_EN: [&str; 12] = [
    "Selection: extend left",
    "Selection: extend right",
    "Selection: extend up",
    "Selection: extend down",
    "Selection: extend to previous word",
    "Selection: extend to next word",
    "Selection: extend to line start",
    "Selection: extend to line end",
    "Selection: extend to previous paragraph",
    "Selection: extend to next paragraph",
    "Selection: extend to document start",
    "Selection: extend to document end",
];

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
    /// Lancer une nouvelle fenêtre.
    NewWindow,
    /// Rouvrir le dernier onglet fermé.
    ReopenClosed,
    /// Afficher la liste des fichiers récents.
    OpenRecent,
    CloseTab,
    NextTab,
    PrevTab,
    /// Ouvrir le panneau flottant dans ce mode.
    OpenPalette(PaletteMode),
    /// Recharger les plugins Lua.
    ReloadPlugins,
    /// Demande au serveur de langage (voir [`LspAction`]).
    Lsp(LspAction),
    OpenFolder,
    CloseFolder,
    ToggleSidebar,
    ToggleSplit,
    FocusOtherPane,
    MoveToOtherPane,
    OpenSettings,
    About,
    /// Occurrence suivante (`true`) ou précédente de la dernière recherche.
    FindStep(bool),
    /// Bascule le retour à la ligne automatique (réglage commun à tous les onglets).
    ToggleWordWrap,
    /// Taille du texte : `+1`, `-1`, ou retour à la valeur par défaut (`0`).
    Zoom(i32),
    /// Change la langue de l'interface.
    SetLanguage(Lang),
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
    fn les_intitules_anglais_sont_complets_et_uniques() {
        let mut titles: Vec<_> = Command::all()
            .iter()
            .map(|c| c.title_in(Lang::En))
            .collect();
        assert!(titles.iter().all(|t| !t.is_empty()));
        let n = titles.len();
        titles.sort_unstable();
        titles.dedup();
        assert_eq!(titles.len(), n, "intitulés anglais en double");
        // l'anglais et le français diffèrent pour chaque commande (aucune traduction oubliée)
        let same: Vec<_> = Command::all()
            .into_iter()
            .filter(|c| c.title_in(Lang::En) == c.title_in(Lang::Fr))
            .map(Command::name)
            .collect();
        assert!(same.is_empty(), "non traduits : {same:?}");
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
