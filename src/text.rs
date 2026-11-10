//! Japanese-aware text folding and natural ordering.
//!
//! `fold` makes matching insensitive to the differences Japanese users run into
//! all the time: full-width vs half-width (ＡＢＣ / ABC, ｶﾀｶﾅ / カタカナ),
//! katakana vs hiragana (テスト / てすと) and upper vs lower case.

use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;

/// Normalise a string for searching and sorting.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.nfkc() {
        let c = kata_to_hira(c);
        for l in c.to_lowercase() {
            out.push(l);
        }
    }
    out
}

fn kata_to_hira(c: char) -> char {
    let u = c as u32;
    // ァ (U+30A1) ..= ヶ (U+30F6) map onto ぁ (U+3041) ..= ゖ (U+3096)
    if (0x30A1..=0x30F6).contains(&u) {
        char::from_u32(u - 0x60).unwrap_or(c)
    } else {
        c
    }
}

/// Natural ("human") comparison: `file2` < `file10`, digits compared by value.
/// Works on already-folded keys, so full-width digits are handled too.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = take_num(&mut ai);
                let nb = take_num(&mut bi);
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta
                    .len()
                    .cmp(&tb.len())
                    .then_with(|| ta.cmp(tb))
                    .then_with(|| na.len().cmp(&nb.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

fn take_num(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    s
}

/// Subsequence ("fuzzy") match on folded strings; returns a score (higher is better).
pub fn fuzzy_score(hay: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    if let Some(pos) = hay.find(needle) {
        return Some(1000 - pos.min(500) as i32 - hay.len().min(400) as i32 / 4);
    }
    let mut score = 0;
    let mut hi = hay.chars();
    let mut last_gap = 0;
    for n in needle.chars() {
        let mut gap = 0;
        loop {
            match hi.next() {
                Some(h) if h == n => break,
                Some(_) => gap += 1,
                None => return None,
            }
        }
        score -= gap;
        last_gap = gap;
    }
    Some(score - last_gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_japanese_variants() {
        assert_eq!(fold("テスト"), fold("てすと"));
        assert_eq!(fold("ﾃｽﾄ"), fold("てすと"));
        assert_eq!(fold("ＡＢＣ１２３"), "abc123");
        assert!(fold("報告書_ﾌｧｲﾙ.TXT").contains(&fold("ファイル")));
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "file2", "file1", "ファイル２", "ファイル10"];
        v.sort_by(|a, b| natural_cmp(&fold(a), &fold(b)));
        assert_eq!(v, ["file1", "file2", "file10", "ファイル２", "ファイル10"]);
    }

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score("documents", "dcm").is_some());
        assert!(fuzzy_score("documents", "xyz").is_none());
        assert!(fuzzy_score("ダウンロード", "ダウン").is_some());
    }
}
