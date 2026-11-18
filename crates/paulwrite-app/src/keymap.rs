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
            NamedKey::ArrowUp if ctrl && shift => Some(Command::MoveLineUp),
            NamedKey::ArrowDown if ctrl && shift => Some(Command::MoveLineDown),
            NamedKey::ArrowUp if ctrl && !shift => Some(Command::ScrollUp),
            NamedKey::ArrowDown if ctrl && !shift => Some(Command::ScrollDown),
            NamedKey::ArrowUp => Some(motion(Motion::Up, Some(Motion::ParagraphUp))),
            NamedKey::ArrowDown => Some(motion(Motion::Down, Some(Motion::ParagraphDown))),
            NamedKey::Home => Some(motion(Motion::LineStart, Some(Motion::DocStart))),
            NamedKey::End => Some(motion(Motion::LineEnd, Some(Motion::DocEnd))),
            NamedKey::PageUp => Some(Command::PageUp(shift)),
            NamedKey::PageDown => Some(Command::PageDown(shift)),
            NamedKey::Space if ctrl && !shift => Some(Command::LspCompletion),
            NamedKey::F3 if shift => Some(Command::FindPrevious),
            NamedKey::F3 => Some(Command::FindNext),
            NamedKey::F12 if !ctrl && !shift => Some(Command::LspDefinition),
            NamedKey::F1 if !ctrl && !shift => Some(Command::LspHover),
            NamedKey::Enter => Some(Command::InsertNewline),
            NamedKey::Tab if ctrl && shift => Some(Command::PrevTab),
            NamedKey::Tab if ctrl => Some(Command::NextTab),
            NamedKey::Tab if shift => Some(Command::Unindent),
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
            ("f", false) => Some(Command::Find),
            ("h", true) => Some(Command::ReplaceInProject),
            ("h", false) => Some(Command::Replace),
            ("d", false) => Some(Command::SelectNextOccurrence),
            ("d", true) => Some(Command::DuplicateLine),
            ("k", true) => Some(Command::DeleteLine),
            ("l", _) => Some(Command::SelectLine),
            // Ctrl+/ ; sur un clavier AZERTY la barre oblique est Maj+« : »
            ("/", _) | (":", true) => Some(Command::ToggleComment),
            // zoom : Ctrl+= ou Ctrl++ (AZERTY : « = » ou Maj+« = »), Ctrl+-, Ctrl+0 (AZERTY : « à »)
            ("=" | "+", _) => Some(Command::ZoomIn),
            ("-", _) => Some(Command::ZoomOut),
            ("0" | "\u{e0}", _) => Some(Command::ZoomReset),
            ("n", false) => Some(Command::NewFile),
            ("n", true) => Some(Command::NewWindow),
            ("t", true) => Some(Command::ReopenClosedTab),
            ("p", false) => Some(Command::GotoFile),
            ("p", true) => Some(Command::CommandPalette),
            ("g", _) => Some(Command::GotoLine),
            ("m", true) => Some(Command::LspDiagnostics),
            ("f", true) => Some(Command::SearchProject),
            ("w", _) => Some(Command::CloseTab),
            ("q", _) => Some(Command::Quit),
            // Replier / déplier (Ctrl+Maj+[ et Ctrl+Maj+], le Maj donne { et } selon la disposition)
            ("[" | "{", true) => Some(Command::Fold),
            ("]" | "}", true) => Some(Command::Unfold),
            _ => None,
        },
        Key::Character(s) if alt && !ctrl && !shift && s.to_lowercase() == "z" => {
            Some(Command::ToggleWordWrap)
        }
        _ => None,
    }
}

/// Raccourci clavier d'une commande, d'après la table [`command_for`] elle-même (jamais
/// désynchronisé) : `Ctrl+Maj+S`, `Ctrl+←`… `None` si la commande n'a pas de raccourci.
#[must_use]
pub fn shortcut_label(cmd: Command) -> Option<String> {
    use NamedKey::*;
    let named: [(NamedKey, &str); 17] = [
        (Space, "Espace"),
        (F1, "F1"),
        (F3, "F3"),
        (F12, "F12"),
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
    keys.extend(
        ["[", "]"]
            .iter()
            .map(|c| (Key::Character((*c).into()), (*c).to_owned())),
    );
    // Du plus simple au plus composé : on préfère `Ctrl+S` à `Ctrl+Maj+S`.
    let combos: [(ModifiersState, &str); 6] = [
        (ModifiersState::empty(), ""),
        (ModifiersState::ALT, "Alt+"),
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
    fn raccourcis_du_langage() {
        let c = ModifiersState::CONTROL;
        assert_eq!(
            command_for(&named(NamedKey::Space), c),
            Some(Command::LspCompletion)
        );
        assert_eq!(
            command_for(&named(NamedKey::Space), NONE),
            None,
            "l'espace seul est du texte"
        );
        assert_eq!(
            command_for(&named(NamedKey::F12), NONE),
            Some(Command::LspDefinition)
        );
        assert_eq!(
            command_for(&named(NamedKey::F1), NONE),
            Some(Command::LspHover)
        );
        assert_eq!(
            command_for(&ch("M"), c | ModifiersState::SHIFT),
            Some(Command::LspDiagnostics)
        );
        assert_eq!(
            shortcut_label(Command::LspCompletion).as_deref(),
            Some("Ctrl+Espace")
        );
        assert_eq!(
            shortcut_label(Command::LspDefinition).as_deref(),
            Some("F12")
        );
    }

    #[test]
    fn remplacer_dans_le_projet() {
        let c = ModifiersState::CONTROL;
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let h = Key::Character("h".into());
        assert_eq!(command_for(&h, c), Some(Command::Replace));
        assert_eq!(command_for(&h, cs), Some(Command::ReplaceInProject));
    }

    #[test]
    fn raccourcis_du_zoom() {
        let ctrl = ModifiersState::CONTROL;
        let ch = |c: &str| Key::Character(c.into());
        for k in ["=", "+"] {
            assert_eq!(command_for(&ch(k), ctrl), Some(Command::ZoomIn), "{k}");
        }
        assert_eq!(command_for(&ch("-"), ctrl), Some(Command::ZoomOut));
        assert_eq!(command_for(&ch("0"), ctrl), Some(Command::ZoomReset));
        // sans Ctrl, ce sont des caractères comme les autres
        assert_eq!(command_for(&ch("-"), ModifiersState::empty()), None);
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
            Some(Command::Find),
            "Ctrl+F : recherche dans le document"
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
        assert_eq!(
            command_for(&ch("n"), c | ModifiersState::SHIFT),
            Some(Command::NewWindow)
        );
        assert_eq!(
            shortcut_label(Command::NewWindow).as_deref(),
            Some("Ctrl+Maj+N")
        );
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

    #[test]
    fn raccourcis_d_edition() {
        let c = ModifiersState::CONTROL;
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            command_for(&ch("d"), c),
            Some(Command::SelectNextOccurrence)
        );
        assert_eq!(command_for(&ch("d"), cs), Some(Command::DuplicateLine));
        assert_eq!(command_for(&ch("k"), cs), Some(Command::DeleteLine));
        assert_eq!(command_for(&ch("l"), c), Some(Command::SelectLine));
        assert_eq!(command_for(&ch("/"), c), Some(Command::ToggleComment));
        assert_eq!(command_for(&ch(":"), cs), Some(Command::ToggleComment));
        assert_eq!(
            command_for(&named(NamedKey::ArrowUp), cs),
            Some(Command::MoveLineUp)
        );
        assert_eq!(
            command_for(&named(NamedKey::ArrowDown), cs),
            Some(Command::MoveLineDown)
        );
        assert_eq!(
            command_for(&named(NamedKey::Tab), ModifiersState::SHIFT),
            Some(Command::Unindent)
        );
        // inchangés : Tab, Ctrl+Tab, Ctrl+Haut (défilement), Ctrl+K seul (début d'un enchaînement)
        assert_eq!(
            command_for(&named(NamedKey::Tab), ModifiersState::empty()),
            Some(Command::InsertTab)
        );
        assert_eq!(
            command_for(&named(NamedKey::Tab), c),
            Some(Command::NextTab)
        );
        assert_eq!(
            command_for(&named(NamedKey::ArrowUp), c),
            Some(Command::ScrollUp)
        );
        assert_eq!(command_for(&ch("k"), c), None);
        assert_eq!(
            shortcut_label(Command::DuplicateLine).as_deref(),
            Some("Ctrl+Maj+D")
        );
        assert_eq!(
            shortcut_label(Command::SelectNextOccurrence).as_deref(),
            Some("Ctrl+D")
        );
    }
}
