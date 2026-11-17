//! Raccourcis clavier → commandes. Données pures : testable sans fenêtre.

use paulwrite_core::Motion;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::command::Command;

/// Commande associée à une touche. `key` est la touche **sans** effet des modificateurs
/// (Ctrl+Z donne `z`, pas un caractère de contrôle).
#[must_use]
pub fn command_for(key: &Key, mods: ModifiersState) -> Option<Command> {
    let ctrl = mods.control_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();

    let motion = |plain: Motion, ctrl_variant: Option<Motion>| -> Command {
        let m = if ctrl {
            ctrl_variant.unwrap_or(plain)
        } else {
            plain
        };
        if shift {
            Command::Extend(m)
        } else {
            Command::Move(m)
        }
    };

    match key {
        Key::Named(named) => match named {
            NamedKey::ArrowLeft => Some(motion(Motion::Left, Some(Motion::WordLeft))),
            NamedKey::ArrowRight => Some(motion(Motion::Right, Some(Motion::WordRight))),
            NamedKey::ArrowUp if ctrl && alt => Some(Command::AddCursorAbove),
            NamedKey::ArrowDown if ctrl && alt => Some(Command::AddCursorBelow),
            NamedKey::ArrowUp if ctrl && !shift => Some(Command::ScrollUp),
            NamedKey::ArrowDown if ctrl && !shift => Some(Command::ScrollDown),
            NamedKey::ArrowUp => Some(motion(Motion::Up, Some(Motion::ParagraphUp))),
            NamedKey::ArrowDown => Some(motion(Motion::Down, Some(Motion::ParagraphDown))),
            NamedKey::Home => Some(motion(Motion::LineStart, Some(Motion::DocStart))),
            NamedKey::End => Some(motion(Motion::LineEnd, Some(Motion::DocEnd))),
            NamedKey::PageUp => Some(Command::PageUp(shift)),
            NamedKey::PageDown => Some(Command::PageDown(shift)),
            NamedKey::Enter => Some(Command::InsertNewline),
            NamedKey::Tab if ctrl && shift => Some(Command::PrevTab),
            NamedKey::Tab if ctrl => Some(Command::NextTab),
            NamedKey::Tab if !shift => Some(Command::InsertTab),
            NamedKey::Backspace => Some(Command::DeleteBackward),
            NamedKey::Delete => Some(Command::DeleteForward),
            NamedKey::Escape => Some(Command::CollapseSelections),
            _ => None,
        },
        Key::Character(s) if ctrl && !alt => match (s.to_lowercase().as_str(), shift) {
            ("z", false) => Some(Command::Undo),
            ("z", true) | ("y", _) => Some(Command::Redo),
            ("a", _) => Some(Command::SelectAll),
            ("c", _) => Some(Command::Copy),
            ("x", _) => Some(Command::Cut),
            ("v", _) => Some(Command::Paste),
            ("s", false) => Some(Command::Save),
            ("s", true) => Some(Command::SaveAs),
            ("o", _) => Some(Command::Open),
            ("n", _) => Some(Command::NewFile),
            ("p", false) => Some(Command::GotoFile),
            ("p", true) => Some(Command::CommandPalette),
            ("g", _) => Some(Command::GotoLine),
            ("f", true) => Some(Command::SearchProject),
            ("w", _) => Some(Command::CloseTab),
            ("q", _) => Some(Command::Quit),
            _ => None,
        },
        _ => None,
    }
}

/// Raccourci clavier d'une commande, d'après la table [`command_for`] elle-même (jamais
/// désynchronisé) : `Ctrl+Maj+S`, `Ctrl+←`… `None` si la commande n'a pas de raccourci.
#[must_use]
pub fn shortcut_label(cmd: Command) -> Option<String> {
    use NamedKey::*;
    let named: [(NamedKey, &str); 13] = [
        (ArrowLeft, "←"),
        (ArrowRight, "→"),
        (ArrowUp, "↑"),
        (ArrowDown, "↓"),
        (Home, "Début"),
        (End, "Fin"),
        (PageUp, "Page ↑"),
        (PageDown, "Page ↓"),
        (Enter, "Entrée"),
        (Tab, "Tab"),
        (Backspace, "Retour arrière"),
        (Delete, "Suppr"),
        (Escape, "Échap"),
    ];
    let mut keys: Vec<(Key, String)> = named
        .iter()
        .map(|(k, l)| (Key::Named(*k), (*l).to_owned()))
        .collect();
    keys.extend(('a'..='z').map(|c| {
        (
            Key::Character(c.to_string().into()),
            c.to_ascii_uppercase().to_string(),
        )
    }));
    // Du plus simple au plus composé : on préfère `Ctrl+S` à `Ctrl+Maj+S`.
    let combos: [(ModifiersState, &str); 5] = [
        (ModifiersState::empty(), ""),
        (ModifiersState::CONTROL, "Ctrl+"),
        (ModifiersState::SHIFT, "Maj+"),
        (ModifiersState::CONTROL | ModifiersState::SHIFT, "Ctrl+Maj+"),
        (ModifiersState::CONTROL | ModifiersState::ALT, "Ctrl+Alt+"),
    ];
    for (mods, prefix) in combos {
        for (key, label) in &keys {
            // une lettre seule sans modificateur est du texte, pas un raccourci
            if mods.is_empty() && matches!(key, Key::Character(_)) {
                continue;
            }
            if command_for(key, mods) == Some(cmd) {
                return Some(format!("{prefix}{label}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(n: NamedKey) -> Key {
        Key::Named(n)
    }
    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }
    const NONE: ModifiersState = ModifiersState::empty();

    #[test]
    fn fleches_et_modificateurs() {
        assert_eq!(
            command_for(&named(NamedKey::ArrowLeft), NONE),
            Some(Command::Move(Motion::Left))
        );
        assert_eq!(
            command_for(&named(NamedKey::ArrowLeft), ModifiersState::CONTROL),
            Some(Command::Move(Motion::WordLeft))
        );
        assert_eq!(
            command_for(
                &named(NamedKey::ArrowRight),
                ModifiersState::CONTROL | ModifiersState::SHIFT
            ),
            Some(Command::Extend(Motion::WordRight))
        );
        assert_eq!(
            command_for(&named(NamedKey::End), ModifiersState::CONTROL),
            Some(Command::Move(Motion::DocEnd))
        );
        assert_eq!(
            command_for(
                &named(NamedKey::ArrowDown),
                ModifiersState::CONTROL | ModifiersState::ALT
            ),
            Some(Command::AddCursorBelow)
        );
        assert_eq!(
            command_for(&named(NamedKey::ArrowDown), ModifiersState::CONTROL),
            Some(Command::ScrollDown)
        );
    }

    #[test]
    fn raccourcis_ctrl() {
        let c = ModifiersState::CONTROL;
        assert_eq!(command_for(&ch("z"), c), Some(Command::Undo));
        assert_eq!(
            command_for(&ch("Z"), c | ModifiersState::SHIFT),
            Some(Command::Redo)
        );
        assert_eq!(command_for(&ch("y"), c), Some(Command::Redo));
        assert_eq!(command_for(&ch("s"), c), Some(Command::Save));
        assert_eq!(
            command_for(&ch("s"), c | ModifiersState::SHIFT),
            Some(Command::SaveAs)
        );
        assert_eq!(command_for(&ch("v"), c), Some(Command::Paste));
        // sans Ctrl, une lettre est du texte, pas une commande
        assert_eq!(command_for(&ch("z"), NONE), None);
        // AltGr (Ctrl+Alt) ne doit pas déclencher de raccourci : il sert à saisir des caractères
        assert_eq!(command_for(&ch("s"), c | ModifiersState::ALT), None);
    }

    #[test]
    fn intitules_de_raccourcis() {
        assert_eq!(shortcut_label(Command::Undo).as_deref(), Some("Ctrl+Z"));
        assert_eq!(
            shortcut_label(Command::SaveAs).as_deref(),
            Some("Ctrl+Maj+S")
        );
        assert_eq!(shortcut_label(Command::GotoFile).as_deref(), Some("Ctrl+P"));
        assert_eq!(
            shortcut_label(Command::CommandPalette).as_deref(),
            Some("Ctrl+Maj+P")
        );
        assert_eq!(
            shortcut_label(Command::Move(Motion::Left)).as_deref(),
            Some("←")
        );
        assert_eq!(
            shortcut_label(Command::Move(Motion::WordLeft)).as_deref(),
            Some("Ctrl+←")
        );
        assert_eq!(
            shortcut_label(Command::AddCursorBelow).as_deref(),
            Some("Ctrl+Alt+↓")
        );
        assert_eq!(shortcut_label(Command::Quit).as_deref(), Some("Ctrl+Q"));
        // pas de raccourci : paragraphe précédent sans sélection
        assert_eq!(shortcut_label(Command::Move(Motion::ParagraphUp)), None);
    }

    #[test]
    fn raccourcis_du_panneau() {
        let c = ModifiersState::CONTROL;
        assert_eq!(command_for(&ch("p"), c), Some(Command::GotoFile));
        assert_eq!(
            command_for(&ch("P"), c | ModifiersState::SHIFT),
            Some(Command::CommandPalette)
        );
        assert_eq!(command_for(&ch("g"), c), Some(Command::GotoLine));
        assert_eq!(
            command_for(&ch("F"), c | ModifiersState::SHIFT),
            Some(Command::SearchProject)
        );
        assert_eq!(
            command_for(&ch("f"), c),
            None,
            "Ctrl+F : recherche dans le document, plus tard"
        );
    }

    #[test]
    fn raccourcis_des_onglets() {
        let c = ModifiersState::CONTROL;
        assert_eq!(
            command_for(&named(NamedKey::Tab), c),
            Some(Command::NextTab)
        );
        assert_eq!(
            command_for(&named(NamedKey::Tab), c | ModifiersState::SHIFT),
            Some(Command::PrevTab)
        );
        assert_eq!(
            command_for(&named(NamedKey::Tab), NONE),
            Some(Command::InsertTab)
        );
        assert_eq!(command_for(&ch("w"), c), Some(Command::CloseTab));
        assert_eq!(command_for(&ch("n"), c), Some(Command::NewFile));
    }

    #[test]
    fn toutes_les_commandes_du_clavier_existent_dans_le_registre() {
        let all = Command::all();
        for cmd in [
            command_for(&named(NamedKey::Enter), NONE),
            command_for(&named(NamedKey::Backspace), NONE),
            command_for(&named(NamedKey::PageDown), NONE),
            command_for(&ch("o"), ModifiersState::CONTROL),
        ] {
            assert!(all.contains(&cmd.unwrap()));
        }
    }
}
