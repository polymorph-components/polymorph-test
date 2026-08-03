//! Feature tags: `<feature>` (case applies only to targets having the
//! feature) and `!<feature>` (case applies only to targets lacking it —
//! decline-asserting cases). Applicability is (every positive mark
//! present) ∧ (no negative mark present); unmarked cases apply
//! everywhere.

use core::fmt;

use arcstr::ArcStr;

use crate::name::is_wit_label;

/// One mark. Feature names are WIT labels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tag {
    /// `<feature>`: requires the target to have the feature.
    Requires(ArcStr),
    /// `!<feature>`: requires the target to lack the feature.
    Declines(ArcStr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagParseError {
    Empty,
    BadFeatureName { name: String, reason: &'static str },
}

impl fmt::Display for TagParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagParseError::Empty => write!(f, "empty mark"),
            TagParseError::BadFeatureName { name, reason } => {
                write!(f, "feature name `{name}` is not a WIT label ({reason})")
            }
        }
    }
}

impl std::error::Error for TagParseError {}

impl Tag {
    /// Parse from the textual form: `feature` or `!feature`.
    pub fn parse(s: &str) -> Result<Self, TagParseError> {
        let (negated, name) = match s.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        if name.is_empty() {
            return Err(TagParseError::Empty);
        }
        if let Some(reason) = is_wit_label(name) {
            return Err(TagParseError::BadFeatureName {
                name: name.to_string(),
                reason,
            });
        }
        Ok(if negated {
            Tag::Declines(ArcStr::from(name))
        } else {
            Tag::Requires(ArcStr::from(name))
        })
    }

    pub fn feature(&self) -> &str {
        match self {
            Tag::Requires(f) | Tag::Declines(f) => f,
        }
    }

    pub fn is_negative(&self) -> bool {
        matches!(self, Tag::Declines(_))
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tag::Requires(name) => f.write_str(name),
            Tag::Declines(name) => write!(f, "!{name}"),
        }
    }
}

#[cfg(feature = "serde")]
mod serde_impl {
    use super::*;
    use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

    impl Serialize for Tag {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_str(self)
        }
    }

    impl<'de> Deserialize<'de> for Tag {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let s = String::deserialize(d)?;
            Tag::parse(&s).map_err(de::Error::custom)
        }
    }
}

/// Error constructing a mark set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagsError {
    /// The same feature appears with both polarities: such a case can
    /// never apply anywhere, which is always a bug (disabling a case
    /// belongs to ratchets, not mark tricks).
    Contradiction(String),
    /// The same mark appears twice.
    Duplicate(String),
}

impl fmt::Display for TagsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagsError::Contradiction(feature) => {
                write!(f, "contradictory tags: both `{feature}` and `!{feature}`")
            }
            TagsError::Duplicate(mark) => write!(f, "duplicate mark `{mark}`"),
        }
    }
}

impl std::error::Error for TagsError {}

/// A case's validated mark set: at most one mark per feature name (a
/// feature marked with both polarities is rejected — see
/// [`TagsError::Contradiction`]). Stored refcounted so that sharing a
/// row's tags across thousands of generated cases is a refcount bump,
/// not a `Vec` allocation per case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags(std::sync::Arc<[Tag]>);

impl Tags {
    /// Parse and validate a slice of textual tags.
    pub fn parse_all<S: AsRef<str>>(tags: &[S]) -> Result<Self, String> {
        let parsed = tags
            .iter()
            .map(|t| Tag::parse(t.as_ref()).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        Tags::new(parsed).map_err(|e| e.to_string())
    }

    /// Validate and construct.
    pub fn new(tags: Vec<Tag>) -> Result<Self, TagsError> {
        for (i, mark) in tags.iter().enumerate() {
            for earlier in &tags[..i] {
                if earlier == mark {
                    return Err(TagsError::Duplicate(mark.to_string()));
                }
                if earlier.feature() == mark.feature() {
                    return Err(TagsError::Contradiction(mark.feature().to_string()));
                }
            }
        }
        Ok(Tags(tags.into()))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tag> {
        self.0.iter()
    }

    pub fn as_slice(&self) -> &[Tag] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Does a target missing `missing_features` get this case?
    /// Predicate: every `Requires` feature is NOT missing, and every
    /// `Declines` feature IS missing.
    pub fn applies<S: AsRef<str>>(&self, missing_features: &[S]) -> bool {
        let missing = |f: &str| missing_features.iter().any(|m| m.as_ref() == f);
        self.0.iter().all(|mark| match mark {
            Tag::Requires(f) => !missing(f),
            Tag::Declines(f) => missing(f),
        })
    }

    /// The mark that excludes this case for the given missing-set, if
    /// any (the `not-applicable` detail).
    pub fn excluding_mark<S: AsRef<str>>(&self, missing_features: &[S]) -> Option<&Tag> {
        let missing = |f: &str| missing_features.iter().any(|m| m.as_ref() == f);
        self.0.iter().find(|mark| match mark {
            Tag::Requires(f) => missing(f),
            Tag::Declines(f) => !missing(f),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for s in ["big-int", "!big-int", "hsm", "!aes-gcm-any-iv"] {
            assert_eq!(Tag::parse(s).unwrap().to_string(), s);
        }
        assert!(Tag::parse("").is_err());
        assert!(Tag::parse("!").is_err());
        assert!(Tag::parse("Big-Int").is_err());
        assert!(Tag::parse("big_int").is_err());
        assert!(Tag::parse("!!x").is_err());
    }

    #[test]
    fn construction_invariants() {
        let hsm = || Tag::parse("hsm").unwrap();
        let not_hsm = || Tag::parse("!hsm").unwrap();
        assert_eq!(
            Tags::new(vec![hsm(), not_hsm()]).unwrap_err(),
            TagsError::Contradiction("hsm".into())
        );
        assert_eq!(
            Tags::new(vec![not_hsm(), not_hsm()]).unwrap_err(),
            TagsError::Duplicate("!hsm".into())
        );
        Tags::new(vec![hsm(), Tag::parse("!sim").unwrap()]).unwrap();
    }

    #[test]
    fn applicability() {
        let unmarked = Tags::default();
        assert!(unmarked.applies::<&str>(&[]));
        assert!(unmarked.applies(&["anything"]));

        let requires = Tags::new(vec![Tag::parse("hsm").unwrap()]).unwrap();
        assert!(requires.applies::<&str>(&[]));
        assert!(!requires.applies(&["hsm"]));
        assert_eq!(
            requires.excluding_mark(&["hsm"]).unwrap().to_string(),
            "hsm"
        );

        let declines = Tags::new(vec![Tag::parse("!hsm").unwrap()]).unwrap();
        assert!(!declines.applies::<&str>(&[]));
        assert!(declines.applies(&["hsm"]));

        // conjunction: requires a AND b, declines c
        let multi = Tags::new(vec![
            Tag::parse("a").unwrap(),
            Tag::parse("b").unwrap(),
            Tag::parse("!c").unwrap(),
        ])
        .unwrap();
        assert!(multi.applies(&["c"]));
        assert!(!multi.applies(&["a", "c"]));
        assert!(!multi.applies::<&str>(&[]));
    }
}
