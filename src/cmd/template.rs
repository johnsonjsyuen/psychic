//! Command templates: the command text with path-like arguments replaced by
//! `PATH`, so `cd foo` and `cd bar` share one template.
//!
//! The upstream tokenizer in `import/shell_history.rs` is deliberately not
//! reused: it drops quotes and blanks out any word holding `$`, a glob or a
//! redirection, while a template has to keep such words verbatim. The splitting
//! here is a heuristic, not a shell parser; `docs/ctrl-r/01-record.md` section
//! 7 specifies it exactly, with a table of worked examples tested below.

use super::tuning::{CMD_OWN_COMMANDS, CMD_PATH_ARG_COMMANDS};

/// What a [`RawWord`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WordKind {
    Word,
    /// `;`, `|`, `||`, `&`, `&&`, or a newline (which is reported as `;`).
    Operator,
}

/// One word or operator, as a slice of the input (a newline operator is the
/// static string `;`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RawWord<'a> {
    pub text: &'a str,
    pub kind: WordKind,
}

/// Split `text` into words and operators, keeping quotes and escapes inside
/// the words they belong to. One pass, no allocation per word.
pub(super) fn raw_words<'a>(text: &'a str) -> Vec<RawWord<'a>> {
    let mut words = Vec::new();
    each_raw_word(text, |word| words.push(word));
    words
}

/// [`raw_words`], handing each word to `visit` in order instead of
/// collecting them.
fn each_raw_word<'a>(text: &'a str, mut visit: impl FnMut(RawWord<'a>)) {
    let mut start: Option<usize> = None;
    let mut chars = text.char_indices().peekable();

    let end_word = |visit: &mut dyn FnMut(RawWord<'a>), start: &mut Option<usize>, end: usize| {
        if let Some(begin) = start.take() {
            visit(RawWord {
                text: &text[begin..end],
                kind: WordKind::Word,
            });
        }
    };

    while let Some((index, c)) = chars.next() {
        match c {
            ' ' | '\t' => end_word(&mut visit, &mut start, index),
            '&' if start.is_some_and(|begin| text[begin..index].ends_with(['<', '>'])) => {
                // `2>&1`, `>&2`: part of the redirection word.
            }
            '\n' | ';' | '|' | '&' => {
                end_word(&mut visit, &mut start, index);
                let doubled = matches!(c, '|' | '&') && chars.peek().is_some_and(|&(_, n)| n == c);
                let operator = if c == '\n' {
                    ";"
                } else if doubled {
                    chars.next();
                    &text[index..index + 2]
                } else {
                    &text[index..index + 1]
                };
                visit(RawWord {
                    text: operator,
                    kind: WordKind::Operator,
                });
            }
            '\'' => {
                start.get_or_insert(index);
                for (_, next) in chars.by_ref() {
                    if next == '\'' {
                        break;
                    }
                }
            }
            '"' => {
                start.get_or_insert(index);
                while let Some((_, next)) = chars.next() {
                    match next {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '\\' => {
                start.get_or_insert(index);
                chars.next();
            }
            _ => {
                start.get_or_insert(index);
            }
        }
    }
    end_word(&mut visit, &mut start, text.len());
}

/// `^[A-Za-z_][A-Za-z0-9_]*=`: a `NAME=value` assignment in front of a command.
fn is_assignment(word: &str) -> bool {
    let Some((name, _value)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Words that sit in front of the real command word.
fn is_prefix_word(word: &str) -> bool {
    is_assignment(word) || matches!(word, "command" | "builtin" | "sudo")
}

/// `^[0-9]*[<>]+&?[0-9-]*$`: `>`, `2>`, `>>`, `2>&1`, `<`.
fn is_redirection(word: &str) -> bool {
    let rest = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let after_arrows = rest.trim_start_matches(['<', '>']);
    if after_arrows.len() == rest.len() {
        return false;
    }
    let after_amp = after_arrows.strip_prefix('&').unwrap_or(after_arrows);
    after_amp.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// `^[A-Za-z0-9_.-]+\.[A-Za-z][A-Za-z0-9]{0,4}$`: a bare file name whose
/// extension starts with a letter (so `v1.2` is not one).
fn is_file_name(word: &str) -> bool {
    let Some((stem, extension)) = word.rsplit_once('.') else {
        return false;
    };
    let stem_ok = !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    let mut ext = extension.chars();
    let ext_ok = (1..=5).contains(&extension.len())
        && ext.next().is_some_and(|c| c.is_ascii_alphabetic())
        && ext.all(|c| c.is_ascii_alphanumeric());
    stem_ok && ext_ok
}

/// The word with one pair of enclosing `'` or `"` removed.
fn unquoted(word: &str) -> &str {
    for quote in ['\'', '"'] {
        if word.len() >= 2 && word.starts_with(quote) && word.ends_with(quote) {
            return &word[1..word.len() - 1];
        }
    }
    word
}

/// Whether `word`, an argument of `command_word`, is a path (rules P1-P5).
fn is_path_like(word: &str, command_word: &str) -> bool {
    let word = unquoted(word);
    if word.starts_with('-') || is_redirection(word) {
        return false;
    }
    CMD_PATH_ARG_COMMANDS.contains(&command_word)
        || word.contains('/')
        || word.starts_with('~')
        || word == "."
        || word == ".."
        || is_file_name(word)
}

/// One entry of [`classify`]: a word or operator, and whether it is a path.
struct Classified<'a> {
    word: RawWord<'a>,
    is_command_word: bool,
    is_path: bool,
}

/// Walk the words of `text`, marking each simple command's command word and
/// the path-like words after it.
fn classify(text: &str) -> Vec<Classified<'_>> {
    let mut out = Vec::new();
    let mut command_word: Option<&str> = None;
    for word in raw_words(text) {
        let mut entry = Classified {
            word,
            is_command_word: false,
            is_path: false,
        };
        match (word.kind, command_word) {
            (WordKind::Operator, _) => command_word = None,
            (WordKind::Word, None) => {
                if !is_prefix_word(word.text) {
                    command_word = Some(word.text);
                    entry.is_command_word = true;
                }
            }
            (WordKind::Word, Some(command)) => entry.is_path = is_path_like(word.text, command),
        }
        out.push(entry);
    }
    out
}

/// The command text with path-like arguments replaced by `PATH` and runs of
/// `PATH PATH` collapsed to one; words joined by one space.
pub(super) fn templatize(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for entry in classify(text.trim()) {
        if entry.is_path {
            if out.last() != Some(&"PATH") {
                out.push("PATH");
            }
        } else {
            out.push(entry.word.text);
        }
    }
    out.join(" ")
}

/// The command word of the first simple command: what actually runs, after
/// any `NAME=value`, `command`, `builtin` or `sudo` in front.
pub(super) fn first_command_word(text: &str) -> Option<&str> {
    classify(text)
        .into_iter()
        .take_while(|entry| entry.word.kind == WordKind::Word)
        .find(|entry| entry.is_command_word)
        .map(|entry| entry.word.text)
}

/// Whether `text` runs psychic itself (`psychic`, `p`, `pd`, `pc`), which is
/// never recorded.
pub(super) fn is_own_command(text: &str) -> bool {
    first_command_word(text).is_some_and(|word| CMD_OWN_COMMANDS.contains(&word))
}

/// How many words (not operators) `text` has.
pub(super) fn word_count(text: &str) -> usize {
    let mut words = 0;
    each_raw_word(text, |word| {
        words += usize::from(word.kind == WordKind::Word)
    });
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_match_the_table_in_the_spec() {
        let cases = [
            ("cd foo", "cd PATH"),
            ("cd ~/src/psychic", "cd PATH"),
            ("git status", "git status"),
            ("git add src/main.rs Cargo.toml", "git add PATH"),
            ("vim -p a.rs b.rs", "vim -p PATH"),
            (
                "cargo test --test shell_hooks",
                "cargo test --test shell_hooks",
            ),
            ("FOO=1 make -C build all", "FOO=1 make -C build all"),
            (
                "cat foo | grep \"a b\" > out.txt",
                "cat PATH | grep \"a b\" > PATH",
            ),
            ("echo 'multi\nline'", "echo 'multi\nline'"),
            ("ls\npwd", "ls ; pwd"),
            ("git push origin v1.2", "git push origin v1.2"),
            ("curl https://example.com/x", "curl PATH"),
            ("cat a > b", "cat PATH > PATH"),
            ("make test 2>&1", "make test 2>&1"),
            ("echo $(date) done", "echo $(date) done"),
            ("sudo -u bob ls /x", "sudo -u bob ls PATH"),
        ];
        for (text, template) in cases {
            assert_eq!(templatize(text), template, "template of {text:?}");
        }
    }

    #[test]
    fn templatize_trims_and_squeezes_whitespace() {
        assert_eq!(templatize("  git   log \t -1  "), "git log -1");
    }

    #[test]
    fn adjacent_operators_without_spaces_split_the_words() {
        let words: Vec<(&str, WordKind)> = raw_words("a||b && c;d")
            .into_iter()
            .map(|word| (word.text, word.kind))
            .collect();
        assert_eq!(
            words,
            vec![
                ("a", WordKind::Word),
                ("||", WordKind::Operator),
                ("b", WordKind::Word),
                ("&&", WordKind::Operator),
                ("c", WordKind::Word),
                (";", WordKind::Operator),
                ("d", WordKind::Word),
            ]
        );
    }

    #[test]
    fn quotes_and_escapes_stay_inside_their_word() {
        let words: Vec<&str> = raw_words(r#"echo "a \" b" 'c d'e f\ g"#)
            .into_iter()
            .map(|word| word.text)
            .collect();
        assert_eq!(words, vec!["echo", r#""a \" b""#, "'c d'e", r"f\ g"]);
    }

    #[test]
    fn an_unclosed_quote_runs_to_the_end() {
        let words: Vec<&str> = raw_words("echo 'abc def")
            .into_iter()
            .map(|word| word.text)
            .collect();
        assert_eq!(words, vec!["echo", "'abc def"]);
    }

    #[test]
    fn the_command_word_skips_assignments_and_prefixes() {
        assert_eq!(
            first_command_word("FOO=1 command psychic zsh"),
            Some("psychic")
        );
        assert_eq!(first_command_word("sudo pd x"), Some("pd"));
        assert_eq!(first_command_word("echo psychic"), Some("echo"));
        assert_eq!(first_command_word("FOO=1"), None);
        assert_eq!(first_command_word(""), None);
    }

    #[test]
    fn own_commands_are_recognised_by_their_first_word_only() {
        assert!(is_own_command("p"));
        assert!(is_own_command("pd foo"));
        assert!(is_own_command("psychic history"));
        assert!(!is_own_command("echo psychic"));
        assert!(!is_own_command("python"));
    }

    #[test]
    fn word_count_ignores_operators() {
        assert_eq!(word_count("git add a b && make"), 5);
        assert_eq!(word_count(""), 0);
    }

    #[test]
    fn redirections_and_file_names_are_recognised() {
        for word in [">", "2>", ">>", "2>&1", "<", ">&2", "1>&-"] {
            assert!(is_redirection(word), "{word} is a redirection");
        }
        for word in ["a>", "x", "2", "&>"] {
            assert!(!is_redirection(word), "{word} is not a redirection");
        }
        for word in ["main.rs", "Cargo.toml", "a.b.tar", "x-y_z.md"] {
            assert!(is_file_name(word), "{word} is a file name");
        }
        for word in ["v1.2", ".rs", "a.toolong", "a.", "a/b.rs"] {
            assert!(!is_file_name(word), "{word} is not a file name");
        }
    }
}
