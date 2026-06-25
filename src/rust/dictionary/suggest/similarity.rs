//! Ratcliff–Obershelp similarity ratio used to score fuzzy n-gram matches.
//!
//! Matches Python's `difflib.SequenceMatcher(None, a, b).ratio()` closely
//! enough to share confidence thresholds with the Python implementation.
//! Operates on Unicode `char`s, not raw bytes, so multi-byte letters compare
//! correctly.

/// See module docs. Returns `1.0` when both strings are empty (Python
/// convention) and `0.0` when they share no characters.
pub fn ratcliff_obershelp_ratio(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let total = a_chars.len() + b_chars.len();
    if total == 0 {
        return 1.0;
    }
    let matches = matching_chars(&a_chars, &b_chars);
    (2.0 * matches as f64) / (total as f64)
}

fn matching_chars(a: &[char], b: &[char]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (a_start, b_start, length) = longest_common_substring(a, b);
    if length == 0 {
        return 0;
    }
    let mut total = length;
    if a_start > 0 && b_start > 0 {
        total += matching_chars(&a[..a_start], &b[..b_start]);
    }
    let a_end = a_start + length;
    let b_end = b_start + length;
    if a_end < a.len() && b_end < b.len() {
        total += matching_chars(&a[a_end..], &b[b_end..]);
    }
    total
}

fn longest_common_substring(a: &[char], b: &[char]) -> (usize, usize, usize) {
    if a.is_empty() || b.is_empty() {
        return (0, 0, 0);
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut curr = vec![0usize; b.len() + 1];
    let mut best_a = 0usize;
    let mut best_b = 0usize;
    let mut best_len = 0usize;
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            if a[i - 1] == b[j - 1] {
                let length = prev[j - 1] + 1;
                curr[j] = length;
                if length > best_len {
                    best_len = length;
                    best_a = i - length;
                    best_b = j - length;
                }
            } else {
                curr[j] = 0;
            }
        }
        std::mem::swap(&mut prev, &mut curr);
        for cell in curr.iter_mut() {
            *cell = 0;
        }
    }
    (best_a, best_b, best_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratcliff_obershelp_basic() {
        // difflib.SequenceMatcher(None, "abcd", "abcd").ratio() == 1.0
        assert!((ratcliff_obershelp_ratio("abcd", "abcd") - 1.0).abs() < 1e-9);
        // SequenceMatcher(None, "", "").ratio() == 1.0 (Python convention)
        assert!((ratcliff_obershelp_ratio("", "") - 1.0).abs() < 1e-9);
        // SequenceMatcher(None, "abc", "xyz").ratio() == 0.0
        assert!(ratcliff_obershelp_ratio("abc", "xyz") < 1e-9);
        // "murch" vs "merge": LCS="r", recurse on "m" vs "me" (m matches=1)
        // and on "ch" vs "ge" (0). Total matches = 2; ratio = 2*2/(5+5) = 0.4
        let r = ratcliff_obershelp_ratio("murch", "merge");
        assert!((r - 0.4).abs() < 1e-9, "got {r}");
    }
}
