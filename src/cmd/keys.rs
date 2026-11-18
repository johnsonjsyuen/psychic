//! The picker's keys (`docs/ctrl-r/02-browse.md` section 9).
//!
//! A table of its own (ADR-3): `keymap::KEYMAP` has one row per file-mode
//! `Action`, enforced by its tests, and feeds the file-mode help screen.
//! The trigger types and their names are shared with it.

use crate::keymap::{Chord, Trigger};
use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum_macros::EnumIter)]
pub(super) enum CmdAction {
    AppendToQuery,
    DeleteFromQuery,
    ClearQuery,
    MoveUp,
    MoveDown,
    Accept,
    ScopeForward,
    ScopeBackward,
    DeleteSelected,
    ToggleDebug,
    ToggleHelp,
    Cancel,
}

/// One row of the table: what fires it and the help text.
pub(super) struct CmdBinding {
    pub action: CmdAction,
    pub triggers: &'static [Trigger],
    pub description: &'static str,
}

const fn key(code: KeyCode) -> Trigger {
    Trigger::Key(Chord {
        code,
        ctrl: false,
        alt: false,
    })
}

const fn ctrl_key(code: KeyCode) -> Trigger {
    Trigger::Key(Chord {
        code,
        ctrl: true,
        alt: false,
    })
}

const fn ctrl(c: char) -> Trigger {
    ctrl_key(KeyCode::Char(c))
}

/// Every key the picker responds to, in help-screen order.
pub(super) static CMD_KEYMAP: &[CmdBinding] = &[
    CmdBinding {
        action: CmdAction::AppendToQuery,
        triggers: &[Trigger::AnyChar],
        description: "type to search",
    },
    CmdBinding {
        action: CmdAction::DeleteFromQuery,
        triggers: &[key(KeyCode::Backspace)],
        description: "delete the last character",
    },
    CmdBinding {
        action: CmdAction::ClearQuery,
        triggers: &[ctrl('u')],
        description: "clear the query",
    },
    CmdBinding {
        action: CmdAction::MoveUp,
        triggers: &[
            key(KeyCode::Up),
            ctrl('p'),
            Trigger::Mouse(MouseEventKind::ScrollUp),
        ],
        description: "previous command",
    },
    CmdBinding {
        action: CmdAction::MoveDown,
        triggers: &[
            key(KeyCode::Down),
            ctrl('n'),
            Trigger::Mouse(MouseEventKind::ScrollDown),
        ],
        description: "next command",
    },
    CmdBinding {
        action: CmdAction::Accept,
        triggers: &[key(KeyCode::Enter)],
        description: "put the command on your prompt",
    },
    CmdBinding {
        action: CmdAction::ScopeForward,
        triggers: &[key(KeyCode::Tab)],
        description: "next scope: all, this directory, this repo, this session",
    },
    CmdBinding {
        action: CmdAction::ScopeBackward,
        triggers: &[key(KeyCode::BackTab)],
        description: "previous scope",
    },
    CmdBinding {
        action: CmdAction::DeleteSelected,
        triggers: &[key(KeyCode::Delete), ctrl('x')],
        description: "delete this command from psychic's history",
    },
    CmdBinding {
        action: CmdAction::ToggleDebug,
        triggers: &[ctrl('o')],
        description: "show why this command ranks here",
    },
    CmdBinding {
        action: CmdAction::ToggleHelp,
        triggers: &[ctrl('g'), key(KeyCode::F(1))],
        description: "show this help",
    },
    CmdBinding {
        action: CmdAction::Cancel,
        triggers: &[key(KeyCode::Esc), ctrl('c'), ctrl('d')],
        description: "leave without choosing",
    },
];

/// Shift is ignored: crossterm puts it in the character (and in `BackTab`).
fn chord_matches(chord: &Chord, code: KeyCode, modifiers: KeyModifiers) -> bool {
    chord.code == code
        && chord.ctrl == modifiers.contains(KeyModifiers::CONTROL)
        && chord.alt == modifiers.contains(KeyModifiers::ALT)
}

/// A printable character with neither Ctrl nor Alt.
fn is_plain_char(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char(_))
        && !modifiers.contains(KeyModifiers::CONTROL)
        && !modifiers.contains(KeyModifiers::ALT)
}

/// The action of a key: explicit chords first, then `AnyChar` for a plain char.
pub(super) fn lookup(code: KeyCode, modifiers: KeyModifiers) -> Option<CmdAction> {
    let chord = CMD_KEYMAP.iter().find(|binding| {
        binding.triggers.iter().any(|trigger| match trigger {
            Trigger::Key(chord) => chord_matches(chord, code, modifiers),
            _ => false,
        })
    });
    if let Some(binding) = chord {
        return Some(binding.action);
    }
    if is_plain_char(code, modifiers) {
        return find(&Trigger::AnyChar);
    }
    None
}

/// The action of a mouse event.
pub(super) fn lookup_mouse(kind: MouseEventKind) -> Option<CmdAction> {
    find(&Trigger::Mouse(kind))
}

fn find(trigger: &Trigger) -> Option<CmdAction> {
    CMD_KEYMAP
        .iter()
        .find(|binding| binding.triggers.contains(trigger))
        .map(|binding| binding.action)
}

/// The key column of the help overlay, e.g. `Up / Ctrl-P / Wheel up`.
pub(super) fn keys_of(binding: &CmdBinding) -> String {
    binding
        .triggers
        .iter()
        .map(crate::keymap::describe_trigger)
        .collect::<Vec<_>>()
        .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn tc_b_11_every_action_has_one_row_and_no_trigger_is_shared() {
        for action in CmdAction::iter() {
            let rows = CMD_KEYMAP.iter().filter(|b| b.action == action).count();
            assert_eq!(rows, 1, "{action:?} has {rows} rows");
        }
        let triggers: Vec<&Trigger> = CMD_KEYMAP.iter().flat_map(|b| b.triggers).collect();
        for (i, trigger) in triggers.iter().enumerate() {
            assert!(
                !triggers[i + 1..].contains(trigger),
                "{trigger:?} is bound twice"
            );
        }
        assert!(
            CMD_KEYMAP
                .iter()
                .all(|b| !b.description.is_empty() && !b.triggers.is_empty())
        );
        assert_eq!(CMD_KEYMAP.len(), 12);
    }

    #[test]
    fn tc_b_11_lookup_prefers_chords_over_typing() {
        assert_eq!(
            lookup(KeyCode::Char('x'), KeyModifiers::CONTROL),
            Some(CmdAction::DeleteSelected)
        );
        assert_eq!(
            lookup(KeyCode::Char('x'), KeyModifiers::NONE),
            Some(CmdAction::AppendToQuery)
        );
        assert_eq!(lookup(KeyCode::Char('q'), KeyModifiers::CONTROL), None);
        assert_eq!(
            lookup(KeyCode::Char('X'), KeyModifiers::SHIFT),
            Some(CmdAction::AppendToQuery)
        );
        assert_eq!(lookup(KeyCode::Char('x'), KeyModifiers::ALT), None);
        assert_eq!(
            lookup(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(CmdAction::ScopeBackward)
        );
        assert_eq!(
            lookup(KeyCode::F(1), KeyModifiers::NONE),
            Some(CmdAction::ToggleHelp)
        );
        assert_eq!(
            lookup(KeyCode::Esc, KeyModifiers::NONE),
            Some(CmdAction::Cancel)
        );
        assert_eq!(
            lookup_mouse(MouseEventKind::ScrollDown),
            Some(CmdAction::MoveDown)
        );
        assert_eq!(lookup_mouse(MouseEventKind::Moved), None);
    }

    #[test]
    fn help_names_the_keys() {
        let up = CMD_KEYMAP
            .iter()
            .find(|b| b.action == CmdAction::MoveUp)
            .unwrap();
        assert_eq!(keys_of(up), "Up / Ctrl-P / Wheel up");
        let scope = CMD_KEYMAP
            .iter()
            .find(|b| b.action == CmdAction::ScopeBackward)
            .unwrap();
        assert_eq!(keys_of(scope), "Shift-Tab");
    }
}
