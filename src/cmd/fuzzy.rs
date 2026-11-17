//! Matching one command text against the query, for the picker's filter
//! (`docs/ctrl-r/02-browse.md` section 6.3).
//!
//! Two parts, kept apart so the scorer can change without touching the rest:
//!
//! * a prefilter that only rejects: a text can match only if it holds every
//!   byte of the query, in order ([`Pattern::may_match`]). It is true of any
//!   subsequence matcher, so it never changes what matches. Most texts are
//!   rejected by a 64-bit summary of their bytes ([`text_classes`]) without
//!   reading the text at all;
//! * the scorer, [`Pattern::score`]: today exactly
//!   `SkimMatcherV2::default().fuzzy_match`, the one definition of a match
//!   and its score that training and the benchmark use too.

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

/// Class-mask bit kept by `CommandIndex` for a command not deleted. Never
/// set by [`text_classes`]; every [`Pattern`] requires it.
pub(super) const LIVE: u64 = 1 << 63;

/// The class of a byte, as a bit number below 63. ASCII letters are folded
/// to lower case (the matcher ignores ASCII case unless the query has an
/// upper-case letter, and a mask is only ever a necessary condition); the
/// rarer punctuation, control characters and every byte of a multi-byte
/// character share bits, which only makes the mask reject less.
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
        0x80..=0xFF => 61,
        _ => 62, // \ ^ ` ; ? control characters, DEL
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
        .fold(0, |mask, byte| mask | CLASS_BITS[byte as usize])
}

/// A non-empty query, prepared once per filter pass.
pub(super) struct Pattern<'q> {
    query: &'q str,
    /// [`LIVE`] plus the classes of the query's bytes.
    required: u64,
    /// The matcher's smart case: exact only when the query has an ASCII
    /// upper-case letter.
    case_sensitive: bool,
}

impl<'q> Pattern<'q> {
    /// `None` for the empty query, which every command matches with score 0.
    pub fn new(query: &'q str) -> Option<Self> {
        (!query.is_empty()).then(|| Pattern {
            query,
            required: LIVE | text_classes(query),
            case_sensitive: query.bytes().any(|b| b.is_ascii_uppercase()),
        })
    }

    /// Whether `text`, whose class mask (with [`LIVE`]) is `classes`, can
    /// match: it is live and holds the query's bytes in order. False only
    /// when the scorer would return `None`.
    ///
    /// A match pairs each query character with an equal text character
    /// (ignoring ASCII case unless case-sensitive), in order; the UTF-8
    /// bytes of those characters are then also in order, and folding ASCII
    /// case never changes a byte of a multi-byte character.
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

    /// The match score of `text`, `None` when it does not match: the one
    /// scorer of the filter.
    #[inline]
    pub fn score(&self, matcher: &SkimMatcherV2, text: &str) -> Option<i64> {
        matcher.fuzzy_match(text, self.query)
    }
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
    }

    /// Characters chosen to collide: ASCII case pairs, shared mask bits,
    /// multi-byte characters sharing lead bytes, separators.
    const ALPHABET: &[char] = &[
        'a', 'b', 'A', 'B', 'g', 'G', '1', '-', ' ', '/', '\\', '^', ';', 'é', 'É', 'ß', 'ü', '☃',
        '\u{0}', '_',
    ];

    fn random_text(rng: &mut StdRng, max: usize) -> String {
        let len = rng.gen_range(0..=max);
        (0..len)
            .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())])
            .collect()
    }

    #[test]
    fn the_prefilter_never_rejects_a_match() {
        let matcher = SkimMatcherV2::default();
        let mut rng = StdRng::seed_from_u64(7);
        let (mut rejected, mut matched) = (0, 0);
        for _ in 0..200_000 {
            let text = random_text(&mut rng, 12);
            let query = random_text(&mut rng, 4);
            let Some(p) = Pattern::new(&query) else {
                continue;
            };
            let may = p.may_match(text_classes(&text) | LIVE, &text);
            let score = p.score(&matcher, &text);
            assert_eq!(score, matcher.fuzzy_match(&text, &query));
            if score.is_some() {
                matched += 1;
                assert!(may, "{text:?} matches {query:?} but was rejected");
            } else if !may {
                rejected += 1;
            }
        }
        // The prefilter does real work, and the test sees matches.
        assert!(
            rejected > 50_000 && matched > 10_000,
            "{rejected} {matched}"
        );
    }
}
