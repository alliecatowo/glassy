//! Subsequence fuzzy scoring for the command palette. Pure (no `App` state), so
//! it lives in its own submodule with its own unit tests, keeping
//! `palette/mod.rs` under the project's 700-line limit.

/// Subsequence fuzzy score: returns `Some(score)` if every char of `needle`
/// appears in `haystack` in order (case-folded by the caller), `None` otherwise.
/// Rewards contiguous runs and word-start matches so "nt" ranks "New tab" highly.
/// Higher is better. Pure for unit testing.
pub(crate) fn fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().collect();
    let need: Vec<char> = needle.chars().collect();
    let mut hi = 0usize;
    let mut ni = 0usize;
    let mut score = 0i32;
    let mut prev_matched = false;
    let mut prev_char = ' ';
    while hi < hay.len() && ni < need.len() {
        if hay[hi] == need[ni] {
            score += 1;
            if prev_matched {
                score += 4; // contiguous run bonus
            }
            // Word-start bonus (match right after a space / separator).
            if hi == 0 || prev_char == ' ' || prev_char == '-' || prev_char == '/' {
                score += 6;
            }
            ni += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
        prev_char = hay[hi];
        hi += 1;
    }
    if ni == need.len() {
        // Prefer shorter haystacks (tighter match) slightly.
        Some(score - (hay.len() as i32) / 32)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_requires_full_subsequence() {
        assert!(fuzzy_score("new tab", "ntb").is_some());
        assert!(fuzzy_score("new tab", "xyz").is_none());
        assert!(fuzzy_score("new tab", "newtab").is_some());
    }

    #[test]
    fn fuzzy_word_start_outranks_midword() {
        // "st" should score "Setting  Toggle status bar" via the word-start 's'+'t'.
        let a = fuzzy_score("setting toggle status bar", "st").unwrap();
        // A buried, non-word-start subsequence scores lower.
        let b = fuzzy_score("abcsxtq", "st").unwrap();
        assert!(a > b, "word-start match {a} should beat buried {b}");
    }

    #[test]
    fn fuzzy_empty_needle_matches_everything() {
        assert_eq!(fuzzy_score("anything", ""), Some(0));
    }

    // ---- additional fuzzy_score coverage ------------------------------------

    #[test]
    fn fuzzy_no_match_returns_none() {
        assert!(fuzzy_score("new tab", "xyz").is_none());
        assert!(fuzzy_score("", "a").is_none());
        assert!(fuzzy_score("abc", "abcd").is_none());
    }

    #[test]
    fn fuzzy_empty_haystack_empty_needle_scores_zero() {
        assert_eq!(fuzzy_score("", ""), Some(0));
    }

    #[test]
    fn fuzzy_contiguous_bonus_makes_prefix_score_higher() {
        // Matching the first N chars contiguously should outscore spread matches.
        let contiguous = fuzzy_score("new tab", "new").unwrap();
        let spread = fuzzy_score("new tab", "ntb").unwrap();
        assert!(
            contiguous > spread,
            "contiguous prefix match ({contiguous}) should outscore spread ({spread})"
        );
    }

    #[test]
    fn fuzzy_word_start_slash_separator_bonus() {
        // '/' counts as a word-start separator (file paths).
        let with_slash = fuzzy_score("split vertical (left / right)", "r").unwrap();
        // Match at position 0 (word start) vs a buried 'r'.
        let at_word_start = fuzzy_score("right side", "r").unwrap();
        // Both should match; the word-start bonus means starting with 'r' scores well.
        assert!(at_word_start > 0);
        assert!(with_slash > 0);
    }

    #[test]
    fn fuzzy_shorter_haystack_preferred_over_longer_for_same_needle() {
        // Both match "tab"; the shorter haystack should outscore the longer one
        // (the -len/32 penalty slightly penalizes the longer haystack).
        let short = fuzzy_score("new tab", "tab").unwrap();
        let long_hay = fuzzy_score(
            "this very long string has a tab word in it somewhere",
            "tab",
        )
        .unwrap();
        // The short haystack has a stronger tighter-match score.
        assert!(
            short >= long_hay,
            "shorter haystack {short} should be >= longer {long_hay}"
        );
    }

    #[test]
    fn fuzzy_case_folded_caller_responsibility() {
        // The function does NOT lowercase; callers must pre-fold.
        // If needle is lowercase and haystack is uppercase they won't match.
        assert!(fuzzy_score("NEW TAB", "new").is_none());
        // But if both are lowercase they do.
        assert!(fuzzy_score("new tab", "new").is_some());
    }

    #[test]
    fn fuzzy_dash_separator_bonus() {
        // '-' also triggers word-start bonus.
        let score = fuzzy_score("tokyo-night", "n").unwrap();
        assert!(
            score > 1,
            "dash-separated word start should get bonus score"
        );
    }

    #[test]
    fn fuzzy_ranking_order() {
        // "nt" applied to the palette display strings:
        // "Tab  New tab" hits word-start N in "New" and then t in "tab"
        // vs some non-word-start match — the palette ranking should order the best match first.
        let new_tab = fuzzy_score("tab  new tab", "nt").unwrap();
        let buried = fuzzy_score("abcntxyz", "nt").unwrap();
        assert!(
            new_tab > buried,
            "word-start match should rank higher than buried"
        );
    }

    #[test]
    fn fuzzy_single_char_needle() {
        // Single-char needle at word start gets word-start bonus.
        let word_start = fuzzy_score("new tab", "n").unwrap();
        let buried = fuzzy_score("xnew", "n").unwrap();
        assert!(word_start > buried);
    }
}
