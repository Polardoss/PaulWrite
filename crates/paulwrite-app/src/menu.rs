//! Barre de menus (File, Edit, Selection… comme Sublime Text). Chaque entrée est une commande
//! nommée : le menu n'a aucune logique propre.

use paulwrite_core::Motion;
use paulwrite_render::platform::MenuEntry;

use crate::command::Command;
use crate::keymap::shortcut_label;

/// Les identifiants des entrées vont de cette valeur à `MENU_BASE + nombre d'entrées`.
pub const MENU_BASE: u32 = 1000;

/// Une ligne de menu : une commande avec son intitulé, ou un séparateur.
enum Line {
    Item(Command, &'static str),
    Sep,
}

use Line::{Item, Sep};

fn menus() -> Vec<(&'static str, Vec<Line>)> {
    vec![
        (
            "&File",
            vec![
                Item(Command::NewFile, "New File"),
                Item(Command::Open, "Open File\u{2026}"),
                Item(Command::OpenFolder, "Open Folder\u{2026}"),
                Sep,
                Item(Command::Save, "Save"),
                Item(Command::SaveAs, "Save As\u{2026}"),
                Sep,
                Item(Command::CloseTab, "Close File"),
                Sep,
                Item(Command::Quit, "Exit"),
            ],
        ),
        (
            "&Edit",
            vec![
                Item(Command::Undo, "Undo"),
                Item(Command::Redo, "Redo"),
                Sep,
                Item(Command::Cut, "Cut"),
                Item(Command::Copy, "Copy"),
                Item(Command::Paste, "Paste"),
                Sep,
                Item(Command::DeleteForward, "Delete"),
            ],
        ),
        (
            "&Selection",
            vec![
                Item(Command::SelectAll, "Select All"),
                Item(Command::CollapseSelections, "Single Selection"),
                Sep,
                Item(Command::AddCursorAbove, "Add Cursor Above"),
                Item(Command::AddCursorBelow, "Add Cursor Below"),
                Sep,
                Item(
                    Command::Extend(Motion::LineEnd),
                    "Expand Selection to Line End",
                ),
                Item(
                    Command::Extend(Motion::LineStart),
                    "Expand Selection to Line Start",
                ),
            ],
        ),
        (
            "F&ind",
            vec![Item(Command::SearchProject, "Find in Project\u{2026}")],
        ),
        (
            "&View",
            vec![
                Item(Command::ToggleSidebar, "Side Bar"),
                Sep,
                Item(Command::NextTab, "Next File"),
                Item(Command::PrevTab, "Previous File"),
                Sep,
                Item(Command::ScrollUp, "Scroll Up"),
                Item(Command::ScrollDown, "Scroll Down"),
            ],
        ),
        (
            "&Goto",
            vec![
                Item(Command::GotoFile, "Goto Anything\u{2026}"),
                Item(Command::GotoLine, "Goto Line\u{2026}"),
                Sep,
                Item(Command::LspDefinition, "Goto Definition"),
                Item(Command::LspDiagnostics, "Show Diagnostics"),
            ],
        ),
        (
            "&Tools",
            vec![
                Item(Command::CommandPalette, "Command Palette\u{2026}"),
                Sep,
                Item(Command::LspCompletion, "Auto Complete"),
                Item(Command::LspHover, "Show Documentation"),
                Sep,
                Item(Command::ReloadPlugins, "Reload Plugins"),
            ],
        ),
        (
            "&Project",
            vec![
                Item(Command::OpenFolder, "Open Folder\u{2026}"),
                Item(Command::CloseFolder, "Close Folder"),
            ],
        ),
        (
            "P&references",
            vec![Item(Command::OpenSettings, "Settings")],
        ),
        ("&Help", vec![Item(Command::About, "About PaulWrite")]),
    ]
}

/// Raccourci affiché dans le menu (mêmes libellés que Sublime Text).
fn shortcut_text(cmd: Command) -> Option<String> {
    if cmd == Command::ToggleSidebar {
        // Enchaînement de deux touches, géré par l'application.
        return Some("Ctrl+K, Ctrl+B".to_owned());
    }
    shortcut_label(cmd).map(|s| {
        s.replace("Maj+", "Shift+")
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
            .replace('↓', "Down")
    })
}

/// Construit la barre de menus et la table identifiant → commande (l'identifiant d'une entrée est
/// `MENU_BASE` + son indice dans la table).
#[must_use]
pub fn build() -> (Vec<(String, Vec<MenuEntry>)>, Vec<Command>) {
    let mut table = Vec::new();
    let bar = menus()
        .into_iter()
        .map(|(title, lines)| {
            let entries = lines
                .into_iter()
                .map(|line| match line {
                    Line::Sep => MenuEntry::Separator,
                    Line::Item(cmd, label) => {
                        let id = MENU_BASE + table.len() as u32;
                        table.push(cmd);
                        let text = match shortcut_text(cmd) {
                            Some(s) => format!("{label}\t{s}"),
                            None => label.to_owned(),
                        };
                        MenuEntry::Item(id, text)
                    }
                })
                .collect();
            (title.to_owned(), entries)
        })
        .collect();
    (bar, table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_menus_sont_ceux_de_sublime_text() {
        let (bar, table) = build();
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
    fn les_raccourcis_affiches_sont_ceux_du_clavier() {
        let (bar, table) = build();
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
    }
}
