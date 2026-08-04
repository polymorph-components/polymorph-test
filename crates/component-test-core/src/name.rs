//! The normative case-name grammar (README "Case names"):
//!
//! ```text
//! name    = segment *( "/" segment )          ; 1–256 bytes total
//! segment = 1*64 of [a-z 0-9 - _ .]           ; a segment is never "." or ".."
//! ```
//!
//! All segments except the last must additionally be valid WIT labels
//! (kebab-case: words of `[a-z][a-z0-9]*` joined by `-`).
//!
//! Byte equality is the only equality.

use core::fmt;

use arcstr::{ArcStr, Substr};

/// Maximum total name length in bytes.
pub const MAX_NAME_LEN: usize = 256;
/// Maximum segment length in bytes.
pub const MAX_SEGMENT_LEN: usize = 64;
/// Custom section name reserved for feature-mark metadata.
pub const TAGS_SECTION: &str = "component-test:tags@0.1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong(usize),
    EmptySegment,
    SegmentTooLong(String),
    DotSegment(String),
    BadChar {
        segment: String,
        ch: char,
    },
    NonLabelPrefix {
        segment: String,
        reason: &'static str,
    },
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::Empty => write!(f, "name is empty"),
            NameError::TooLong(n) => write!(f, "name is {n} bytes (max {MAX_NAME_LEN})"),
            NameError::EmptySegment => write!(f, "empty segment"),
            NameError::SegmentTooLong(s) => {
                write!(f, "segment `{s}` exceeds {MAX_SEGMENT_LEN} bytes")
            }
            NameError::DotSegment(s) => write!(f, "segment `{s}` is forbidden"),
            NameError::BadChar { segment, ch } => {
                write!(f, "segment `{segment}` contains forbidden character {ch:?}")
            }
            NameError::NonLabelPrefix { segment, reason } => write!(
                f,
                "non-leaf segment `{segment}` is not a WIT label ({reason})"
            ),
        }
    }
}

impl std::error::Error for NameError {}

fn segment_char_ok(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9' | '-' | '_' | '.')
}

/// Is `s` a valid WIT label? Kebab-case words joined by `-`: the first
/// word is `[a-z][a-z0-9]*`; subsequent words may also be number-only
/// (`[0-9]+`), per the amended component-model label grammar (e.g.
/// `sha256-2048`).
pub fn is_wit_label(s: &str) -> Option<&'static str> {
    if s.is_empty() {
        return Some("empty");
    }
    for (i, word) in s.split('-').enumerate() {
        let mut chars = word.chars();
        match chars.next() {
            None => return Some("empty word (leading/trailing/double `-`)"),
            Some('a'..='z') => {
                if !chars.all(|c| matches!(c, 'a'..='z' | '0'..='9')) {
                    return Some("word may contain only [a-z0-9]");
                }
            }
            Some('0'..='9') if i > 0 => {
                if !chars.all(|c| c.is_ascii_digit()) {
                    return Some("number word may contain only digits");
                }
            }
            Some('0'..='9') => return Some("first word must start with a lowercase letter"),
            Some(_) => return Some("word must start with a lowercase letter or digits"),
        }
    }
    None
}

/// A validated case name: a (possibly empty) grouping prefix plus a
/// single-segment leaf, both sharing refcounted buffers. Invariant:
/// slashes appear only in the prefix, so representation is canonical
/// and equality/ordering/hashing derive structurally. The logical name
/// is `prefix + "/" + leaf` (just `leaf` when the prefix is empty).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CaseName {
    prefix: Substr,
    leaf: Substr,
}

impl CaseName {
    /// Validate `name` against the normative grammar.
    pub fn parse(name: &str) -> Result<Self, NameError> {
        Self::new(ArcStr::from(name))
    }

    /// Validate an already-shared string against the grammar (no copy:
    /// prefix and leaf are substrings of `name`).
    pub fn new(name: ArcStr) -> Result<Self, NameError> {
        if name.is_empty() {
            return Err(NameError::Empty);
        }
        if name.len() > MAX_NAME_LEN {
            return Err(NameError::TooLong(name.len()));
        }
        let split = name.rfind('/');
        // Validate without allocating: everything before the final
        // slash is prefix (label segments), the rest is the leaf.
        if let Some(i) = split {
            for seg in name[..i].split('/') {
                validate_segment(seg)?;
                if let Some(reason) = is_wit_label(seg) {
                    return Err(NameError::NonLabelPrefix {
                        segment: seg.to_string(),
                        reason,
                    });
                }
            }
            validate_segment(&name[i + 1..])?;
        } else {
            validate_segment(&name)?;
        }
        Ok(match split {
            Some(i) => CaseName {
                prefix: name.substr(..i),
                leaf: name.substr(i + 1..),
            },
            None => CaseName {
                prefix: Substr::new(),
                leaf: name.substr(..),
            },
        })
    }

    /// Build a name from an already-canonical split: `prefix` is
    /// everything before the final slash (label segments; may be
    /// empty), `leaf` is the final segment (never contains `/`).
    /// Validates the full grammar without allocating — both parts are
    /// typically substrings of shared per-row buffers, so registering
    /// thousands of generated cases costs no per-case string copies.
    pub fn from_parts(prefix: Substr, leaf: Substr) -> Result<Self, NameError> {
        let total = if prefix.is_empty() {
            leaf.len()
        } else {
            prefix.len() + 1 + leaf.len()
        };
        if total > MAX_NAME_LEN {
            return Err(NameError::TooLong(total));
        }
        for seg in (!prefix.is_empty())
            .then(|| prefix.split('/'))
            .into_iter()
            .flatten()
        {
            validate_segment(seg)?;
            if let Some(reason) = is_wit_label(seg) {
                return Err(NameError::NonLabelPrefix {
                    segment: seg.to_string(),
                    reason,
                });
            }
        }
        if leaf.contains('/') {
            // A slashed leaf would break the canonical-representation
            // invariant (slashes live only in the prefix).
            return Err(NameError::BadChar {
                segment: leaf.to_string(),
                ch: '/',
            });
        }
        validate_segment(&leaf)?;
        Ok(CaseName { prefix, leaf })
    }

    /// Build a generated-case name from a row's static prefix (label
    /// segments only) and the case's relative name. Single-segment
    /// relative names attach without any string assembly; multi-segment
    /// ones fold their head into the prefix (one allocation).
    /// Equality/order/hash match `parse("{prefix}/{leaf}")` exactly.
    pub fn prefixed(prefix: ArcStr, leaf: ArcStr) -> Result<Self, NameError> {
        if prefix.is_empty() {
            return Self::new(leaf);
        }
        if prefix.len() + 1 + leaf.len() > MAX_NAME_LEN {
            return Err(NameError::TooLong(prefix.len() + 1 + leaf.len()));
        }
        if leaf.is_empty() {
            return Err(NameError::EmptySegment);
        }
        for seg in prefix.split('/').chain(leaf.split('/').rev().skip(1)) {
            validate_segment(seg)?;
            if let Some(reason) = is_wit_label(seg) {
                return Err(NameError::NonLabelPrefix {
                    segment: seg.to_string(),
                    reason,
                });
            }
        }
        match leaf.rfind('/') {
            None => {
                validate_segment(&leaf)?;
                Ok(CaseName {
                    prefix: prefix.substr(..),
                    leaf: leaf.substr(..),
                })
            }
            Some(i) => {
                validate_segment(&leaf[i + 1..])?;
                // Slashes live in the prefix (canonical form): fold the
                // relative name's head into it.
                let joined = ArcStr::from(format!("{prefix}/{}", &leaf[..i]));
                Ok(CaseName {
                    prefix: joined.substr(..),
                    leaf: leaf.substr(i + 1..),
                })
            }
        }
    }

    /// The logical name. Borrowed for single-segment names; assembled
    /// otherwise.
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        if self.prefix.is_empty() {
            std::borrow::Cow::Borrowed(&self.leaf)
        } else {
            std::borrow::Cow::Owned(format!("{}/{}", self.prefix, self.leaf))
        }
    }

    /// Segments of the logical name.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        let prefix = (!self.prefix.is_empty()).then_some(self.prefix.split('/'));
        prefix
            .into_iter()
            .flatten()
            .chain(std::iter::once(&*self.leaf))
    }

    /// The grouping prefix (everything but the final segment), if any.
    pub fn prefix(&self) -> Option<&str> {
        (!self.prefix.is_empty()).then_some(&self.prefix)
    }

    /// The final segment.
    pub fn leaf(&self) -> &str {
        &self.leaf
    }
}

impl fmt::Display for CaseName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.prefix.is_empty() {
            f.write_str(&self.prefix)?;
            f.write_str("/")?;
        }
        f.write_str(&self.leaf)
    }
}

fn validate_segment(seg: &str) -> Result<(), NameError> {
    if seg.is_empty() {
        return Err(NameError::EmptySegment);
    }
    if seg.len() > MAX_SEGMENT_LEN {
        return Err(NameError::SegmentTooLong(seg.to_string()));
    }
    if seg == "." || seg == ".." {
        return Err(NameError::DotSegment(seg.to_string()));
    }
    if let Some(ch) = seg.chars().find(|c| !segment_char_ok(*c)) {
        return Err(NameError::BadChar {
            segment: seg.to_string(),
            ch,
        });
    }
    Ok(())
}

#[cfg(feature = "serde")]
mod serde_impl {
    use super::*;
    use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

    impl Serialize for CaseName {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_str(self)
        }
    }

    impl<'de> Deserialize<'de> for CaseName {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let s = String::deserialize(d)?;
            CaseName::parse(&s).map_err(de::Error::custom)
        }
    }
}

/// Normalize an arbitrary source-language identifier into grammar-legal
/// form: lowercase, whitespace and forbidden characters to `_`.
/// Callers MUST re-check uniqueness after normalization (distinct
/// sources can merge).
pub fn normalize_segment(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for c in source.chars() {
        match c {
            'a'..='z' | '0'..='9' | '-' | '.' => out.push(c),
            'A'..='Z' => out.push(c.to_ascii_lowercase()),
            _ => out.push('_'),
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    let mut out = if out.len() > MAX_SEGMENT_LEN {
        out.truncate(MAX_SEGMENT_LEN);
        out
    } else {
        out
    };
    if out == "." || out == ".." {
        out = out.replace('.', "_");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for n in [
            "a",
            "sample/math/add",
            "group/source/case-375",
            "alg/wycheproof/0x1a2b_16384.v2",
            "a-b/c1/leaf",
            "sha256-2048/leaf",
            "rsassa-pkcs1-v15-sha256-2048/wycheproof/tc1",
        ] {
            CaseName::parse(n).unwrap_or_else(|e| panic!("{n}: {e}"));
        }
    }

    #[test]
    fn invalid_names() {
        // empty / structure
        assert!(CaseName::parse("").is_err());
        assert!(CaseName::parse("/a").is_err());
        assert!(CaseName::parse("a/").is_err());
        assert!(CaseName::parse("a//b").is_err());
        // charset
        assert!(CaseName::parse("A/b").is_err());
        assert!(CaseName::parse("a/b c").is_err());
        assert!(CaseName::parse("a/б").is_err());
        // dot segments
        assert!(CaseName::parse("./a").is_err());
        assert!(CaseName::parse("a/../b").is_err());
        // non-leaf must be WIT label
        assert!(CaseName::parse("375/leaf").is_err());
        assert!(CaseName::parse("a_b/leaf").is_err());
        assert!(CaseName::parse("a.b/leaf").is_err());
        assert!(CaseName::parse("-a/leaf").is_err());
        assert!(CaseName::parse("a-/leaf").is_err());
        // ...but all are fine as the leaf
        assert!(CaseName::parse("a/375").is_ok());
        assert!(CaseName::parse("a/a_b").is_ok());
        assert!(CaseName::parse("a/a.b").is_ok());
        // length caps
        let long_seg = "a".repeat(MAX_SEGMENT_LEN + 1);
        assert!(CaseName::parse(&long_seg).is_err());
        let long_name = format!("{}/{}", "ab/".repeat(90), "leaf");
        assert!(CaseName::parse(&long_name).is_err());
    }

    #[test]
    fn normalization() {
        assert_eq!(normalize_segment("SHA-256"), "sha-256");
        assert_eq!(normalize_segment("case 375!"), "case_375_");
        assert_eq!(normalize_segment(""), "_");
        assert_eq!(normalize_segment(".."), "__");
    }

    #[test]
    fn prefixed_matches_parsed() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let parsed = CaseName::parse("aes-gcm/wycheproof/tc305/whole").unwrap();
        let split = CaseName::prefixed(
            arcstr::literal!("aes-gcm/wycheproof"),
            arcstr::literal!("tc305/whole"),
        )
        .unwrap();
        assert_eq!(parsed, split);
        assert_eq!(parsed.cmp(&split), core::cmp::Ordering::Equal);
        let h = |n: &CaseName| {
            let mut s = DefaultHasher::new();
            n.hash(&mut s);
            s.finish()
        };
        assert_eq!(h(&parsed), h(&split));
        assert_eq!(parsed.to_string(), split.to_string());
        assert_eq!(split.as_str(), "aes-gcm/wycheproof/tc305/whole");
        assert_eq!(split.leaf(), "whole");
        assert_eq!(split.prefix().as_deref(), Some("aes-gcm/wycheproof/tc305"));
        // grammar still enforced through the split constructor
        assert!(CaseName::prefixed(arcstr::literal!("375"), arcstr::literal!("x")).is_err());
        assert!(CaseName::prefixed(arcstr::literal!("a"), arcstr::literal!("B")).is_err());
        assert!(CaseName::prefixed(arcstr::literal!("a"), arcstr::literal!("375/leaf")).is_err());
        assert!(CaseName::prefixed(arcstr::literal!("a"), arcstr::literal!("x_y")).is_ok());
    }

    #[test]
    fn from_parts_matches_parsed() {
        let full = ArcStr::from("aes-gcm/wycheproof/tc305/whole");
        let i = full.rfind('/').unwrap();
        let split = CaseName::from_parts(full.substr(..i), full.substr(i + 1..)).unwrap();
        let parsed = CaseName::parse("aes-gcm/wycheproof/tc305/whole").unwrap();
        assert_eq!(parsed, split);
        assert_eq!(split.as_str(), "aes-gcm/wycheproof/tc305/whole");
        // empty prefix = bare leaf
        let solo = CaseName::from_parts(Substr::new(), ArcStr::from("solo").substr(..)).unwrap();
        assert_eq!(solo, CaseName::parse("solo").unwrap());
        // invariants still enforced, allocation-free path or not
        let bad = |p: &str, l: &str| {
            CaseName::from_parts(ArcStr::from(p).substr(..), ArcStr::from(l).substr(..))
        };
        assert!(bad("375", "x").is_err()); // non-label prefix segment
        assert!(bad("a", "B").is_err()); // charset
        assert!(bad("a", "tc1/whole").is_err()); // slashed leaf: not canonical
        assert!(bad("a", "").is_err());
        assert!(bad("", "").is_err());
    }

    #[test]
    fn accessors() {
        let n = CaseName::parse("sample/math/add").unwrap();
        assert_eq!(n.prefix().as_deref(), Some("sample/math"));
        assert_eq!(n.leaf(), "add");
        let n = CaseName::parse("solo").unwrap();
        assert_eq!(n.prefix(), None);
        assert_eq!(n.leaf(), "solo");
    }
}

/// Const-evaluable name validation (the grammar of [`CaseName::parse`],
/// usable in `const` asserts for compile-time literal checking).
/// Equivalence with `parse` is tested below.
pub const fn const_valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > MAX_NAME_LEN {
        return false;
    }
    // Find the last '/' to know which segment is the leaf.
    let mut last_slash: Option<usize> = None;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' {
            last_slash = Some(i);
        }
        i += 1;
    }
    let mut seg_start = 0;
    let mut i = 0;
    loop {
        let at_end = i == b.len();
        if at_end || b[i] == b'/' {
            let seg_len = i - seg_start;
            if seg_len == 0 || seg_len > MAX_SEGMENT_LEN {
                return false;
            }
            // "." / ".." forbidden
            if (seg_len == 1 && b[seg_start] == b'.')
                || (seg_len == 2 && b[seg_start] == b'.' && b[seg_start + 1] == b'.')
            {
                return false;
            }
            let is_leaf = match last_slash {
                None => true,
                Some(ls) => seg_start > ls,
            };
            if is_leaf {
                let mut j = seg_start;
                while j < i {
                    if !const_segment_char(b[j]) {
                        return false;
                    }
                    j += 1;
                }
            } else if !const_valid_label_range(b, seg_start, i) {
                return false;
            }
            if at_end {
                return true;
            }
            seg_start = i + 1;
        }
        i += 1;
    }
}

const fn const_segment_char(c: u8) -> bool {
    matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.')
}

/// Const label check over `b[start..end]` (kebab-case; first word
/// `[a-z][a-z0-9]*`, later words may be number-only, per the amended
/// component-model label grammar).
const fn const_valid_label_range(b: &[u8], start: usize, end: usize) -> bool {
    if start >= end {
        return false;
    }
    let mut i = start;
    let mut first_word = true;
    loop {
        // word start
        if i >= end {
            return false;
        }
        if b[i].is_ascii_lowercase() {
            i += 1;
            while i < end && (b[i].is_ascii_lowercase() || b[i].is_ascii_digit()) {
                i += 1;
            }
        } else if b[i].is_ascii_digit() && !first_word {
            i += 1;
            while i < end && b[i].is_ascii_digit() {
                i += 1;
            }
        } else {
            return false;
        }
        first_word = false;
        if i == end {
            return true;
        }
        if b[i] != b'-' {
            return false;
        }
        i += 1; // past '-', next word
    }
}

/// Const-evaluable tag validation (`feature` or `!feature`, feature a
/// WIT label). Also rejects anything that would corrupt the
/// space/newline-delimited section record format.
pub const fn const_valid_tag(s: &str) -> bool {
    let b = s.as_bytes();
    let start = if !b.is_empty() && b[0] == b'!' { 1 } else { 0 };
    b.len() > start && const_valid_label_range(b, start, b.len())
}

#[cfg(test)]
mod const_tests {
    use super::*;

    #[test]
    fn const_name_matches_parse() {
        for s in [
            "a",
            "sample/math/add",
            "group/source/case-375",
            "alg/wycheproof/0x1a2b_16384.v2",
            "a-b/c1/leaf",
            "",
            "/a",
            "a/",
            "a//b",
            "A/b",
            "a/b c",
            "./a",
            "a/../b",
            "375/leaf",
            "2048-sha/leaf",
            "a-2048x/leaf",
            "a_b/leaf",
            "a.b/leaf",
            "-a/leaf",
            "a-/leaf",
            "a/375",
            "a/a_b",
            "a/a.b",
            "a\nb",
            "solo",
            "sha256-2048/leaf",
            "2048-sha/leaf",
            "a-2048x/leaf",
        ] {
            assert_eq!(
                const_valid_name(s),
                CaseName::parse(s).is_ok(),
                "mismatch for {s:?}"
            );
        }
        let long_seg = "a".repeat(MAX_SEGMENT_LEN + 1);
        assert!(!const_valid_name(&long_seg));
    }

    #[test]
    fn const_tag_matches_parse() {
        use crate::tags::Tag;
        for s in [
            "big-int", "!big-int", "hsm", "", "!", "Big-Int", "big_int", "!!x", "a b", "a\nb",
        ] {
            assert_eq!(
                const_valid_tag(s),
                Tag::parse(s).is_ok(),
                "mismatch for {s:?}"
            );
        }
    }
}
