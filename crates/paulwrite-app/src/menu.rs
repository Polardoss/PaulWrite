//! Barre de menus (File, Edit, Selection… comme Sublime Text ; Fichier, Édition… en français).
//! Chaque entrée est une commande nommée : le menu n'a aucune logique propre.

use paulwrite_core::Motion;
use paulwrite_render::platform::MenuEntry;

use crate::command::Command;
use crate::i18n::Lang;
use crate::keymap::shortcut_label;

/// Les identifiants des entrées vont de cette valeur à `MENU_BASE + nombre d'entrées`.
pub const MENU_BASE: u32 = 1000;

/// Une ligne de menu : une commande avec son intitulé (anglais, français), ou un séparateur.
enum Line {
    Item(Command, &'static str, &'static str),
    Sep,
}

use Line::{Item, Sep};

type Menu = (&'static str, &'static str, Vec<Line>);

fn menus() -> Vec<Menu> {
    vec![
        (
            "&File",
            "&Fichier",
            vec![
                Item(Command::NewFile, "New File", "Nouveau fichier"),
                Item(Command::NewWindow, "New Window", "Nouvelle fen\u{ea}tre"),
                Item(
                    Command::Open,
                    "Open File\u{2026}",
                    "Ouvrir un fichier\u{2026}",
                ),
                Item(
                    Command::OpenFolder,
                    "Open Folder\u{2026}",
                    "Ouvrir un dossier\u{2026}",
                ),
                Sep,
                Item(Command::Save, "Save", "Enregistrer"),
                Item(
                    Command::SaveAs,
                    "Save As\u{2026}",
                    "Enregistrer sous\u{2026}",
                ),
                Sep,
                Item(Command::CloseTab, "Close File", "Fermer le fichier"),
                Sep,
                Item(Command::Quit, "Exit", "Quitter"),
            ],
        ),
        (
            "&Edit",
            "\u{c9}&dition",
            vec![
                Item(Command::Undo, "Undo", "Annuler"),
                Item(Command::Redo, "Redo", "R\u{e9}tablir"),
                Sep,
                Item(Command::Cut, "Cut", "Couper"),
                Item(Command::Copy, "Copy", "Copier"),
                Item(Command::Paste, "Paste", "Coller"),
                Sep,
                Item(Command::DeleteForward, "Delete", "Supprimer"),
                Sep,
                Item(
                    Command::DuplicateLine,
                    "Duplicate Line",
                    "Dupliquer la ligne",
                ),
                Item(Command::DeleteLine, "Delete Line", "Supprimer la ligne"),
                Item(
                    Command::MoveLineUp,
                    "Move Line Up",
                    "D\u{e9}placer la ligne vers le haut",
                ),
                Item(
                    Command::MoveLineDown,
                    "Move Line Down",
                    "D\u{e9}placer la ligne vers le bas",
                ),
                Item(
                    Command::ToggleComment,
                    "Toggle Comment",
                    "Commenter / d\u{e9}commenter",
                ),
                Item(Command::Indent, "Indent", "Indenter"),
                Item(Command::Unindent, "Unindent", "D\u{e9}sindenter"),
                Sep,
                Item(Command::Fold, "Fold", "Replier"),
                Item(Command::Unfold, "Unfold", "D\u{e9}plier"),
                Item(Command::FoldAll, "Fold All", "Tout replier"),
                Item(Command::UnfoldAll, "Unfold All", "Tout d\u{e9}plier"),
            ],
        ),
        (
            "&Selection",
            "&S\u{e9}lection",
            vec![
                Item(Command::SelectAll, "Select All", "Tout s\u{e9}lectionner"),
                Item(
                    Command::SelectLine,
                    "Expand Selection to Line",
                    "\u{c9}tendre la s\u{e9}lection \u{e0} la ligne",
                ),
                Item(
                    Command::SelectNextOccurrence,
                    "Select Next Occurrence",
                    "S\u{e9}lectionner l'occurrence suivante",
                ),
                Item(
                    Command::CollapseSelections,
                    "Single Selection",
                    "S\u{e9}lection unique",
                ),
                Sep,
                Item(
                    Command::AddCursorAbove,
                    "Add Cursor Above",
                    "Ajouter un curseur au-dessus",
                ),
                Item(
                    Command::AddCursorBelow,
                    "Add Cursor Below",
                    "Ajouter un curseur en dessous",
                ),
                Sep,
                Item(
                    Command::Extend(Motion::LineEnd),
                    "Expand Selection to Line End",
                    "\u{c9}tendre la s\u{e9}lection \u{e0} la fin de la ligne",
                ),
                Item(
                    Command::Extend(Motion::LineStart),
                    "Expand Selection to Line Start",
                    "\u{c9}tendre la s\u{e9}lection au d\u{e9}but de la ligne",
                ),
            ],
        ),
        (
            "F&ind",
            "&Rechercher",
            vec![Item(
                Command::SearchProject,
                "Find in Project\u{2026}",
                "Rechercher dans le projet\u{2026}",
            )],
        ),
        (
            "&View",
            "A&ffichage",
            vec![
                Item(Command::ToggleSidebar, "Side Bar", "Barre lat\u{e9}rale"),
                Item(
                    Command::ToggleWordWrap,
                    "Word Wrap",
                    "Retour \u{e0} la ligne",
                ),
                Sep,
                Item(Command::NextTab, "Next File", "Fichier suivant"),
                Item(
                    Command::PrevTab,
                    "Previous File",
                    "Fichier pr\u{e9}c\u{e9}dent",
                ),
                Sep,
                Item(Command::ScrollUp, "Scroll Up", "D\u{e9}filer vers le haut"),
                Item(
                    Command::ScrollDown,
                    "Scroll Down",
                    "D\u{e9}filer vers le bas",
                ),
            ],
        ),
        (
            "&Goto",
            "&Aller \u{e0}",
            vec![
                Item(
                    Command::GotoFile,
                    "Goto Anything\u{2026}",
                    "Aller \u{e0} un fichier\u{2026}",
                ),
                Item(
                    Command::GotoLine,
                    "Goto Line\u{2026}",
                    "Aller \u{e0} la ligne\u{2026}",
                ),
                Sep,
                Item(
                    Command::LspDefinition,
                    "Goto Definition",
                    "Aller \u{e0} la d\u{e9}finition",
                ),
                Item(
                    Command::LspDiagnostics,
                    "Show Diagnostics",
                    "Afficher les diagnostics",
                ),
            ],
        ),
        (
            "&Tools",
            "&Outils",
            vec![
                Item(
                    Command::CommandPalette,
                    "Command Palette\u{2026}",
                    "Palette de commandes\u{2026}",
                ),
                Sep,
                Item(Command::LspCompletion, "Auto Complete", "Compl\u{e9}tion"),
                Item(
                    Command::LspHover,
                    "Show Documentation",
                    "Afficher la documentation",
                ),
                Sep,
                Item(
                    Command::ReloadPlugins,
                    "Reload Plugins",
                    "Recharger les plugins",
                ),
            ],
        ),
        (
            "&Project",
            "&Projet",
            vec![
                Item(
                    Command::OpenFolder,
                    "Open Folder\u{2026}",
                    "Ouvrir un dossier\u{2026}",
                ),
                Item(Command::CloseFolder, "Close Folder", "Fermer le dossier"),
            ],
        ),
        (
            "P&references",
            "P&r\u{e9}f\u{e9}rences",
            vec![
                Item(Command::OpenSettings, "Settings", "Param\u{e8}tres"),
                Sep,
                Item(
                    Command::SetLanguage(Lang::En),
                    "Language: English",
                    "Langue : English",
                ),
                Item(
                    Command::SetLanguage(Lang::Fr),
                    "Language: Fran\u{e7}ais",
                    "Langue : Fran\u{e7}ais",
                ),
            ],
        ),
        (
            "&Help",
            "Ai&de",
            vec![Item(
                Command::About,
                "About PaulWrite",
                "\u{c0} propos de PaulWrite",
            )],
        ),
    ]
}

/// Raccourci affiché dans le menu : mêmes libellés que Sublime Text en anglais, libellés français
/// de la table des raccourcis sinon.
fn shortcut_text(cmd: Command, lang: Lang) -> Option<String> {
    if cmd == Command::ToggleSidebar {
        // Enchaînement de deux touches, géré par l'application.
        return Some("Ctrl+K, Ctrl+B".to_owned());
    }
    let label = shortcut_label(cmd)?;
    Some(match lang {
        Lang::Fr => label,
        Lang::En => label
            .replace("Maj+", "Shift+")
            .replace("Espace", "Space")
            .replace("Échap", "Esc")
            .replace("Début", "Home")
            .replace("Fin", "End")
            .replace("Entrée", "Enter")
            .replace("Suppr", "Delete")
            .replace("Retour arrière", "Backspace")
            .replace("Page ↑", "PgUp")
            .replace("Page ↓", "PgDn")
            .replace('←', "Left")
            .replace('→', "Right")
            .replace('↑', "Up")
            .replace('↓', "Down"),
    })
}

/// Construit la barre de menus et la table identifiant → commande (l'identifiant d'une entrée est
/// `MENU_BASE` + son indice dans la table). La table est la même dans les deux langues.
#[must_use]
pub fn build(lang: Lang) -> (Vec<(String, Vec<MenuEntry>)>, Vec<Command>) {
    let mut table = Vec::new();
    let bar = menus()
        .into_iter()
        .map(|(title_en, title_fr, lines)| {
            let entries = lines
                .into_iter()
                .map(|line| match line {
                    Line::Sep => MenuEntry::Separator,
                    Line::Item(cmd, en, fr) => {
                        let id = MENU_BASE + table.len() as u32;
                        table.push(cmd);
                        let label = lang.pick(en, fr);
                        let text = match shortcut_text(cmd, lang) {
                            Some(s) => format!("{label}\t{s}"),
                            None => label.to_owned(),
                        };
                        MenuEntry::Item(id, text)
                    }
                })
                .collect();
            (lang.pick(title_en, title_fr).to_owned(), entries)
        })
        .collect();
    (bar, table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_menus_sont_ceux_de_sublime_text() {
        let (bar, table) = build(Lang::En);
        let titles: Vec<_> = bar.iter().map(|(t, _)| t.replace('&', "")).collect();
        assert_eq!(
            titles,
            [
                "File",
                "Edit",
                "Selection",
                "Find",
                "View",
                "Goto",
                "Tools",
                "Project",
                "Preferences",
                "Help"
            ]
        );
        // chaque identifiant renvoie à sa commande
        for (_, entries) in &bar {
            for e in entries {
                if let MenuEntry::Item(id, _) = e {
                    assert!(table.get((id - MENU_BASE) as usize).is_some());
                }
            }
        }
    }

    #[test]
    fn les_menus_en_francais() {
        let (bar, table) = build(Lang::Fr);
        let titles: Vec<_> = bar.iter().map(|(t, _)| t.replace('&', "")).collect();
        assert_eq!(
            titles,
            [
                "Fichier",
                "Édition",
                "Sélection",
                "Rechercher",
                "Affichage",
                "Aller à",
                "Outils",
                "Projet",
                "Préférences",
                "Aide"
            ]
        );
        // même structure et mêmes commandes que l'anglais : seuls les textes changent
        let (en, en_table) = build(Lang::En);
        assert_eq!(table, en_table);
        for ((_, a), (_, b)) in bar.iter().zip(&en) {
            assert_eq!(a.len(), b.len());
        }
        // aucun libellé français identique à l'anglais (traduction oubliée), hors noms propres
        let label = |bar: &[(String, Vec<MenuEntry>)]| -> Vec<String> {
            bar.iter()
                .flat_map(|(_, e)| e)
                .filter_map(|e| match e {
                    MenuEntry::Item(_, t) => Some(t.split('\t').next().unwrap().to_owned()),
                    MenuEntry::Separator => None,
                })
                .collect()
        };
        let same: Vec<_> = label(&bar)
            .into_iter()
            .zip(label(&en))
            .filter(|(f, e)| f == e)
            .map(|(f, _)| f)
            .collect();
        assert!(same.is_empty(), "non traduits : {same:?}");
    }

    #[test]
    fn les_raccourcis_affiches_sont_ceux_du_clavier() {
        let (bar, table) = build(Lang::En);
        let find = |cmd: Command| {
            bar.iter().flat_map(|(_, e)| e).find_map(|e| match e {
                MenuEntry::Item(id, text) if table[(id - MENU_BASE) as usize] == cmd => {
                    Some(text.clone())
                }
                _ => None,
            })
        };
        assert_eq!(find(Command::Save).as_deref(), Some("Save\tCtrl+S"));
        assert_eq!(
            find(Command::SaveAs).as_deref(),
            Some("Save As\u{2026}\tCtrl+Shift+S")
        );
        assert_eq!(
            find(Command::ToggleSidebar).as_deref(),
            Some("Side Bar\tCtrl+K, Ctrl+B")
        );
        assert_eq!(
            find(Command::OpenFolder).as_deref(),
            Some("Open Folder\u{2026}")
        );
        // en français, les touches gardent leur libellé français
        let (bar, table) = build(Lang::Fr);
        let find = |cmd: Command| {
            bar.iter().flat_map(|(_, e)| e).find_map(|e| match e {
                MenuEntry::Item(id, text) if table[(id - MENU_BASE) as usize] == cmd => {
                    Some(text.clone())
                }
                _ => None,
            })
        };
        assert_eq!(
            find(Command::SaveAs).as_deref(),
            Some("Enregistrer sous\u{2026}\tCtrl+Maj+S")
        );
    }
}
