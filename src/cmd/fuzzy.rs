//! Matching one command text against the query, for the picker's filter
//! (`docs/ctrl-r/02-browse.md` section 6.3), and for training's replay and
//! the benchmark, which must match exactly as the picker does.
//!
//! **Which commands match** is Skim's rule (`SkimMatcherV2`, the matcher
//! the model was built on): the whole query, spaces included, is a
//! subsequence of the text, ASCII case ignored unless the query has an
//! ASCII upper-case letter. **The score** the ranking orders by is fzf's
//! (`super::fzf::FzfQuery`, `--scheme=history`). A Skim match is an fzf
//! match (each space-separated term of the query lies inside the whole
//! query's alignment), except for a query with a non-ASCII character whose
//! lower case differs from it (fzf then compares that term's case, Skim
//! does not) or with a TAB (fzf reads it as a space); such a command keeps
//! `NO_FZF_MATCH`, below every match.
//!
//! One pass per command:
//!
//! * a 64-bit summary of the bytes each text holds ([`text_classes`]) must
//!   hold every class of the query's bytes; most texts are rejected
//!   without reading them;
//! * the text must hold the query's bytes in order (ASCII case folded unless
//!   case-sensitive). On an ASCII text that is exactly Skim's match, so
//!   Skim's dynamic program never runs; on any other text Skim decides;
//! * fzf scores the match.
//!
//! Skim's score, which the model reads as the `fuzzy_score` feature, is
//! computed only for the rows the model scores ([`skim_score`]).

use super::fzf::FzfQuery;
use super::index::CommandIndex;
use super::rank::NO_FZF_MATCH;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use std::sync::LazyLock;

/// Class-mask bit kept by `CommandIndex` for a command not deleted. Never
/// set by [`text_classes`]; every [`Pattern`] requires it.
pub(super) const LIVE: u64 = 1 << 63;

/// The class bit of every byte of a multi-byte character: a text with it
/// is not ASCII.
const NON_ASCII: u64 = 1 << 61;

/// The class of a byte, as a bit number below 63. ASCII letters are folded
/// to lower case (the matcher ignores ASCII case unless the query has an
/// upper-case letter, and a mask is only ever a necessary condition); the
/// rarer punctuation and control characters share bits, which only makes
/// the mask reject less.
const fn class_of(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' => byte - b'a',
        b'A'..=b'Z' => byte - b'A',
        b'0'..=b'9' => 26 + (byte - b'0'),
        b' ' => 36,
        b'-' => 37,
        b'.' => 38,
        b'/' => 39,
        b'_' => 40,
        b'=' => 41,
        b':' => 42,
        b'"' => 43,
        b'\'' => 44,
        b'$' => 45,
        b'|' => 46,
        b'&' => 47,
        b'>' => 48,
        b'<' => 49,
        b',' => 50,
        b'@' => 51,
        b'~' => 52,
        b'*' => 53,
        b'#' => 54,
        b'%' => 55,
        b'+' => 56,
        b'!' => 57,
        b'(' | b')' => 58,
        b'[' | b']' => 59,
        b'{' | b'}' => 60,
        0x80..=0xFF => 61, // NON_ASCII
        _ => 62,           // \ ^ ` ; ? control characters, DEL
    }
}

const CLASS_BITS: [u64; 256] = {
    let mut bits = [0u64; 256];
    let mut byte = 0;
    while byte < 256 {
        bits[byte] = 1 << class_of(byte as u8);
        byte += 1;
    }
    bits
};

/// The classes of the bytes `text` holds, one bit each (never [`LIVE`]).
pub(super) fn text_classes(text: &str) -> u64 {
    text.bytes()
        .fold(0, |mask, byte| mask | CLASS_BITS[usize::from(byte)])
}

/// A non-empty query, prepared once per filter pass.
pub(super) struct Pattern {
    query: String,
    /// [`LIVE`] plus the classes of the query's bytes.
    required: u64,
    /// Skim's smart case: exact only when the query has an ASCII
    /// upper-case letter.
    case_sensitive: bool,
    fzf: FzfQuery,
    /// Resolved here, not inside the filter's parallel loop.
    skim: &'static SkimMatcherV2,
}

impl Pattern {
    /// `None` for the empty query, which every command matches with score 0.
    pub fn new(query: &str) -> Option<Self> {
        (!query.is_empty()).then(|| Pattern {
            query: query.to_string(),
            required: LIVE | text_classes(query),
            case_sensitive: query.bytes().any(|b| b.is_ascii_uppercase()),
            fzf: FzfQuery::new(query),
            skim: &SKIM,
        })
    }

    /// Whether `text`, whose class mask (with [`LIVE`]) is `classes`, is
    /// live and holds the query's bytes in order. Skim pairs each query
    /// character with an equal text character in order (ignoring ASCII case
    /// unless case-sensitive), so this is necessary for its match; on an
    /// ASCII text, where a byte is a character, it is also sufficient.
    #[inline]
    pub fn may_match(&self, classes: u64, text: &str) -> bool {
        if classes & self.required != self.required {
            return false;
        }
        let mut wanted = self.query.as_bytes().iter();
        let mut next = wanted.next();
        for &byte in text.as_bytes() {
            let Some(&want) = next else { break };
            let equal = if self.case_sensitive {
                byte == want
            } else {
                byte.eq_ignore_ascii_case(&want)
            };
            if equal {
                next = wanted.next();
            }
        }
        next.is_none()
    }

    /// The filter's test of one command: fzf's score if `text` matches
    /// (Skim's rule), `None` if not.
    #[inline]
    pub fn test(&self, classes: u64, text: &str) -> Option<i32> {
        if !self.may_match(classes, text) {
            return None;
        }
        if classes & NON_ASCII != 0 && self.skim.fuzzy_match(text, &self.query).is_none() {
            return None;
        }
        // Unreachable but for an exotic case fold; below every match then.
        Some(self.fzf.score(text).unwrap_or(NO_FZF_MATCH))
    }
}

/// A query ready to test the commands of an index one at a time: what
/// training's replay and the benchmark match with, so they match exactly as
/// the picker's filter does.
pub(super) struct Query {
    /// `None` for the empty query.
    pattern: Option<Pattern>,
}

impl Query {
    pub fn new(query: &str) -> Self {
        Query {
            pattern: Pattern::new(query),
        }
    }

    /// fzf's score of command `idx` (0 for the empty query), `None` when it
    /// is deleted or does not match.
    #[inline]
    pub fn matches(&self, index: &CommandIndex, idx: u32) -> Option<i32> {
        let classes = index.filter_classes()[idx as usize];
        if classes & LIVE == 0 {
            return None;
        }
        match &self.pattern {
            None => Some(0),
            Some(pattern) => pattern.test(classes, &index.entries[idx as usize].text),
        }
    }
}

/// The one Skim matcher of the process, shared by reference (it is `Sync`
/// and keeps per-thread scratch space).
static SKIM: LazyLock<SkimMatcherV2> = LazyLock::new(SkimMatcherV2::default);

/// The process's Skim matcher, to resolve before a parallel loop.
pub(super) fn skim() -> &'static SkimMatcherV2 {
    &SKIM
}

/// Skim's score of `text` for `query` (the `fuzzy_score` feature): 0 for
/// the empty query and for a text Skim does not match (a logged impression
/// whose command no longer matches).
pub(super) fn skim_score(skim: &SkimMatcherV2, query: &str, text: &str) -> i64 {
    if query.is_empty() {
        return 0;
    }
    skim.fuzzy_match(text, query).unwrap_or(0)
}

/// Create this thread's matcher buffers, fzf's and Skim's, so the first
/// query does not (the worker calls it on every pool thread at start).
pub(super) fn warm_up() {
    super::fzf::warm_up();
    let _ = skim_score(skim(), "wu", "warm up");
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    #[test]
    fn classes_fold_ascii_case_and_leave_out_live() {
        assert_eq!(text_classes("Git"), text_classes("gIT"));
        assert_eq!(text_classes(""), 0);
        assert_eq!(text_classes("é") & LIVE, 0);
        assert_eq!(text_classes("é"), NON_ASCII);
        assert_ne!(text_classes("a"), text_classes("1"));
    }

    #[test]
    fn a_deleted_or_missing_byte_is_rejected_by_the_mask() {
        let p = Pattern::new("gs").expect("not empty");
        let classes = text_classes("git status");
        assert!(p.may_match(classes | LIVE, "git status"));
        assert!(!p.may_match(classes, "git status"), "not live");
        assert!(!p.may_match(text_classes("ls") | LIVE, "ls"));
        // Out of order: the mask passes, the scan does not.
        assert!(!p.may_match(text_classes("sg") | LIVE, "sg"));
        assert!(Pattern::new("").is_none());
        // The whole query in order, spaces included (Skim's rule).
        let p = Pattern::new("st gi").expect("not empty");
        assert_eq!(p.test(classes | LIVE, "git status"), None);
        let p = Pattern::new("git st").expect("not empty");
        let fzf = FzfQuery::new("git st").score("git status");
        assert_eq!(p.test(classes | LIVE, "git status"), fzf);
    }

    /// Characters chosen to collide: ASCII case pairs, shared mask bits,
    /// multi-byte characters sharing lead bytes, characters whose case
    /// folds to ASCII (the Kelvin sign) or has no one-character lower case
    /// (`İ`), escaped spaces, separators.
    const ALPHABET: &[char] = &[
        'a', 'b', 'A', 'B', 'g', 'G', 'k', 'K', '\u{212A}', 'i', 'İ', '1', '-', ' ', ' ', '/',
        '\\', '^', ';', 'é', 'É', 'ß', 'ü', '☃', '\u{0}', '_', '\t', 'ǅ',
    ];

    fn random_text(rng: &mut StdRng, max: usize) -> String {
        let len = rng.gen_range(0..=max);
        (0..len)
            .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())])
            .collect()
    }

    #[test]
    fn the_test_matches_exactly_what_skim_does_and_scores_like_fzf() {
        let skim = SkimMatcherV2::default();
        let mut rng = StdRng::seed_from_u64(7);
        let (mut rejected_early, mut matched) = (0, 0);
        for _ in 0..300_000 {
            let text = random_text(&mut rng, 12);
            let query = random_text(&mut rng, 5);
            let Some(p) = Pattern::new(&query) else {
                continue;
            };
            let classes = text_classes(&text) | LIVE;
            let tested = p.test(classes, &text);
            let by_skim = skim.fuzzy_match(&text, &query);
            assert_eq!(tested.is_some(), by_skim.is_some(), "{text:?} {query:?}");
            if let Some(score) = tested {
                matched += 1;
                // Every Skim match is an fzf match, but where fzf's smart
                // case sees a non-ASCII upper-case letter that Skim's does
                // not: fzf is case-sensitive there, Skim is not.
                let fzf = FzfQuery::new(&query).score(&text);
                let exotic = query
                    .chars()
                    .any(|c| c == '\t' || (!c.is_ascii() && !c.to_lowercase().eq([c])));
                assert!(
                    Some(score) == fzf || (exotic && fzf.is_none() && score == NO_FZF_MATCH),
                    "{text:?} {query:?}"
                );
            } else if !p.may_match(classes, &text) {
                rejected_early += 1;
            }
        }
        // The byte test does real work, and the test sees matches.
        assert!(
            rejected_early > 50_000 && matched > 10_000,
            "{rejected_early} {matched}"
        );
    }

    #[test]
    fn ascii_texts_never_need_skim() {
        // On ASCII texts the byte scan alone is Skim's answer.
        let skim = SkimMatcherV2::default();
        let mut rng = StdRng::seed_from_u64(8);
        let alphabet = b"abgAGB01 /:-._|,;\\";
        for _ in 0..200_000 {
            let pick = |rng: &mut StdRng, max| -> String {
                let len = rng.gen_range(0..=max);
                (0..len)
                    .map(|_| char::from(alphabet[rng.gen_range(0..alphabet.len())]))
                    .collect()
            };
            let text = pick(&mut rng, 20);
            let query = pick(&mut rng, 4);
            let Some(p) = Pattern::new(&query) else {
                continue;
            };
            assert_eq!(
                p.may_match(text_classes(&text) | LIVE, &text),
                skim.fuzzy_match(&text, &query).is_some(),
                "{text:?} {query:?}"
            );
        }
    }

    #[test]
    fn skim_scores_zero_where_it_does_not_match() {
        assert_eq!(skim_score(skim(), "", "git status"), 0);
        assert!(skim_score(skim(), "gs", "git status") > 0);
        assert_eq!(skim_score(skim(), "zz", "git status"), 0);
    }
}
