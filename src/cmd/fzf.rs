//! fzf's match score under `--scheme=history`, reimplemented
//! (`docs/ctrl-r/02-browse.md` section 7).
//!
//! The cold-start order of history search is what fzf's own Ctrl-R widget
//! shows: the best match score first, ties newest first. This module is the
//! score. It follows fzf v0.74.5 (commit c0a193e, MIT licence, (c) Junegunn
//! Choi): `FuzzyMatchV2` and its `FuzzyMatchV1` fallback with
//! `calculateScore` from `src/algo/algo.go`, the bonus table of
//! `Init("history")`, and the extended-search split of `src/pattern.go`
//! (space-separated terms, each smart-case, scores summed). It is a
//! reimplementation from reading that source, not a copy.
//!
//! Not adopted, on purpose (section 7 says why): latin-script normalisation
//! (fzf folds `é` to `e`), and the extended-search operators (`'`, `^`,
//! `$`, `!`, `|`): every term here is a fuzzy term, matched literally.

use std::cell::RefCell;

const SCORE_MATCH: i16 = 16;
const SCORE_GAP_START: i16 = -3;
const SCORE_GAP_EXTENSION: i16 = -1;
/// `scoreMatch / 2`; under the history scheme the start of the text, a
/// word after whitespace and a word after a delimiter all get this much.
const BONUS_BOUNDARY: i16 = SCORE_MATCH / 2;
const BONUS_NON_WORD: i16 = SCORE_MATCH / 2;
const BONUS_CAMEL_123: i16 = BONUS_BOUNDARY + SCORE_GAP_EXTENSION;
const BONUS_CONSECUTIVE: i16 = -(SCORE_GAP_START + SCORE_GAP_EXTENSION);
const BONUS_FIRST_CHAR_MULTIPLIER: i16 = 2;
/// fzf's `slab16Size`: above `text chars * pattern chars` of this, V2's
/// score matrix does not fit and fzf uses the greedy V1.
const V2_MAX_CELLS: usize = 100 * 1024;
/// fzf refuses V2 for longer patterns (16-bit overflow).
const V2_MAX_PATTERN: usize = 1000;

/// The two bonuses fzf's `--scheme` changes (`Init` in `algo.go`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scheme {
    /// A word after whitespace, and the start of the text.
    boundary_white: i16,
    /// A word after one of the delimiters `/,:;|`.
    boundary_delimiter: i16,
}

/// `--scheme=history`, what fzf's Ctrl-R widgets pass: the start of a
/// command is worth no more than the start of any other word.
const HISTORY: Scheme = Scheme {
    boundary_white: BONUS_BOUNDARY,
    boundary_delimiter: BONUS_BOUNDARY,
};

/// fzf's `charClass`, in its order (`bonus_for` compares them).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    White,
    NonWord,
    Delimiter,
    Lower,
    Upper,
    Letter,
    Number,
}

/// The class before the first character (`initialCharClass`).
const INITIAL_CLASS: Class = Class::White;

fn class_of(c: char) -> Class {
    if c.is_ascii() {
        ASCII_CLASSES[c as usize]
    } else if c.is_lowercase() {
        Class::Lower
    } else if c.is_uppercase() {
        Class::Upper
    } else if c.is_numeric() {
        Class::Number
    } else if c.is_alphabetic() {
        Class::Letter
    } else if c.is_whitespace() {
        Class::White
    } else {
        Class::NonWord
    }
}

const fn ascii_class(byte: u8) -> Class {
    match byte {
        b'a'..=b'z' => Class::Lower,
        b'A'..=b'Z' => Class::Upper,
        b'0'..=b'9' => Class::Number,
        b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r' => Class::White,
        b'/' | b',' | b':' | b';' | b'|' => Class::Delimiter, // DELIMITERS
        _ => Class::NonWord,
    }
}

/// The class of each ASCII character (fzf's `asciiCharClasses`).
const ASCII_CLASSES: [Class; 128] = {
    let mut classes = [Class::NonWord; 128];
    let mut byte = 0;
    while byte < 128 {
        classes[byte] = ascii_class(byte as u8);
        byte += 1;
    }
    classes
};

/// `bonusFor`: the bonus of a character of `class` after one of `prev`.
fn bonus_for(scheme: Scheme, prev: Class, class: Class) -> i16 {
    if class >= Class::NonWord {
        match prev {
            Class::White => return scheme.boundary_white,
            Class::Delimiter => return scheme.boundary_delimiter,
            Class::NonWord => return BONUS_BOUNDARY,
            _ => {}
        }
    }
    if prev == Class::Lower && class == Class::Upper
        || prev != Class::Number && class == Class::Number
    {
        return BONUS_CAMEL_123;
    }
    match class {
        Class::NonWord | Class::Delimiter => BONUS_NON_WORD,
        Class::White => scheme.boundary_white,
        _ => 0,
    }
}

/// The largest bonus `bonus_for` can give under `scheme`.
fn max_bonus(scheme: Scheme) -> i16 {
    scheme
        .boundary_white
        .max(scheme.boundary_delimiter)
        .max(BONUS_BOUNDARY)
        .max(BONUS_NON_WORD)
        .max(BONUS_CAMEL_123)
}

/// `c` as a case-insensitive match compares it: lower case of an upper-case
/// letter (a one-character mapping only, like Go's `unicode.To`).
fn fold(c: char, class: Class) -> char {
    if class != Class::Upper {
        return c;
    }
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// One unit of a text as the matcher reads it: a byte of an ASCII text, or
/// a `char`. The byte form gives the same classes and folding as the `char`
/// form of the same ASCII character, without decoding the text.
trait Unit: Copy + PartialEq {
    fn class(self) -> Class;
    /// The unit as a case-insensitive match compares it; `class` is
    /// `self.class()`.
    fn fold(self, class: Class) -> Self;
}

impl Unit for u8 {
    #[inline]
    fn class(self) -> Class {
        ASCII_CLASSES[usize::from(self & 0x7F)]
    }

    #[inline]
    fn fold(self, class: Class) -> Self {
        if class == Class::Upper {
            self.to_ascii_lowercase()
        } else {
            self
        }
    }
}

impl Unit for char {
    #[inline]
    fn class(self) -> Class {
        class_of(self)
    }

    #[inline]
    fn fold(self, class: Class) -> Self {
        fold(self, class)
    }
}

/// One space-separated term of a query.
#[derive(Clone, Debug)]
struct Term {
    chars: Vec<char>,
    /// `chars` as bytes when every one is ASCII: the form an ASCII text is
    /// matched with. `None` otherwise (such a term matches no ASCII text).
    bytes: Option<Vec<u8>>,
    case_sensitive: bool,
    scheme: Scheme,
}

impl Term {
    fn new(chars: Vec<char>, case_sensitive: bool, scheme: Scheme) -> Self {
        let bytes = chars
            .iter()
            .all(char::is_ascii)
            .then(|| chars.iter().map(|&c| c as u8).collect());
        Term {
            chars,
            bytes,
            case_sensitive,
            scheme,
        }
    }

    /// `text` unit as this term compares it with its own units.
    #[inline]
    fn view<U: Unit>(&self, raw: U, class: Class) -> U {
        if self.case_sensitive {
            raw
        } else {
            raw.fold(class)
        }
    }
}

/// A query as fzf's extended search splits it. Built once per keystroke.
#[derive(Clone, Debug, Default)]
pub(super) struct FzfQuery {
    terms: Vec<Term>,
}

impl FzfQuery {
    /// `query` split like `BuildPattern` + `parseTerms`: leading spaces and
    /// unescaped trailing spaces dropped, split on runs of spaces, `\ ` a
    /// literal space; a term is case-sensitive only if it has an upper-case
    /// letter (smart case).
    pub(super) fn new(query: &str) -> Self {
        Self::with_scheme(query, HISTORY)
    }

    fn with_scheme(query: &str, scheme: Scheme) -> Self {
        let mut text = query.trim_start_matches(' ');
        while text.ends_with(' ') && !text.ends_with("\\ ") {
            text = &text[..text.len() - 1];
        }
        let terms = text
            .replace("\\ ", "\t")
            .split(' ')
            .filter(|token| !token.is_empty())
            .map(|token| {
                let token = token.replace('\t', " ");
                let lower = token.to_lowercase();
                let case_sensitive = token != lower;
                let chars = if case_sensitive { token } else { lower };
                Term::new(chars.chars().collect(), case_sensitive, scheme)
            })
            .collect();
        FzfQuery { terms }
    }

    /// The sum of the terms' scores, or `None` when a term does not match.
    /// An ASCII text is read as bytes; any other is decoded once.
    pub(super) fn score(&self, text: &str) -> Option<i32> {
        if self.terms.is_empty() {
            return Some(0);
        }
        let total = if text.is_ascii() {
            BYTES.with(|dp| {
                let dp = &mut *dp.borrow_mut();
                let mut total = 0;
                for term in &self.terms {
                    // A non-ASCII character equals no ASCII one, folded or not.
                    let pattern = term.bytes.as_deref()?;
                    total += i32::from(term_score(term, pattern, text.as_bytes(), dp)?);
                }
                Some(total)
            })
        } else {
            CHARS.with(|dp| {
                let (text_chars, dp) = &mut *dp.borrow_mut();
                text_chars.clear();
                text_chars.extend(text.chars());
                let mut total = 0;
                for term in &self.terms {
                    total += i32::from(term_score(term, &term.chars, text_chars, dp)?);
                }
                Some(total)
            })
        }?;
        // fzf ranks by `util.AsUint16(score)`: a negative total (V1 on a
        // long text with long gaps) ties at 0 with the input order.
        Some(total.clamp(0, i32::from(u16::MAX)))
    }
}

/// V2's buffers, reused across calls on one thread: no allocation per
/// candidate.
struct Dp<U> {
    folded: Vec<U>,
    bonus: Vec<i16>,
    h0: Vec<i16>,
    c0: Vec<i16>,
    first: Vec<usize>,
    h: Vec<i16>,
    c: Vec<i16>,
}

impl<U> Default for Dp<U> {
    fn default() -> Self {
        Dp {
            folded: Vec::new(),
            bonus: Vec::new(),
            h0: Vec::new(),
            c0: Vec::new(),
            first: Vec::new(),
            h: Vec::new(),
            c: Vec::new(),
        }
    }
}

thread_local! {
    static BYTES: RefCell<Dp<u8>> = RefCell::new(Dp::default());
    /// The decoded text, and the buffers.
    static CHARS: RefCell<(Vec<char>, Dp<char>)> = RefCell::new((Vec::new(), Dp::default()));
}

/// Make sure this thread's buffers exist, so the first query does not
/// create them (the worker calls it on every pool thread at start).
pub(super) fn warm_up() {
    let query = FzfQuery::new("wu");
    let _ = query.score("warm up");
    let _ = query.score("wärm up");
}

fn term_score<U: Unit>(term: &Term, pattern: &[U], text: &[U], dp: &mut Dp<U>) -> Option<i16> {
    let m = pattern.len();
    let n = text.len();
    if m == 0 {
        return Some(0);
    }
    if m > n {
        return None;
    }
    if n * m > V2_MAX_CELLS || m > V2_MAX_PATTERN {
        return fuzzy_v1(term, pattern, text);
    }
    if m == 1 {
        return single_v2(term, pattern[0], text);
    }
    fuzzy_v2(term, pattern, text, dp)
}

/// V2 for a one-character pattern, which fzf answers after its phase 2:
/// the best of `SCORE_MATCH + 2 * bonus` over the occurrences. Stops at the
/// largest bonus possible.
fn single_v2<U: Unit>(term: &Term, p: U, text: &[U]) -> Option<i16> {
    let ceiling = SCORE_MATCH + max_bonus(term.scheme) * BONUS_FIRST_CHAR_MULTIPLIER;
    let mut best = None;
    let mut prev_class = INITIAL_CLASS;
    for &raw in text {
        let class = raw.class();
        if term.view(raw, class) == p {
            let score = SCORE_MATCH
                + bonus_for(term.scheme, prev_class, class) * BONUS_FIRST_CHAR_MULTIPLIER;
            if best.is_none_or(|best| score > best) {
                best = Some(score);
                if score == ceiling {
                    break;
                }
            }
        }
        prev_class = class;
    }
    best
}

/// Phase 1 of V2 (`asciiFuzzyIndex`): `(from, to)` such that every
/// alignment lies in `text[from..to]` - from one before the first
/// occurrence of the first pattern character to the last occurrence of the
/// last one - or `None` when the pattern is not a subsequence.
fn match_window<U: Unit>(term: &Term, pattern: &[U], text: &[U]) -> Option<(usize, usize)> {
    let equal = |c: U, p: U| term.view(c, c.class()) == p;
    let (mut first, mut at, mut last) = (0, 0, 0);
    for (pidx, &p) in pattern.iter().enumerate() {
        at += text[at..].iter().position(|&c| equal(c, p))?;
        if pidx == 0 && at > 0 {
            first = at - 1;
        }
        last = at;
        at += 1;
    }
    let p = *pattern.last().expect("a non-empty term");
    let end = text[last + 1..]
        .iter()
        .rposition(|&c| equal(c, p))
        .map_or(last + 1, |tail| last + 1 + tail + 1);
    Some((first, end))
}

/// fzf's `FuzzyMatchV2`, score only: the best-scoring alignment by
/// dynamic programming (a Smith-Waterman variant without omissions).
fn fuzzy_v2<U: Unit>(term: &Term, pattern: &[U], text: &[U], s: &mut Dp<U>) -> Option<i16> {
    let (from, to) = match_window(term, pattern, text)?;
    let m = pattern.len();
    let width_all = to - from;

    // Phase 2: classes, bonuses, the first row, first occurrences.
    s.folded.clear();
    s.bonus.clear();
    s.h0.clear();
    s.c0.clear();
    s.first.clear();
    let (mut max_score, mut pidx, mut last_idx) = (0i16, 0usize, 0usize);
    let (pchar0, mut pchar) = (pattern[0], pattern[0]);
    let (mut prev_h0, mut prev_class, mut in_gap) = (0i16, INITIAL_CLASS, false);
    for &raw in &text[from..from + width_all] {
        let off = s.folded.len();
        let class = raw.class();
        let c = term.view(raw, class);
        s.folded.push(c);
        let bonus = bonus_for(term.scheme, prev_class, class);
        s.bonus.push(bonus);
        prev_class = class;
        if c == pchar {
            if pidx < m {
                s.first.push(off);
                pidx += 1;
                pchar = pattern[pidx.min(m - 1)];
            }
            last_idx = off;
        }
        let h0 = if c == pchar0 {
            let score = SCORE_MATCH + bonus * BONUS_FIRST_CHAR_MULTIPLIER;
            s.c0.push(1);
            if m == 1 && score > max_score {
                max_score = score;
            }
            in_gap = false;
            score
        } else {
            let gap = if in_gap {
                SCORE_GAP_EXTENSION
            } else {
                SCORE_GAP_START
            };
            s.c0.push(0);
            in_gap = true;
            (prev_h0 + gap).max(0)
        };
        s.h0.push(h0);
        prev_h0 = h0;
    }
    if pidx != m {
        return None;
    }
    if m == 1 {
        return Some(max_score);
    }

    // Phase 3: the score matrix, one row per pattern character.
    let f0 = s.first[0];
    let width = last_idx - f0 + 1;
    s.h.clear();
    s.h.resize(width * m, 0);
    s.c.clear();
    s.c.resize(width * m, 0);
    s.h[..width].copy_from_slice(&s.h0[f0..=last_idx]);
    s.c[..width].copy_from_slice(&s.c0[f0..=last_idx]);
    for (pidx, &pchar) in pattern.iter().enumerate().skip(1) {
        let f = s.first[pidx];
        let row = pidx * width;
        let mut in_gap = false;
        // Column `col` of the text is cell `col - f0` of a row.
        s.h[row + f - f0 - 1] = 0;
        for col in f..=last_idx {
            let cell = row + col - f0;
            let diag = cell - width - 1;
            let s2 = s.h[cell - 1]
                + if in_gap {
                    SCORE_GAP_EXTENSION
                } else {
                    SCORE_GAP_START
                };
            let (mut s1, mut consecutive) = (0i16, 0i16);
            if s.folded[col] == pchar {
                s1 = s.h[diag] + SCORE_MATCH;
                let mut b = s.bonus[col];
                consecutive = s.c[diag] + 1;
                if consecutive > 1 {
                    let fb = s.bonus[col + 1 - consecutive as usize];
                    // A boundary inside a chunk starts a new chunk.
                    if b >= BONUS_BOUNDARY && b > fb {
                        consecutive = 1;
                    } else {
                        b = b.max(BONUS_CONSECUTIVE).max(fb);
                    }
                }
                if s1 + b < s2 {
                    s1 += s.bonus[col];
                    consecutive = 0;
                } else {
                    s1 += b;
                }
            }
            s.c[cell] = consecutive;
            in_gap = s1 < s2;
            let score = s1.max(s2).max(0);
            if pidx == m - 1 && score > max_score {
                max_score = score;
            }
            s.h[cell] = score;
        }
    }
    Some(max_score)
}

/// fzf's `FuzzyMatchV1` (the long-text fallback), score only: the first
/// occurrence scanning forward, shortened scanning back, scored by
/// `calculateScore`.
fn fuzzy_v1<U: Unit>(term: &Term, pattern: &[U], text: &[U]) -> Option<i16> {
    match_window(term, pattern, text)?;
    let folded = |c: U| term.view(c, c.class());
    let mut pidx = 0;
    let mut start = None;
    let mut end = None;
    for (at, &c) in text.iter().enumerate() {
        if folded(c) == pattern[pidx] {
            start.get_or_insert(at);
            pidx += 1;
            if pidx == pattern.len() {
                end = Some(at + 1);
                break;
            }
        }
    }
    let (mut start, end) = (start?, end?);
    let mut pidx = pattern.len();
    for at in (start..end).rev() {
        if folded(text[at]) == pattern[pidx - 1] {
            pidx -= 1;
            if pidx == 0 {
                start = at;
                break;
            }
        }
    }
    Some(calculate_score(term, pattern, text, start, end))
}

/// fzf's `calculateScore`: the score of the alignment that matches greedily
/// within `text[start..end]`.
fn calculate_score<U: Unit>(
    term: &Term,
    pattern: &[U],
    text: &[U],
    start: usize,
    end: usize,
) -> i16 {
    let (mut pidx, mut score, mut in_gap, mut consecutive, mut first_bonus) =
        (0usize, 0i32, false, 0, 0i16);
    let mut prev_class = if start > 0 {
        text[start - 1].class()
    } else {
        INITIAL_CLASS
    };
    for &raw in &text[start..end] {
        let class = raw.class();
        let c = term.view(raw, class);
        if pidx < pattern.len() && c == pattern[pidx] {
            score += i32::from(SCORE_MATCH);
            let mut bonus = bonus_for(term.scheme, prev_class, class);
            if consecutive == 0 {
                first_bonus = bonus;
            } else {
                if bonus >= BONUS_BOUNDARY && bonus > first_bonus {
                    first_bonus = bonus;
                }
                bonus = bonus.max(first_bonus).max(BONUS_CONSECUTIVE);
            }
            score += i32::from(if pidx == 0 {
                bonus * BONUS_FIRST_CHAR_MULTIPLIER
            } else {
                bonus
            });
            in_gap = false;
            consecutive += 1;
            pidx += 1;
        } else {
            score += i32::from(if in_gap {
                SCORE_GAP_EXTENSION
            } else {
                SCORE_GAP_START
            });
            in_gap = true;
            consecutive = 0;
            first_bonus = 0;
        }
        prev_class = class;
    }
    score.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests;
