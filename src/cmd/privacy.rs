//! What is recorded, who can read it, and how it is forgotten.
//!
//! * Opt-in: the marker file the shell hooks test (`recording_enabled` is the
//!   same rule, for display).
//! * `PSYCHIC_HISTORY_IGNORE`: glob patterns over the whole command text.
//! * Permissions: the data dir `0700`, the databases and model files `0600`.
//! * Tombstones: a deleted command's FNV-1a hash, so an import does not bring
//!   it back.
//!
//! See `docs/ctrl-r/01-record.md` section 10.

use super::tuning::CMD_MARKER_FILE;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The opt-in marker file in `data_dir`.
pub(super) fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CMD_MARKER_FILE)
}

/// Whether the shell hooks record commands, by the same rule they use:
/// `PSYCHIC_HISTORY` (`env`) of `1` or `0` wins, else the marker decides.
#[allow(dead_code, reason = "the picker's hint (Phase B)")]
pub(super) fn recording_enabled(data_dir: &Path, env: Option<&str>) -> bool {
    match env {
        Some("1") => true,
        Some("0") => false,
        _ => marker_path(data_dir).exists(),
    }
}

// ---------------------------------------------------------------------------
// PSYCHIC_HISTORY_IGNORE
// ---------------------------------------------------------------------------

/// Whether any of the colon-separated glob `patterns` matches the whole of
/// `text`. Empty patterns are skipped, so `""` and `":"` match nothing.
pub(super) fn is_ignored(text: &str, patterns: Option<&str>) -> bool {
    patterns.is_some_and(|patterns| {
        patterns
            .split(':')
            .filter(|pattern| !pattern.is_empty())
            .any(|pattern| glob_match(pattern, text))
    })
}

/// One element of a parsed glob.
#[derive(Debug, PartialEq)]
enum GlobToken {
    /// `*`: any run of characters, including none.
    Star,
    /// `?`: exactly one character.
    Any,
    /// `[...]` or `[!...]`.
    Set {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    Literal(char),
}

impl GlobToken {
    fn matches(&self, c: char) -> bool {
        match self {
            GlobToken::Star => unreachable!("a star is handled by the matcher"),
            GlobToken::Any => true,
            GlobToken::Set { negated, ranges } => {
                ranges.iter().any(|&(low, high)| low <= c && c <= high) != *negated
            }
            GlobToken::Literal(literal) => *literal == c,
        }
    }
}

/// Parse a bracket expression; `chars` starts just after its `[`. Returns the
/// token and how many chars it used, or `None` when the `]` is missing.
fn parse_set(chars: &[char]) -> Option<(GlobToken, usize)> {
    let mut i = 0;
    let negated = chars.first() == Some(&'!');
    if negated {
        i += 1;
    }
    let mut ranges = Vec::new();
    let first = i;
    while i < chars.len() {
        let c = chars[i];
        if c == ']' && i > first {
            return Some((GlobToken::Set { negated, ranges }, i + 1));
        }
        if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != ']' {
            ranges.push((c, chars[i + 2]));
            i += 3;
        } else {
            ranges.push((c, c));
            i += 1;
        }
    }
    None
}

fn parse_glob(pattern: &str) -> Vec<GlobToken> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => tokens.push(GlobToken::Star),
            '?' => tokens.push(GlobToken::Any),
            '\\' if i + 1 < chars.len() => {
                i += 1;
                tokens.push(GlobToken::Literal(chars[i]));
            }
            '[' => match parse_set(&chars[i + 1..]) {
                Some((set, used)) => {
                    tokens.push(set);
                    i += used;
                }
                None => tokens.push(GlobToken::Literal('[')),
            },
            c => tokens.push(GlobToken::Literal(c)),
        }
        i += 1;
    }
    tokens
}

/// Shell-style glob over the whole of `text`: `*`, `?`, `[a-z]`, `[!a-z]`,
/// `\x`. Iterative, with one backtrack point (the most recent `*`), so it is
/// linear-ish and cannot blow up on many stars.
fn glob_match(pattern: &str, text: &str) -> bool {
    let tokens = parse_glob(pattern);
    let text: Vec<char> = text.chars().collect();
    let (mut t, mut p) = (0, 0);
    // Where to resume after the last star: (token after it, text position).
    let mut backtrack: Option<(usize, usize)> = None;
    while t < text.len() {
        match tokens.get(p) {
            Some(GlobToken::Star) => {
                p += 1;
                backtrack = Some((p, t));
            }
            Some(token) if token.matches(text[t]) => {
                p += 1;
                t += 1;
            }
            _ => match backtrack {
                Some((after_star, from)) => {
                    p = after_star;
                    t = from + 1;
                    backtrack = Some((after_star, from + 1));
                }
                None => return false,
            },
        }
    }
    tokens[p..].iter().all(|token| *token == GlobToken::Star)
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

/// The files that hold command text or are derived from it.
const PRIVATE_FILES: &[&str] = &[
    "events.db",
    "events.db-wal",
    "events.db-shm",
    CMD_MARKER_FILE,
    "cmd_model.txt",
    "cmd_model_stats.json",
];

/// Make `data_dir` `0700` and each private file in it that exists `0600`.
///
/// Only paths whose mode differs are changed. Upstream never sets a mode, so
/// a user who never enables history keeps whatever their umask gave them.
pub(super) fn tighten_permissions(data_dir: &Path) -> std::io::Result<()> {
    tighten_counting(data_dir).map(|_changed| ())
}

/// [`tighten_permissions`], returning how many modes it had to change.
fn tighten_counting(data_dir: &Path) -> std::io::Result<usize> {
    let mut changed = 0;
    let targets = std::iter::once((data_dir.to_path_buf(), 0o700)).chain(
        PRIVATE_FILES
            .iter()
            .map(|name| (data_dir.join(name), 0o600)),
    );
    for (path, mode) in targets {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(not_private(&path, error)),
        };
        if metadata.permissions().mode() & 0o777 != mode {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .map_err(|error| not_private(&path, error))?;
            changed += 1;
        }
    }
    Ok(changed)
}

fn not_private(path: &Path, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        format!("could not make {} private: {}", path.display(), error),
    )
}

/// Delete the command model files, if any (purge).
pub(super) fn remove_model_files(data_dir: &Path) -> std::io::Result<()> {
    for name in ["cmd_model.txt", "cmd_model_stats.json"] {
        match std::fs::remove_file(data_dir.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tombstones
// ---------------------------------------------------------------------------

/// 64-bit FNV-1a over the UTF-8 bytes of `text`, reinterpreted as `i64` for
/// SQLite. Stored for deleted commands instead of their text.
pub(super) fn fnv1a64(text: &str) -> i64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let hash = text.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    });
    hash as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_anchors_to_the_whole_text() {
        let cases = [
            ("*password*", "mysql -ppassword1", true),
            ("git ?", "git s", true),
            ("git ?", "git st", false),
            ("[a-c]x", "bx", true),
            ("[!a-c]x", "bx", false),
            ("a\\*b", "a*b", true),
            ("a\\*b", "aXb", false),
            ("[x", "[x", true),
        ];
        for (pattern, text, expected) in cases {
            assert_eq!(
                glob_match(pattern, text),
                expected,
                "{pattern:?} against {text:?}"
            );
        }
    }

    #[test]
    fn glob_star_crosses_slashes_and_newlines_and_backtracks() {
        assert!(glob_match("*", ""));
        assert!(glob_match("a*", "a/b\nc"));
        assert!(glob_match("*ab*ab", "xabyabab"));
        assert!(!glob_match("*ab*ab", "xabyaba"));
        assert!(glob_match("[]]", "]"));
        assert!(glob_match("[a-]", "-"));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn is_ignored_splits_on_colons_and_skips_empty_patterns() {
        let text = "export TOKEN=1";
        assert!(!is_ignored(text, None));
        assert!(!is_ignored(text, Some("")));
        assert!(is_ignored(text, Some("ls:export *")));
        assert!(!is_ignored(text, Some(":")));
    }

    #[test]
    fn fnv1a64_matches_the_published_vectors() {
        assert_eq!(fnv1a64(""), 0xcbf29ce484222325_u64 as i64);
        assert_eq!(fnv1a64("a"), 0xaf63dc4c8601ec8c_u64 as i64);
    }

    #[test]
    fn recording_enabled_lets_the_variable_override_the_marker() {
        let dir = std::env::temp_dir().join(format!("psychic-cmd-optin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(marker_path(&dir));

        assert!(!recording_enabled(&dir, None));
        assert!(recording_enabled(&dir, Some("1")));
        std::fs::write(marker_path(&dir), "1\n").unwrap();
        assert!(recording_enabled(&dir, None));
        assert!(recording_enabled(&dir, Some("yes")));
        assert!(!recording_enabled(&dir, Some("0")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tighten_permissions_makes_the_dir_and_files_private_once() {
        let dir = std::env::temp_dir().join(format!("psychic-cmd-perms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let db = dir.join("events.db");
        std::fs::write(&db, b"").unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(tighten_counting(&dir).unwrap(), 2);
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&db), 0o600);
        assert_eq!(
            tighten_counting(&dir).unwrap(),
            0,
            "modes already right: no set_permissions; missing -wal/-shm are not an error"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
