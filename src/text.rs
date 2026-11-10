//! Japanese-aware text folding and natural ordering.
//!
//! `fold` makes matching insensitive to the differences Japanese users run into
//! all the time: full-width vs half-width (ＡＢＣ / ABC, ｶﾀｶﾅ / カタカナ),
//! katakana vs hiragana (テスト / てすと) and upper vs lower case.

use std::cmp::Ordering;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfkc_quick};

/// Name for display. macOS hands back names in decomposed form (NFD), where
/// "ガ" is "カ" + U+3099; composing to NFC makes dakuten/handakuten render as one glyph.
pub fn display_name(s: &std::ffi::OsStr) -> String {
    s.to_string_lossy().nfc().collect()
}

/// Normalise a string for searching and sorting.
pub fn fold(s: &str) -> String {
    // Plain ASCII names need no Unicode work at all.
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(s.len());
    // Text that is already normalised (ordinary kanji and kana) skips the NFKC pass;
    // only names with half-width kana, full-width Latin, combining marks etc. need it.
    if is_nfkc_quick(s.chars()) == IsNormalized::Yes {
        s.chars().for_each(|c| push_folded(&mut out, c));
    } else {
        s.nfkc().for_each(|c| push_folded(&mut out, c));
    }
    out
}

#[inline]
fn push_folded(out: &mut String, c: char) {
    let c = kata_to_hira(c);
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
    } else {
        out.extend(c.to_lowercase());
    }
}

#[inline]
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
///
/// Runs on the UTF-8 bytes without allocating: byte order equals code point order,
/// and ASCII digits are always whole characters.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    loop {
        match (a.get(i), b.get(j)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = digit_run(&a[i..]);
                let nb = digit_run(&b[j..]);
                i += na.len();
                j += nb.len();
                let ta = trim_zeros(na);
                let tb = trim_zeros(nb);
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
                    return x.cmp(y);
                }
                i += 1;
                j += 1;
            }
        }
    }
}

#[inline]
fn digit_run(s: &[u8]) -> &[u8] {
    let n = s.iter().position(|c| !c.is_ascii_digit()).unwrap_or(s.len());
    &s[..n]
}

#[inline]
fn trim_zeros(s: &[u8]) -> &[u8] {
    let n = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
    &s[n..]
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
    fn nfd_names_compose_for_display() {
        let nfd = "\u{30CF}\u{309A}\u{30F3}\u{30AB}\u{3099}"; // パンガ, decomposed
        assert_eq!(display_name(std::ffi::OsStr::new(nfd)), "パンガ");
        assert_eq!(fold(nfd), fold("ぱんが"));
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "file2", "file1", "ファイル２", "ファイル10"];
        v.sort_by(|a, b| natural_cmp(&fold(a), &fold(b)));
        assert_eq!(v, ["file1", "file2", "file10", "ファイル２", "ファイル10"]);
    }

    /// The straightforward definitions; the fast paths above must agree with them.
    fn fold_reference(s: &str) -> String {
        s.nfkc().map(kata_to_hira).flat_map(char::to_lowercase).collect()
    }

    fn natural_cmp_reference(a: &str, b: &str) -> Ordering {
        fn num(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
            let mut s = String::new();
            while let Some(c) = it.next_if(char::is_ascii_digit) {
                s.push(c);
            }
            s
        }
        let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
        loop {
            match (ai.peek().copied(), bi.peek().copied()) {
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                    let (na, nb) = (num(&mut ai), num(&mut bi));
                    let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                    let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
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

    const SAMPLES: &[&str] = &[
        "", "a", "A", "file", "file1", "file01", "file001", "file2", "file10", "file10b", "file9z", "FILE10.TXT",
        "IMG_00042.JPG", "img_42.jpg", "Report 2024-11-10 (2).pdf", "0", "00", "007", "7", "10", "9", "1a2b3", "1a2b03",
        "資料", "資料1", "資料10", "資料２", "資料_テスト.txt", "研究メモ3版.docx", "テスト", "てすと", "ﾃｽﾄ", "ﾃﾞｰﾀ", "データ",
        "ＡＢＣ１２３", "ｶﾀｶﾅ ファイル 12.png", "㈱サンプル", "①②③", "ｱ゛", "パンガ", "\u{30CF}\u{309A}\u{30F3}\u{30AB}\u{3099}",
        "Ǆungla", "ÀÉÎõü", "ß", "İstanbul", "ﬁle", "Ω", "Ω", "한글", "😀 emoji.png", "é", "e\u{301}", "ヴァイオリン", "ｳﾞｧ",
    ];

    #[test]
    fn fast_fold_matches_reference() {
        for s in SAMPLES {
            assert_eq!(fold(s), fold_reference(s), "{s:?}");
        }
    }

    #[test]
    fn fast_natural_cmp_matches_reference() {
        let keys: Vec<String> = SAMPLES.iter().map(|s| fold(s)).collect();
        for a in keys.iter().map(String::as_str).chain(SAMPLES.iter().copied()) {
            for b in keys.iter().map(String::as_str).chain(SAMPLES.iter().copied()) {
                assert_eq!(natural_cmp(a, b), natural_cmp_reference(a, b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score("documents", "dcm").is_some());
        assert!(fuzzy_score("documents", "xyz").is_none());
        assert!(fuzzy_score("ダウンロード", "ダウン").is_some());
    }
}
