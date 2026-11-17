//! fzf's own score vectors (`src/algo/algo_test.go` of v0.74.5, default
//! scheme) and orders checked against the fzf 0.74.5 binary with
//! `--scheme=history` (`docs/ctrl-r/02-browse.md` section 14).

use super::*;

/// fzf's `--scheme=default`, for its own test vectors.
const DEFAULT: Scheme = Scheme {
    boundary_white: BONUS_BOUNDARY + 2,
    boundary_delimiter: BONUS_BOUNDARY + 1,
};

fn term(pattern: &str, case_sensitive: bool, scheme: Scheme) -> Term {
    let chars = if case_sensitive {
        pattern.to_string()
    } else {
        pattern.to_lowercase()
    };
    Term::new(chars.chars().collect(), case_sensitive, scheme)
}

/// (V1, V2) scores of `pattern` in `text`.
fn both(
    text: &str,
    pattern: &str,
    case_sensitive: bool,
    scheme: Scheme,
) -> (Option<i16>, Option<i16>) {
    let term = term(pattern, case_sensitive, scheme);
    let chars: Vec<char> = text.chars().collect();
    let v1 = fuzzy_v1(&term, &term.chars, &chars);
    let v2 = fuzzy_v2(&term, &term.chars, &chars, &mut Dp::default());
    (v1, v2)
}

#[test]
fn fzf_test_fuzzy_match_vectors() {
    const M: i16 = SCORE_MATCH;
    const W: i16 = BONUS_BOUNDARY + 2;
    const D: i16 = BONUS_BOUNDARY + 1;
    const B: i16 = BONUS_BOUNDARY;
    const N: i16 = BONUS_NON_WORD;
    const C: i16 = BONUS_CAMEL_123;
    const K: i16 = BONUS_CONSECUTIVE;
    const GS: i16 = SCORE_GAP_START;
    const GE: i16 = SCORE_GAP_EXTENSION;
    const X: i16 = BONUS_FIRST_CHAR_MULTIPLIER;
    let cases: &[(&str, &str, bool, i16)] = &[
        ("fooBarbaz1", "oBZ", false, M * 3 + C + GS + GE * 3),
        (
            "foo bar baz",
            "fbb",
            false,
            M * 3 + W * X + W * 2 + 2 * GS + 4 * GE,
        ),
        ("/AutomatorDocument.icns", "rdoc", false, M * 4 + C + K * 2),
        ("/man1/zshcompctl.1", "zshc", false, M * 4 + D * X + D * 3),
        (
            "/.oh-my-zsh/cache",
            "zshc",
            false,
            M * 4 + B * X + B * 2 + GS + D,
        ),
        (".vimrc", ".vimrc", false, M * 6 + W * (X + 5)),
        ("/.vimrc", ".vimrc", false, M * 6 + D * (X + 5)),
        ("a.vimrc", ".vimrc", false, M * 6 + B * (X + 5)),
        ("ab0123 456", "12356", false, M * 5 + K * 3 + GS + GE),
        (
            "abc123 456",
            "12356",
            false,
            M * 5 + C * X + C * 2 + K + GS + GE,
        ),
        (
            "foo/bar/baz",
            "fbb",
            false,
            M * 3 + W * X + D * 2 + 2 * GS + 4 * GE,
        ),
        (
            "fooBarBaz",
            "fbb",
            false,
            M * 3 + W * X + C * 2 + 2 * GS + 2 * GE,
        ),
        (
            "foo barbaz",
            "fbb",
            false,
            M * 3 + W * X + W + GS * 2 + GE * 3,
        ),
        ("fooBar Baz", "foob", false, M * 4 + W * X + W * 3),
        (
            "xFoo-Bar Baz",
            "foo-b",
            false,
            M * 5 + C * X + C * 2 + N + B,
        ),
        ("fooBarbaz", "oBz", true, M * 3 + C + GS + GE * 3),
        (
            "Foo/Bar/Baz",
            "FBB",
            true,
            M * 3 + W * X + D * 2 + GS * 2 + GE * 4,
        ),
        (
            "FooBarBaz",
            "FBB",
            true,
            M * 3 + W * X + C * 2 + GS * 2 + GE * 2,
        ),
        ("FooBar Baz", "FooB", true, M * 4 + W * X + W * 2 + W),
        ("foo-bar", "o-ba", true, M * 4 + B * 3),
    ];
    for &(text, pattern, case_sensitive, score) in cases {
        let (v1, v2) = both(text, pattern, case_sensitive, DEFAULT);
        assert_eq!(v1, Some(score), "v1 {text} / {pattern}");
        assert_eq!(v2, Some(score), "v2 {text} / {pattern}");
    }
    for (text, pattern) in [
        ("fooBarbaz", "oBZ"),
        ("Foo Bar Baz", "fbb"),
        ("fooBarbaz", "fooBarbazz"),
    ] {
        assert_eq!(
            both(text, pattern, true, DEFAULT),
            (None, None),
            "{text} / {pattern}"
        );
    }
}

#[test]
fn the_history_scheme_scores_every_word_start_alike() {
    // Under --scheme=history a word after `/` is worth as much as the start
    // of the command or a word after a space; the default scheme ranks
    // them 10 > 9.
    let history = |text: &str| FzfQuery::new("gi").score(text);
    assert_eq!(history("git status"), Some(16 * 2 + 8 * 2 + 8));
    assert_eq!(history("git status"), history("tig git"));
    assert_eq!(history("git status"), history("tig/git"));
    let default = |text: &str| FzfQuery::with_scheme("gi", DEFAULT).score(text);
    assert_eq!(default("git status"), default("tig git"));
    assert!(default("git status") > default("tig/git"));
}

#[test]
fn extended_search_splits_terms_and_sums_them() {
    let q = FzfQuery::new("  git  st ");
    assert_eq!(q.terms.len(), 2);
    let sum = FzfQuery::new("git").score("git status").unwrap()
        + FzfQuery::new("st").score("git status").unwrap();
    assert_eq!(q.score("git status"), Some(sum));
    // Order does not matter; every term must match.
    assert_eq!(FzfQuery::new("st git").score("git status"), Some(sum));
    assert_eq!(q.score("git log"), None);
    // `\ ` is a literal space; a trailing escaped space is kept.
    assert_eq!(FzfQuery::new("a\\ b").terms.len(), 1);
    assert_eq!(FzfQuery::new("ab\\ ").terms[0].chars, vec!['a', 'b', ' ']);
    assert!(FzfQuery::new("   ").terms.is_empty());
    assert_eq!(FzfQuery::new("").score("anything"), Some(0));
}

#[test]
fn smart_case_per_term() {
    assert!(FzfQuery::new("Git").score("git status").is_none());
    assert!(FzfQuery::new("Git").score("Git status").is_some());
    assert!(FzfQuery::new("git").score("GIT STATUS").is_some());
    assert!(FzfQuery::new("Git st").score("Git STATUS").is_some());
}

#[test]
fn long_texts_fall_back_to_v1_like_fzf() {
    // 100 * 1024 cells: a 2-character pattern switches at 51,201 characters.
    let text = format!("{}gx{}", "a".repeat(60_000), "b g x");
    let q = FzfQuery::new("gx");
    let chars: Vec<char> = text.chars().collect();
    assert_eq!(
        q.score(&text).map(|s| s as i16),
        fuzzy_v1(&q.terms[0], &q.terms[0].chars, &chars)
    );
}

#[test]
fn negative_long_text_scores_tie_at_zero_like_fzf() {
    // V1 on 60,000-character texts: the gaps make both totals negative;
    // fzf sorts on the score as a u16, so both are 0 and the input order
    // stays (the fzf 0.74.5 binary prints the 300-gap line first).
    let far = format!("a{}b{}", "x".repeat(300), "y".repeat(60_000));
    let near = format!("a{}b{}", "x".repeat(100), "z".repeat(60_000));
    let q = FzfQuery::new("ab");
    let chars: Vec<char> = near.chars().collect();
    assert!(
        fuzzy_v1(&q.terms[0], &q.terms[0].chars, &chars).unwrap() < 0,
        "negative before the clamp"
    );
    assert_eq!(q.score(&far), Some(0));
    assert_eq!(q.score(&near), Some(0));
}

/// Orders produced by `fzf --filter=QUERY --scheme=history` (fzf 0.74.5,
/// `FZF_DEFAULT_OPTS` empty) on the list below, given newest first: index
/// into `LIST`, best first. A tie keeps the input order (newest first).
const LIST: &[&str] = &[
    "git status",
    "cargo test --release",
    "tig",
    "git commit -m 'fix: gitignore'",
    "docker compose up -d",
    "ls -la src/cmd",
    "cd ~/code/psychic",
    "kubectl get pods -A",
    "vim src/cmd/rank.rs",
    "GIT_PAGER=cat git log --oneline",
    "cat Cargo.toml | grep -i version",
    "make test",
];

fn order(query: &str) -> Vec<usize> {
    let q = FzfQuery::new(query);
    let mut hits: Vec<(i32, usize)> = LIST
        .iter()
        .enumerate()
        .filter_map(|(i, text)| q.score(text).map(|s| (s, i)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}

#[test]
fn orders_match_the_fzf_binary() {
    for (query, expected) in EXPECTED {
        assert_eq!(order(query), *expected, "query {query:?}");
    }
}

include!("expected.rs");

/// Bulk check against the binary (run by hand, section 14 of 02): writes,
/// per query of `FZF_CHECK_QUERIES` (one per line), the 1-based line numbers
/// of `FZF_CHECK_LIST` in this module's order, to `FZF_CHECK_OUT`.
#[test]
#[ignore = "needs a list, queries and the fzf binary's output to compare with"]
fn bulk_order_for_comparison_with_the_binary() {
    let var = |name: &str| std::env::var(name).expect(name);
    let list = std::fs::read_to_string(var("FZF_CHECK_LIST")).unwrap();
    let lines: Vec<&str> = list.lines().collect();
    let mut out = String::new();
    for query in std::fs::read_to_string(var("FZF_CHECK_QUERIES"))
        .unwrap()
        .lines()
    {
        let q = FzfQuery::new(query);
        let mut hits: Vec<(i32, usize)> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, text)| q.score(text).map(|s| (s, i)))
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let ids: Vec<String> = hits.iter().map(|(_, i)| (i + 1).to_string()).collect();
        out.push_str(&ids.join(" "));
        out.push('\n');
    }
    std::fs::write(var("FZF_CHECK_OUT"), out).unwrap();
}

/// Random texts and patterns over characters that exercise every class:
/// both cases, digits, delimiters, other punctuation, whitespace.
fn random_ascii(rng: &mut rand::rngs::StdRng, alphabet: &[u8], max: usize) -> String {
    use rand::Rng;
    let len = rng.gen_range(0..=max);
    (0..len)
        .map(|_| char::from(alphabet[rng.gen_range(0..alphabet.len())]))
        .collect()
}

#[test]
fn ascii_texts_read_as_bytes_score_as_decoded_ones() {
    use rand::SeedableRng;
    let mut rng = rand::rngs::StdRng::seed_from_u64(5);
    let alphabet = b"abgAGB01 /:-._|,;\t";
    let mut matched = 0;
    for _ in 0..100_000 {
        let text = random_ascii(&mut rng, alphabet, 24);
        let pattern = random_ascii(&mut rng, b"abgAG0 /-", 4);
        for scheme in [HISTORY, DEFAULT] {
            for case_sensitive in [false, true] {
                let term = term(&pattern, case_sensitive, scheme);
                let bytes = term.bytes.clone().expect("an ASCII pattern");
                let chars: Vec<char> = text.chars().collect();
                let by_char = term_score(&term, &term.chars, &chars, &mut Dp::default());
                let by_byte = term_score(&term, &bytes, text.as_bytes(), &mut Dp::default());
                assert_eq!(by_char, by_byte, "{text:?} {pattern:?}");
                matched += usize::from(by_char.is_some());
            }
        }
        let query = FzfQuery::new(&pattern);
        // The query's own path (bytes for this ASCII text) against the
        // decoded path, term by term.
        let decoded: Option<i32> = query.terms.iter().try_fold(0i32, |total, term| {
            let chars: Vec<char> = text.chars().collect();
            Some(total + i32::from(term_score(term, &term.chars, &chars, &mut Dp::default())?))
        });
        assert_eq!(
            query.score(&text),
            decoded.map(|t| t.clamp(0, i32::from(u16::MAX)))
        );
    }
    assert!(matched > 50_000, "{matched}");
}

#[test]
fn the_one_character_path_scores_as_the_full_v2() {
    use rand::SeedableRng;
    let mut rng = rand::rngs::StdRng::seed_from_u64(6);
    for _ in 0..50_000 {
        let text = random_ascii(&mut rng, b"aAbg1 /:-_.,|", 30);
        let pattern = random_ascii(&mut rng, b"aAg1/-", 1);
        if pattern.is_empty() {
            continue;
        }
        for scheme in [HISTORY, DEFAULT] {
            for case_sensitive in [false, true] {
                let term = term(&pattern, case_sensitive, scheme);
                let chars: Vec<char> = text.chars().collect();
                assert_eq!(
                    single_v2(&term, term.chars[0], &chars),
                    fuzzy_v2(&term, &term.chars, &chars, &mut Dp::default()),
                    "{text:?} {pattern:?}"
                );
            }
        }
    }
    // Non-ASCII texts take the decoded path, the fast path included.
    let q = FzfQuery::new("é");
    assert_eq!(q.score("café au lait"), Some(i32::from(SCORE_MATCH)));
    assert_eq!(FzfQuery::new("É").score("café"), None);
    assert_eq!(
        FzfQuery::new("é").score("CAFÉ"),
        Some(i32::from(SCORE_MATCH))
    );
    assert_eq!(FzfQuery::new("e").score("cafÉ"), None);
}

#[test]
fn a_non_ascii_term_matches_no_ascii_text() {
    assert_eq!(FzfQuery::new("é").score("cafe"), None);
    assert_eq!(FzfQuery::new("git é").score("git status"), None);
    // The Kelvin sign folds to an ASCII `k`: a non-ASCII text can match an
    // ASCII query through folding.
    assert!(FzfQuery::new("k").score("\u{212A}ubectl").is_some());
}
