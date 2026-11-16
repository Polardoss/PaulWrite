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
            ("q", _) => Some(Command::Quit),
            _ => None,
        },
        _ => None,
    }
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
