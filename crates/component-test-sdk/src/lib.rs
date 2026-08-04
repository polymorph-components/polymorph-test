//! Guest SDK for `lann:component-test` suites.
//!
//! **Start with [`suite`]** (the `#[component_test_sdk::suite]`
//! attribute): it owns names, tags, inventory, and all glue. The
//! [`Registry`]/[`case!`] layer underneath is plumbing for the macro
//! and for unusual producers; raw [`Registry::case`] registration
//! bypasses the static inventory and will trip the runner's drift
//! cross-check.
//!
//! This crate owns the contract bindings (see [`bindings`], generated
//! from the symlinked contract WIT): suite crates depend only on this
//! SDK and need neither a wit dir nor a wit-bindgen dependency of
//! their own. A suite that imports a system-under-test adds a separate
//! plain `wit_bindgen::generate!` for *that* surface only.

use std::future::Future;
use std::pin::Pin;

pub use component_test_core::{
    arcstr,
    name::{const_valid_name, const_valid_tag},
    normalize_segment, ArcStr, CaseName, Failure, NameError, Tag, Tags, Verdict,
};
pub use component_test_sdk_macro::suite;

/// The contract bindings, generated once here (async mode — there is
/// no reason for a Rust crate to use sync bindings; sync case bodies
/// work fine inside async bindings). Suite crates never see WIT.
#[allow(warnings)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "tests.wit",
        world: "suite",
        generate_all,
        pub_export_macro: true,
        default_bindings_module: "component_test_sdk::bindings",
    });
}

/// The runner-provided per-case context: a curated wrapper over the
/// raw contract binding. Unwrapped contract methods remain reachable
/// through `Deref`.
#[repr(transparent)]
pub struct TestContext(bindings::lann::component_test::test_context::Context);

impl TestContext {
    #[doc(hidden)]
    pub fn from_raw(raw: &bindings::lann::component_test::test_context::Context) -> &TestContext {
        // Sound: repr(transparent) single-field wrapper.
        unsafe {
            &*(raw as *const bindings::lann::component_test::test_context::Context
                as *const TestContext)
        }
    }

    /// Report a diagnostic message (the sideband; never affects the
    /// verdict). Runners surface these with the case's results;
    /// delivery may await the observer.
    pub async fn diag(&self, msg: impl Into<String>) {
        self.0.diagnostic(msg.into()).await
    }
}

impl std::ops::Deref for TestContext {
    type Target = bindings::lann::component_test::test_context::Context;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Convenience imports for suite crates. `Context` comes from the
/// `#[suite]`-generated bindings (re-exported at the suite module
/// root), not from here.
pub mod prelude {
    pub use crate::{
        check, check_eq, failed, gen_case, skipped, Failure, GeneratedCase as Case, OrFail,
        TestContext, Verdict,
    };
}

/// `?`-adjacent ergonomics for error types outside the blanket
/// `From<E: std::error::Error> for Failure` — most notably
/// `anyhow::Error`, which deliberately does not implement `Error` and
/// therefore cannot ride `?` into a [`Verdict`] directly. In a case
/// body: `let v = helper().or_fail()?;`.
pub trait OrFail<T> {
    /// Map the error into a one-line [`Failure::Failed`] (`{:#}`
    /// formatting, so error chains flatten onto the line).
    fn or_fail(self) -> Result<T, Failure>;
}

impl<T, E: std::fmt::Display> OrFail<T> for Result<T, E> {
    fn or_fail(self) -> Result<T, Failure> {
        self.map_err(|e| Failure::Failed(format!("{e:#}")))
    }
}

/// Boxed case body: borrows the context for the duration of the run.
pub type CaseFn =
    Box<dyn for<'a> Fn(&'a TestContext) -> Pin<Box<dyn Future<Output = Verdict> + 'a>>>;

/// One registered case (the registry's entry type, keyed by name;
/// generator-produced cases are [`GeneratedCase`], prelude-aliased to
/// `Case`).
pub struct RegisteredCase {
    pub tags: Tags,
    pub run: CaseFn,
}

/// A case produced by a `#[case_generator]`: a leaf name (one or more
/// segments, appended under the row's prefix) plus the body.
pub struct GeneratedCase {
    leaf: ArcStr,
    run: CaseFn,
}

impl GeneratedCase {
    pub fn new<F>(leaf: impl Into<ArcStr>, run: F) -> Self
    where
        F: for<'a> Fn(&'a TestContext) -> Pin<Box<dyn Future<Output = Verdict> + 'a>> + 'static,
    {
        GeneratedCase {
            leaf: leaf.into(),
            run: Box::new(run),
        }
    }
}

/// Ordered case registry with grammar + duplicate enforcement:
/// insertion-ordered, name-keyed (one structure, one name copy).
#[derive(Default)]
pub struct Registry {
    cases: indexmap::IndexMap<CaseName, RegisteredCase, foldhash::fast::RandomState>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a case. Panics (traps, in a suite) on grammar
    /// violations or duplicates — a misdeclared case is a harness bug.
    /// `body` returns a boxed future borrowing the context; use
    /// [`case!`](crate::case) to register plain `async fn`s.
    pub fn case<F>(&mut self, name: impl Into<ArcStr>, tags: &[&str], body: F) -> &mut Self
    where
        F: for<'a> Fn(&'a TestContext) -> Pin<Box<dyn Future<Output = Verdict> + 'a>> + 'static,
    {
        let name = name.into();
        let name = CaseName::new(name.clone())
            .unwrap_or_else(|e| panic!("invalid case name `{name}`: {e}"));
        let tags = Tags::new(
            tags.iter()
                .map(|m| Tag::parse(m).unwrap_or_else(|e| panic!("invalid tag `{m}`: {e}")))
                .collect(),
        )
        .unwrap_or_else(|e| panic!("invalid tags on `{name}`: {e}"));
        let entry = RegisteredCase {
            tags,
            run: Box::new(body),
        };
        if self.cases.insert(name.clone(), entry).is_some() {
            panic!("duplicate case name `{name}` (check post-normalization collisions)");
        }
        self
    }

    pub fn cases(&self) -> impl Iterator<Item = (&CaseName, &RegisteredCase)> {
        self.cases.iter()
    }

    pub fn len(&self) -> usize {
        self.cases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<(&CaseName, &RegisteredCase)> {
        self.cases.get_index(index)
    }

    /// Register a generated case under `prefix` with the row's `tags`.
    /// Panics on grammar violations or duplicates — harness bugs.
    pub fn generated(&mut self, prefix: &ArcStr, tags: &Tags, case: GeneratedCase) {
        // Zero string assembly: the name shares the row's prefix (a
        // refcount bump) and the case's leaf.
        let name = CaseName::prefixed(prefix.clone(), case.leaf)
            .unwrap_or_else(|e| panic!("invalid generated case name under `{prefix}`: {e}"));
        let entry = RegisteredCase {
            tags: tags.clone(),
            run: case.run,
        };
        if self.cases.insert(name.clone(), entry).is_some() {
            panic!("duplicate case name `{name}`");
        }
    }

    /// Register a generated case whose name was already split into
    /// canonical (prefix, leaf) form (see [`CaseName::from_parts`]) —
    /// the allocation-free fast path for large corpora: no name
    /// assembly, no `GeneratedCase` intermediary, a single boxed
    /// closure, tags shared by refcount. `row_prefix` is the generator
    /// row's static prefix; the name must sit under it (that inventory
    /// invariant is what the runner's drift cross-check relies on).
    /// Panics on violations — harness bugs.
    pub fn generated_named(&mut self, row_prefix: &str, tags: &Tags, name: CaseName, run: CaseFn) {
        // Deliberately looser than `core::name::is_under`: the case's
        // *prefix* may equal the row prefix exactly (a row leaf is one
        // or more segments appended below the row).
        let under_row = name
            .prefix()
            .is_some_and(|p| p == row_prefix || component_test_core::name::is_under(p, row_prefix));
        if !under_row {
            panic!("generated case `{name}` is not under its row prefix `{row_prefix}`");
        }
        let entry = RegisteredCase {
            tags: tags.clone(),
            run,
        };
        if self.cases.insert(name.clone(), entry).is_some() {
            panic!("duplicate case name `{name}`");
        }
    }
}

/// Register an `async fn(&Context) -> Verdict` as a case:
/// `case!(registry, "group/name", ["tag", "!other"], my_async_fn);`
///
/// Also emits the case's inventory record (name + tags) into the
/// `component-test:tags@0.1` custom section, enabling execution-free
/// inventory/lockfile generation. Requires literal name/tags.
#[macro_export]
macro_rules! case {
    ($registry:expr, $name:expr, [$($mark:expr),* $(,)?], $body:path) => {{
        const _: () = {
            // Compile-time validation: the record is emitted into the
            // inventory section whether or not registration ever runs,
            // so bad literals must fail the build (a bad name could
            // otherwise forge extra records via embedded separators).
            assert!($crate::const_valid_name($name), "invalid case name literal");
            $(assert!($crate::const_valid_tag($mark), "invalid tag literal");)*
            const RECORD: &str = concat!($name $(, " ", $mark)*, "\n");
            // The one copy of the section name that cannot reference
            // `component_test_core::name::TAGS_SECTION` (macro_rules
            // attribute values must be literal). Keep in sync.
            #[link_section = "component-test:tags@0.1"]
            #[used]
            static TAGS_RECORD: [u8; RECORD.len()] =
                $crate::str_to_array::<{ RECORD.len() }>(RECORD);
        };
        $registry.case(
            $crate::arcstr::literal!($name),
            &[$($mark),*],
            move |ctx| Box::pin($body(ctx)),
        )
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

/// Sugar for [`GeneratedCase`] bodies inside `#[case_generator]` fns:
/// `gen_case!(format!("tc{n}"), |ctx| async move { ... })`. Removes the
/// `Box::pin` and the `&TestContext` annotation (the `TestContext` name
/// is resolved at the call site, where `#[suite]` injects it).
#[macro_export]
macro_rules! gen_case {
    ($name:expr, |$ctx:ident| $body:expr) => {
        $crate::GeneratedCase::new($name, move |$ctx: &TestContext| {
            ::std::boxed::Box::pin($body)
        })
    };
}

/// Verdict ergonomics.
pub fn failed(detail: impl Into<String>) -> Verdict {
    Err(Failure::Failed(detail.into()))
}

/// Runtime skip — **exceptional, not the normal conditional-skip
/// tool**: gating knowable before the run belongs in feature tags
/// (`#[case(tags("hsm"))]`), which skip without executing. Return
/// `skipped` only when a run-stable target fact turns out not to hold
/// at run time (e.g. a declared hardware token is unavailable), and
/// say in the claim what the case asserted instead.
pub fn skipped(claim: impl Into<String>) -> Verdict {
    Err(Failure::Skipped(claim.into()))
}

/// Assert equality, failing the case with a one-line detail.
/// Argument order is `check_eq!(actual, expected)`: the message reads
/// "expected {expected}, got {actual}". The optional context message
/// may be any `Display` expression.
#[macro_export]
macro_rules! check_eq {
    ($actual:expr, $expected:expr) => {{
        let (a, e) = (&$actual, &$expected);
        if a != e {
            return $crate::failed(format!("expected {e:?}, got {a:?}"));
        }
    }};
    ($actual:expr, $expected:expr, $ctxmsg:expr) => {{
        let (a, e) = (&$actual, &$expected);
        if a != e {
            return $crate::failed(format!("{}: expected {e:?}, got {a:?}", $ctxmsg));
        }
    }};
}

/// Assert a condition, failing the case with the given one-line detail
/// (or, with no message, the stringified condition — mirroring
/// `assert!`). Expands to an early `return`, so it may only be used
/// directly inside a function returning [`Verdict`] (not in helpers
/// with other return types).
#[macro_export]
macro_rules! check {
    ($cond:expr $(,)?) => {{
        if !$cond {
            return $crate::failed(concat!("check failed: ", stringify!($cond)));
        }
    }};
    ($cond:expr, $($detail:tt)+) => {{
        if !$cond {
            return $crate::failed(format!($($detail)+));
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_registers_and_orders() {
        let mut reg: Registry = Registry::new();
        async fn ok(_: &crate::TestContext) -> Verdict {
            Ok(())
        }
        crate::case!(reg, "a/x", [], ok);
        crate::case!(reg, "a/y", ["hsm"], ok);
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.get(0).unwrap().0.as_str(), "a/x");
        assert_eq!(reg.get(1).unwrap().1.tags.as_slice().len(), 1);
    }

    #[test]
    #[should_panic(expected = "duplicate case name")]
    fn registry_rejects_duplicates() {
        let mut reg: Registry = Registry::new();
        async fn ok(_: &crate::TestContext) -> Verdict {
            Ok(())
        }
        crate::case!(reg, "a/x", [], ok);
        crate::case!(reg, "a/x", [], ok);
    }

    #[test]
    fn generated_named_registers_under_row() {
        let mut reg: Registry = Registry::new();
        let full = ArcStr::from("row/sub/tc1/whole");
        let name = CaseName::from_parts(full.substr(..11), full.substr(12..)).unwrap();
        let tags = Tags::default();
        reg.generated_named(
            "row/sub",
            &tags,
            name,
            Box::new(|_| Box::pin(async { Ok(()) })),
        );
        assert_eq!(reg.get(0).unwrap().0.as_str(), "row/sub/tc1/whole");
    }

    #[test]
    #[should_panic(expected = "not under its row prefix")]
    fn generated_named_rejects_foreign_names() {
        let mut reg: Registry = Registry::new();
        let name = CaseName::parse("other/tc1").unwrap();
        reg.generated_named(
            "row",
            &Tags::default(),
            name,
            Box::new(|_| Box::pin(async { Ok(()) })),
        );
    }

    // Bad literal names in `case!` are now a compile error (const
    // assert), superseding the old runtime-panic test; the runtime
    // path still guards non-macro registration:
    #[test]
    #[should_panic(expected = "invalid case name")]
    fn registry_rejects_bad_names_at_runtime() {
        let mut reg: Registry = Registry::new();
        async fn ok(_: &crate::TestContext) -> Verdict {
            Ok(())
        }
        reg.case("Bad/Name", &[], move |ctx| Box::pin(ok(ctx)));
    }

    #[test]
    fn or_fail_maps_display_errors() {
        // std error types ride the blanket `From<E: Error>` via `?`;
        // or_fail covers Display-only errors — anyhow above all.
        fn anyhow_body() -> Verdict {
            let e = anyhow::anyhow!("root").context("outer");
            Err::<(), _>(e).or_fail()?;
            Ok(())
        }
        match anyhow_body() {
            Err(Failure::Failed(d)) => assert_eq!(d, "outer: root"), // {:#} flattens the chain
            other => panic!("expected failed, got {other:?}"),
        }

        fn ok_body() -> Verdict {
            let n: i32 = "42".parse().or_fail()?;
            check!(n == 42);
            Ok(())
        }
        assert_eq!(ok_body(), Ok(()));
    }

    #[test]
    fn bare_check_uses_stringified_condition() {
        fn body(n: u32) -> Verdict {
            check!(n > 3);
            Ok(())
        }
        assert_eq!(body(4), Ok(()));
        match body(1) {
            Err(Failure::Failed(d)) => assert_eq!(d, "check failed: n > 3"),
            other => panic!("expected failed, got {other:?}"),
        }
    }
}
