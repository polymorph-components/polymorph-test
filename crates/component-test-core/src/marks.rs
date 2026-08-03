//! Feature marks: `<feature>` (case applies only to targets having the
//! feature) and `!<feature>` (case applies only to targets lacking it —
//! decline-asserting cases). Applicability is (every positive mark
//! present) ∧ (no negative mark present); unmarked cases apply
//! everywhere.

use core::fmt;

use crate::name::is_wit_label;

/// One mark. Feature names are WIT labels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mark {
    /// `<feature>`: requires the target to have the feature.
    Requires(String),
    /// `!<feature>`: requires the target to lack the feature.
    Declines(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkParseError {
    Empty,
    BadFeatureName { name: String, reason: &'static str },
}

impl fmt::Display for MarkParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarkParseError::Empty => write!(f, "empty mark"),
            MarkParseError::BadFeatureName { name, reason } => {
                write!(f, "feature name `{name}` is not a WIT label ({reason})")
            }
        }
    }
}

impl std::error::Error for MarkParseError {}

impl Mark {
    /// Parse from the textual form: `feature` or `!feature`.
    pub fn parse(s: &str) -> Result<Self, MarkParseError> {
        let (negated, name) = match s.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        if name.is_empty() {
            return Err(MarkParseError::Empty);
        }
        if let Some(reason) = is_wit_label(name) {
            return Err(MarkParseError::BadFeatureName {
                name: name.to_string(),
                reason,
            });
        }
        Ok(if negated {
            Mark::Declines(name.to_string())
        } else {
            Mark::Requires(name.to_string())
        })
    }

    pub fn feature(&self) -> &str {
        match self {
            Mark::Requires(f) | Mark::Declines(f) => f,
        }
    }

    pub fn is_negative(&self) -> bool {
        matches!(self, Mark::Declines(_))
    }
}

impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mark::Requires(name) => f.write_str(name),
            Mark::Declines(name) => write!(f, "!{name}"),
        }
    }
}

#[cfg(feature = "serde")]
mod serde_impl {
    use super::*;
    use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

    impl Serialize for Mark {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_str(self)
        }
    }

    impl<'de> Deserialize<'de> for Mark {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let s = String::deserialize(d)?;
            Mark::parse(&s).map_err(de::Error::custom)
        }
    }
}

/// Error constructing a mark set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarksError {
    /// The same feature appears with both polarities: such a case can
    /// never apply anywhere, which is always a bug (disabling a case
    /// belongs to ratchets, not mark tricks).
    Contradiction(String),
    /// The same mark appears twice.
    Duplicate(String),
}

impl fmt::Display for MarksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarksError::Contradiction(feature) => {
                write!(f, "contradictory marks: both `{feature}` and `!{feature}`")
            }
            MarksError::Duplicate(mark) => write!(f, "duplicate mark `{mark}`"),
        }
    }
}

impl std::error::Error for MarksError {}

/// A case's validated mark set: at most one mark per feature name (a
/// feature marked with both polarities is rejected — see
/// [`MarksError::Contradiction`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks(Vec<Mark>);

impl Marks {
    /// Validate and construct.
    pub fn new(marks: Vec<Mark>) -> Result<Self, MarksError> {
        for (i, mark) in marks.iter().enumerate() {
            for earlier in &marks[..i] {
                if earlier == mark {
                    return Err(MarksError::Duplicate(mark.to_string()));
                }
                if earlier.feature() == mark.feature() {
                    return Err(MarksError::Contradiction(mark.feature().to_string()));
                }
            }
        }
        Ok(Marks(marks))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Mark> {
        self.0.iter()
    }

    pub fn as_slice(&self) -> &[Mark] {
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
            Mark::Requires(f) => !missing(f),
            Mark::Declines(f) => missing(f),
        })
    }

    /// The mark that excludes this case for the given missing-set, if
    /// any (the `not-applicable` detail).
    pub fn excluding_mark<S: AsRef<str>>(&self, missing_features: &[S]) -> Option<&Mark> {
        let missing = |f: &str| missing_features.iter().any(|m| m.as_ref() == f);
        self.0.iter().find(|mark| match mark {
            Mark::Requires(f) => missing(f),
            Mark::Declines(f) => !missing(f),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for s in ["big-int", "!big-int", "hsm", "!aes-gcm-any-iv"] {
            assert_eq!(Mark::parse(s).unwrap().to_string(), s);
        }
        assert!(Mark::parse("").is_err());
        assert!(Mark::parse("!").is_err());
        assert!(Mark::parse("Big-Int").is_err());
        assert!(Mark::parse("big_int").is_err());
        assert!(Mark::parse("!!x").is_err());
    }

    #[test]
    fn construction_invariants() {
        let hsm = || Mark::parse("hsm").unwrap();
        let not_hsm = || Mark::parse("!hsm").unwrap();
        assert_eq!(
            Marks::new(vec![hsm(), not_hsm()]).unwrap_err(),
            MarksError::Contradiction("hsm".into())
        );
        assert_eq!(
            Marks::new(vec![not_hsm(), not_hsm()]).unwrap_err(),
            MarksError::Duplicate("!hsm".into())
        );
        Marks::new(vec![hsm(), Mark::parse("!sim").unwrap()]).unwrap();
    }

    #[test]
    fn applicability() {
        let unmarked = Marks::default();
        assert!(unmarked.applies::<&str>(&[]));
        assert!(unmarked.applies(&["anything"]));

        let requires = Marks::new(vec![Mark::parse("hsm").unwrap()]).unwrap();
        assert!(requires.applies::<&str>(&[]));
        assert!(!requires.applies(&["hsm"]));
        assert_eq!(
            requires.excluding_mark(&["hsm"]).unwrap().to_string(),
            "hsm"
        );

        let declines = Marks::new(vec![Mark::parse("!hsm").unwrap()]).unwrap();
        assert!(!declines.applies::<&str>(&[]));
        assert!(declines.applies(&["hsm"]));

        // conjunction: requires a AND b, declines c
        let multi = Marks::new(vec![
            Mark::parse("a").unwrap(),
            Mark::parse("b").unwrap(),
            Mark::parse("!c").unwrap(),
        ])
        .unwrap();
        assert!(multi.applies(&["c"]));
        assert!(!multi.applies(&["a", "c"]));
        assert!(!multi.applies::<&str>(&[]));
    }
}
