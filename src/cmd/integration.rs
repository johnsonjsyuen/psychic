//! What the picker and `--status` say about the shell integration: whether
//! Ctrl-R opens the picker in the user's shell, and how to get a pick onto
//! the prompt when `psychic history` was typed by hand
//! (`docs/ctrl-r/02-browse.md` section 11.4).
//!
//! The scripts in `shell/` export two variables this reads: `PSYCHIC_SHELL`
//! (the integration is loaded) and `PSYCHIC_CTRL_R` (what became of the
//! Ctrl-R binding when it was). Nothing here names a command's text.

/// The shell integration as the environment describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Integration<'a> {
    /// `PSYCHIC_SHELL`: `zsh`, `bash` or `fish` when the integration is loaded.
    pub shell: Option<&'a str>,
    /// `PSYCHIC_CTRL_R`: `bound`, `partly`, `taken`, `recording-off`,
    /// `opted-out` or `old-bash`; absent from a script older than this.
    pub ctrl_r: Option<&'a str>,
}

/// One shell's commands, quoted for the user to type.
struct ShellCommands {
    /// Shows what Ctrl-R runs now.
    check: &'static str,
    /// Binds the picker to another key.
    rebind: &'static str,
}

fn commands(shell: &str) -> Option<ShellCommands> {
    match shell {
        "zsh" => Some(ShellCommands {
            check: "bindkey -M emacs '^R'; bindkey -M viins '^R'",
            rebind: "bindkey '^X^R' __psychic_history_widget",
        }),
        "bash" => Some(ShellCommands {
            check: r"bind -X | grep -F '\C-r'",
            rebind: r#"bind -x '"\C-x\C-r": __psychic_history_widget'"#,
        }),
        "fish" => Some(ShellCommands {
            check: r"bind \cr",
            rebind: r"bind \cx\cr __psychic_history_widget",
        }),
        _ => None,
    }
}

/// The lines `--status` adds about the integration (section 11.4).
pub(super) fn status_lines(integration: Integration) -> Vec<String> {
    let Some(shell) = integration.shell else {
        return vec![
            "shell integration: not loaded in this shell; set it up with: psychic init".to_string(),
            "Ctrl-R: not psychic's here (it needs the shell integration)".to_string(),
        ];
    };
    let mut lines = vec![format!("shell integration: {shell}")];
    let ctrl_r = match integration.ctrl_r {
        Some("bound") => "psychic's picker, bound when this shell started; \
                          if it opens something else, a plugin rebound it later: load psychic last"
            .to_string(),
        Some("partly") => "psychic's picker in some editing modes only; \
                           something else had Ctrl-R in the others when this shell started"
            .to_string(),
        Some("taken") => "left alone: something else (fzf, Atuin, a plugin) had Ctrl-R \
                          when this shell started"
            .to_string(),
        Some("recording-off") => {
            "not bound: recording was off when this shell started; open a new shell".to_string()
        }
        Some("opted-out") => "not bound: PSYCHIC_NO_CTRL_R is set".to_string(),
        Some("old-bash") => "not bound: needs bash 4 or newer".to_string(),
        _ => "unknown: this shell loaded an older integration; open a new shell".to_string(),
    };
    lines.push(format!("Ctrl-R: {ctrl_r}"));
    if let Some(commands) = commands(shell) {
        lines.push(format!(
            "  see what Ctrl-R runs: {}   (psychic's is __psychic_history_widget)",
            commands.check
        ));
        if matches!(integration.ctrl_r, Some("partly" | "taken")) {
            lines.push(format!(
                "  bind psychic's to another key: {}",
                commands.rebind
            ));
        }
    }
    lines.push(typed_line(shell).to_string());
    lines
}

/// What `ph` does in `shell`.
fn typed_line(shell: &str) -> &'static str {
    match shell {
        "bash" => "ph [words]: the picker; Up then brings the pick back to edit and run",
        _ => "ph [words]: the picker; the pick goes on your prompt to edit and run",
    }
}

/// Said on stderr after the pick when stdout is a terminal: someone typed
/// `psychic history` at the prompt, where the pick can only be printed.
pub(super) fn by_hand_hint(integration: Integration) -> String {
    let how = match integration.shell {
        Some(_) => "press Ctrl-R or type ph instead",
        None => "set up the shell integration (psychic init), then press Ctrl-R or type ph",
    };
    format!(
        "psychic: printed only, not put on your prompt. To get a pick on the prompt, ready to run, \
         {how}. Ctrl-R opens something else? See psychic history --status"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(shell: &'static str, ctrl_r: Option<&'static str>) -> Integration<'static> {
        Integration {
            shell: Some(shell),
            ctrl_r,
        }
    }

    #[test]
    fn without_the_integration_status_says_to_set_it_up() {
        let lines = status_lines(Integration {
            shell: None,
            ctrl_r: Some("bound"),
        });
        assert_eq!(
            lines,
            [
                "shell integration: not loaded in this shell; set it up with: psychic init",
                "Ctrl-R: not psychic's here (it needs the shell integration)",
            ]
        );
    }

    #[test]
    fn a_bound_ctrl_r_gets_the_check_but_no_rebind_hint() {
        let lines = status_lines(loaded("zsh", Some("bound")));
        assert_eq!(lines[0], "shell integration: zsh");
        assert!(lines[1].starts_with("Ctrl-R: psychic's picker, bound when this shell started"));
        assert_eq!(
            lines[2],
            "  see what Ctrl-R runs: bindkey -M emacs '^R'; bindkey -M viins '^R'   \
             (psychic's is __psychic_history_widget)"
        );
        assert_eq!(
            lines[3],
            "ph [words]: the picker; the pick goes on your prompt to edit and run"
        );
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn a_taken_ctrl_r_says_how_to_bind_another_key() {
        for (shell, rebind) in [
            ("zsh", "bindkey '^X^R' __psychic_history_widget"),
            ("bash", r#"bind -x '"\C-x\C-r": __psychic_history_widget'"#),
            ("fish", r"bind \cx\cr __psychic_history_widget"),
        ] {
            for state in ["taken", "partly"] {
                let lines = status_lines(loaded(shell, Some(state)));
                assert!(
                    lines.contains(&format!("  bind psychic's to another key: {rebind}")),
                    "{shell} {state}: {lines:?}"
                );
            }
        }
        let bash = status_lines(loaded("bash", Some("taken")));
        assert_eq!(
            bash.last().unwrap(),
            "ph [words]: the picker; Up then brings the pick back to edit and run"
        );
    }

    #[test]
    fn every_state_has_its_own_explanation() {
        let states = [
            Some("bound"),
            Some("partly"),
            Some("taken"),
            Some("recording-off"),
            Some("opted-out"),
            Some("old-bash"),
            None,
        ];
        let explained: std::collections::HashSet<String> = states
            .iter()
            .map(|state| status_lines(loaded("bash", *state))[1].clone())
            .collect();
        assert_eq!(explained.len(), states.len());
        assert!(explained.contains("Ctrl-R: not bound: PSYCHIC_NO_CTRL_R is set"));
        assert!(
            explained.contains(
                "Ctrl-R: unknown: this shell loaded an older integration; open a new shell"
            )
        );
    }

    #[test]
    fn an_unknown_shell_gets_no_commands_to_type() {
        let lines = status_lines(loaded("tcsh", Some("bound")));
        assert_eq!(lines.len(), 3, "{lines:?}");
    }

    #[test]
    fn the_by_hand_hint_names_ctrl_r_ph_and_status() {
        let loaded_hint = by_hand_hint(loaded("zsh", None));
        let bare_hint = by_hand_hint(Integration {
            shell: None,
            ctrl_r: None,
        });
        for hint in [&loaded_hint, &bare_hint] {
            assert!(hint.starts_with("psychic: printed only"), "{hint}");
            assert!(hint.contains("Ctrl-R") && hint.contains(" ph"), "{hint}");
            assert!(hint.contains("psychic history --status"), "{hint}");
            assert!(!hint.contains('\n'), "one line: {hint}");
        }
        assert!(bare_hint.contains("psychic init"));
        assert!(!loaded_hint.contains("psychic init"));
    }
}
