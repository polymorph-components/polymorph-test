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

/// Maximum total name length in bytes.
pub const MAX_NAME_LEN: usize = 256;
/// Maximum segment length in bytes.
pub const MAX_SEGMENT_LEN: usize = 64;
/// Custom section name reserved for feature-mark metadata.
pub const TAGS_SECTION: &str = "component-test:tags@0.1";

/// A validated case name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CaseName(String);

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

/// Is `s` a valid WIT label (kebab-case: words of `[a-z][a-z0-9]*`
/// joined by `-`)?
pub fn is_wit_label(s: &str) -> Option<&'static str> {
    if s.is_empty() {
        return Some("empty");
    }
    for word in s.split('-') {
        let mut chars = word.chars();
        match chars.next() {
            None => return Some("empty word (leading/trailing/double `-`)"),
            Some('a'..='z') => {}
            Some(_) => return Some("word must start with a lowercase letter"),
        }
        if !chars.all(|c| matches!(c, 'a'..='z' | '0'..='9')) {
            return Some("word may contain only [a-z0-9]");
        }
    }
    None
}

impl CaseName {
    /// Validate `name` against the normative grammar.
    pub fn parse(name: &str) -> Result<Self, NameError> {
        if name.is_empty() {
            return Err(NameError::Empty);
        }
        if name.len() > MAX_NAME_LEN {
            return Err(NameError::TooLong(name.len()));
        }
        let segments: Vec<&str> = name.split('/').collect();
        let last = segments.len() - 1;
        for (i, seg) in segments.iter().enumerate() {
            if seg.is_empty() {
                return Err(NameError::EmptySegment);
            }
            if seg.len() > MAX_SEGMENT_LEN {
                return Err(NameError::SegmentTooLong(seg.to_string()));
            }
            if *seg == "." || *seg == ".." {
                return Err(NameError::DotSegment(seg.to_string()));
            }
            if let Some(ch) = seg.chars().find(|c| !segment_char_ok(*c)) {
                return Err(NameError::BadChar {
                    segment: seg.to_string(),
                    ch,
                });
            }
            if i != last {
                if let Some(reason) = is_wit_label(seg) {
                    return Err(NameError::NonLabelPrefix {
                        segment: seg.to_string(),
                        reason,
                    });
                }
            }
        }
        Ok(CaseName(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Segments of the name.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// The grouping prefix (everything but the leaf), if any.
    pub fn prefix(&self) -> Option<&str> {
        self.0.rsplit_once('/').map(|(p, _)| p)
    }

    /// The leaf segment.
    pub fn leaf(&self) -> &str {
        self.0.rsplit_once('/').map(|(_, l)| l).unwrap_or(&self.0)
    }
}

impl fmt::Display for CaseName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for CaseName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "serde")]
mod serde_impl {
    use super::*;
    use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

    impl Serialize for CaseName {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&self.0)
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
    fn accessors() {
        let n = CaseName::parse("sample/math/add").unwrap();
        assert_eq!(n.prefix(), Some("sample/math"));
        assert_eq!(n.leaf(), "add");
        let n = CaseName::parse("solo").unwrap();
        assert_eq!(n.prefix(), None);
        assert_eq!(n.leaf(), "solo");
    }
}
