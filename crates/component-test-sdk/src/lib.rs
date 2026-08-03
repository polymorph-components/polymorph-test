//! Guest SDK core for `lann:component-test` suites.
//!
//! Deliberately independent of wit-bindgen: the suite crate owns its
//! generated bindings (and their `Context` type); this crate provides
//! the registry, name validation, tags, and verdict ergonomics that
//! the thin generated-glue delegates to. (Macro sugar that hides the
//! glue entirely is tracked for later in M1.1.)

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

pub use component_test_core::{
    normalize_segment, CaseName, Failure, NameError, Tag, Tags, Verdict,
};

/// Boxed case body: borrows the (bindings-generated) context for the
/// duration of the run.
pub type CaseFn<Ctx> = Box<dyn for<'a> Fn(&'a Ctx) -> Pin<Box<dyn Future<Output = Verdict> + 'a>>>;

/// One registered case.
pub struct Case<Ctx> {
    pub name: CaseName,
    pub tags: Tags,
    pub run: CaseFn<Ctx>,
}

/// Ordered case registry with grammar + duplicate enforcement.
pub struct Registry<Ctx> {
    cases: Vec<Case<Ctx>>,
    names: BTreeSet<String>,
}

impl<Ctx> Default for Registry<Ctx> {
    fn default() -> Self {
        Registry {
            cases: Vec::new(),
            names: BTreeSet::new(),
        }
    }
}

impl<Ctx> Registry<Ctx> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a case. Panics (traps, in a suite) on grammar
    /// violations or duplicates — a misdeclared case is a harness bug.
    /// `body` returns a boxed future borrowing the context; use
    /// [`case!`](crate::case) to register plain `async fn`s.
    pub fn case<F>(&mut self, name: &str, tags: &[&str], body: F) -> &mut Self
    where
        F: for<'a> Fn(&'a Ctx) -> Pin<Box<dyn Future<Output = Verdict> + 'a>> + 'static,
    {
        let name =
            CaseName::parse(name).unwrap_or_else(|e| panic!("invalid case name `{name}`: {e}"));
        if !self.names.insert(name.as_str().to_string()) {
            panic!("duplicate case name `{name}` (check post-normalization collisions)");
        }
        let tags = Tags::new(
            tags.iter()
                .map(|m| Tag::parse(m).unwrap_or_else(|e| panic!("invalid mark `{m}`: {e}")))
                .collect(),
        )
        .unwrap_or_else(|e| panic!("invalid tags on `{name}`: {e}"));
        self.cases.push(Case {
            name,
            tags,
            run: Box::new(body),
        });
        self
    }

    pub fn cases(&self) -> &[Case<Ctx>] {
        &self.cases
    }

    pub fn len(&self) -> usize {
        self.cases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Case<Ctx>> {
        self.cases.get(index)
    }
}

/// Register an `async fn(&Context) -> Verdict` as a case:
/// `case!(registry, "group/name", ["mark", "!other"], my_async_fn);`
///
/// Also emits the case's inventory record (name + tags) into the
/// `component-test:tags@0.1` custom section, enabling execution-free
/// inventory/lockfile generation. Requires literal name/tags.
#[macro_export]
macro_rules! case {
    ($registry:expr, $name:expr, [$($mark:expr),* $(,)?], $body:path) => {{
        const _: () = {
            const RECORD: &str = concat!($name $(, " ", $mark)*, "\n");
            #[link_section = "component-test:tags@0.1"]
            #[used]
            static TAGS_RECORD: [u8; RECORD.len()] =
                $crate::str_to_array::<{ RECORD.len() }>(RECORD);
        };
        $registry.case($name, &[$($mark),*], move |ctx| Box::pin($body(ctx)))
    }};
}

/// Const helper for [`case!`]'s section emission.
#[doc(hidden)]
pub const fn str_to_array<const N: usize>(s: &str) -> [u8; N] {
    let b = s.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = b[i];
        i += 1;
    }
    out
}

/// Verdict ergonomics.
pub fn failed(detail: impl Into<String>) -> Verdict {
    Err(Failure::Failed(detail.into()))
}

pub fn skipped(claim: impl Into<String>) -> Verdict {
    Err(Failure::Skipped(claim.into()))
}

/// Assert equality, failing the case with a one-line detail.
#[macro_export]
macro_rules! check_eq {
    ($left:expr, $right:expr $(, $ctxmsg:expr)?) => {{
        let (l, r) = (&$left, &$right);
        if l != r {
            return $crate::failed(format!(
                concat!($($ctxmsg, ": ",)? "expected {:?}, got {:?}"),
                r, l
            ));
        }
    }};
}

/// Assert a condition, failing the case with the given one-line detail.
#[macro_export]
macro_rules! check {
    ($cond:expr, $($detail:tt)+) => {{
        if !$cond {
            return $crate::failed(format!($($detail)+));
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCtx;

    #[test]
    fn registry_registers_and_orders() {
        let mut reg: Registry<FakeCtx> = Registry::new();
        async fn ok(_: &FakeCtx) -> Verdict {
            Ok(())
        }
        crate::case!(reg, "a/x", [], ok);
        crate::case!(reg, "a/y", ["hsm"], ok);
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.get(0).unwrap().name.as_str(), "a/x");
        assert_eq!(reg.get(1).unwrap().tags.as_slice().len(), 1);
    }

    #[test]
    #[should_panic(expected = "duplicate case name")]
    fn registry_rejects_duplicates() {
        let mut reg: Registry<FakeCtx> = Registry::new();
        async fn ok(_: &FakeCtx) -> Verdict {
            Ok(())
        }
        crate::case!(reg, "a/x", [], ok);
        crate::case!(reg, "a/x", [], ok);
    }

    #[test]
    #[should_panic(expected = "invalid case name")]
    fn registry_rejects_bad_names() {
        let mut reg: Registry<FakeCtx> = Registry::new();
        async fn ok(_: &FakeCtx) -> Verdict {
            Ok(())
        }
        crate::case!(reg, "Bad/Name", [], ok);
    }
}
