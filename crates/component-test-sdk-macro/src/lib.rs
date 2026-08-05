//! `#[component_test_sdk::suite]`: the module-attribute suite macro.
//!
//! Walks the annotated module tree collecting `#[case]` functions and
//! `#[case_generator]` rows; derives case names from the module path
//! (idents mapped `_` → `-`); resolves tag inheritance
//! (nearest-declaration-per-feature wins); validates names/tags and the
//! decline-pair lint at expansion; and emits the registry, the glue
//! impls, the `export!`, and the `component-test:tags@0.1` inventory
//! records. The contract bindings themselves live in
//! `component_test_sdk::bindings` (generated there from the symlinked
//! WIT); this macro only emits references to them, so suite crates
//! never vendor WIT.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, spanned::Spanned, Error, Expr, ExprLit, Ident, Item, ItemFn, ItemMod, Lit,
    Meta, Result,
};

/// Declare a test-suite component from a module tree.
///
/// ```
/// #[component_test_sdk::suite]          // or #[suite(name = "acme-crypto")]
/// mod sample {
///     mod math {
///         #[case]
///         async fn add(ctx: &TestContext) -> Verdict {   // "sample/math/add"
///             ctx.diag("computing").await;               // diagnostics sideband
///             check_eq!(2 + 2, 4, "2 + 2");              // (actual, expected)
///             Ok(())
///         }
///
///         #[case]                       // sync and ctx-less fns also work
///         fn quick() -> Verdict { Ok(()) }
///     }
///
///     #[cases(tags("hsm"))]             // tags inherit down the subtree
///     mod hsm {
///         #[case]
///         async fn attest(ctx: &TestContext) -> Verdict { Ok(()) }
///
///         #[case(tags("!hsm"))]         // polarity flip overrides inherited "hsm"
///         async fn declined(ctx: &TestContext) -> Verdict { Ok(()) }
///     }
///
///     #[case_generator(prefix = "gen")] // runtime leaves under a static prefix
///     fn cases() -> impl Iterator<Item = Case> {
///         (1..=2).map(|n| gen_case!(format!("tc{n}"), |ctx| async move {
///             ctx.diag(format!("case {n}")).await;
///             Ok(())
///         }))
///     }
/// }
/// # fn main() {}
/// ```
///
/// Semantics:
/// - **Names** derive from the module path + fn name (idents mapped
///   `_` → `-`; override a leaf with `#[case(name = "0x1a2b")]`;
///   rename the root with `#[suite(name = "...")]`, or drop the root
///   segment entirely with `#[suite(name = "")]` — an *unrooted* suite,
///   for porting corpora whose established case ids share no common
///   root). Non-leaf segments must be kebab-case; validated at compile
///   time.
/// - **Tags** (`"feature"` / `"!feature"`) gate cases against target
///   capability manifests. `#[cases(tags(...))]` on a module applies to
///   its subtree; per-feature, the nearest declaration wins whole.
///   Every positively-tagged feature needs a `!feature` decline case
///   (compile-time lint).
/// - **Inventory**: every case (and each generator's `prefix/*`) is
///   recorded in a custom section for execution-free lockfile
///   generation; `#[cfg]` on cases is therefore rejected.
/// - **`#[case_row(prefix = "...")]`** is `#[case_generator]`'s
///   direct-registration sibling for very large corpora: the fn has
///   signature `fn(&mut Registry, prefix: &ArcStr, tags: &Tags)` and
///   registers its cases itself (typically via
///   `Registry::generated_named` with pre-split names). Identical
///   inventory record and prefix validation; the fn is responsible for
///   keeping every registered name under `prefix` (enforced at
///   registration).
/// - **Shared setup/fixtures**: plain items (statics, helpers, `use`)
///   inside the module are untouched — use `std::sync::LazyLock` for
///   expensive shared tables.
/// - The SDK prelude and `TestContext` are auto-imported in every
///   module of the tree; suite files need no `use` lines.
/// - The contract bindings come from `component_test_sdk::bindings`:
///   the suite crate needs no `wit/` directory (SUT imports go in a
///   separate plain `wit_bindgen::generate!`).
#[proc_macro_attribute]
pub fn suite(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut module = parse_macro_input!(item as ItemMod);
    let args = parse_macro_input!(attr as SuiteArgs);
    match expand(&mut module, args) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ------------------------------------------------------------ arguments

#[derive(Default)]
struct SuiteArgs {
    name: Option<String>,
}

impl syn::parse::Parse for SuiteArgs {
    fn parse(input: syn::parse::ParseStream) -> Result<Self> {
        let mut args = SuiteArgs::default();
        while !input.is_empty() {
            let meta: Meta = input.parse()?;
            match &meta {
                Meta::NameValue(nv) if nv.path.is_ident("name") => {
                    args.name = Some(lit_str(&nv.value)?);
                }
                other => return Err(Error::new(other.span(), "expected `name = \"...\"`")),
            }
            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(args)
    }
}

fn lit_str(expr: &Expr) -> Result<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.value()),
        other => Err(Error::new(other.span(), "expected string literal")),
    }
}

// ----------------------------------------------------------- collection

struct CaseDef {
    name: String,
    tags: Vec<String>,
    /// Path segments (module idents) from the suite root to the fn.
    fn_path: Vec<Ident>,
    is_async: bool,
    has_ctx: bool,
    span: proc_macro2::Span,
}

struct GenDef {
    prefix: String,
    tags: Vec<String>,
    fn_path: Vec<Ident>,
    span: proc_macro2::Span,
    /// `#[case_row]` rather than `#[case_generator]`: the fn registers
    /// its row directly (`fn(&mut Registry, &ArcStr, &Tags)`) instead
    /// of yielding `Case`s. Same inventory record, same validation.
    is_row: bool,
}

/// A tag declaration level: feature name -> full tag string.
type TagScope = Vec<(String, String)>;

fn expand(module: &mut ItemMod, args: SuiteArgs) -> Result<TokenStream2> {
    let root_ident = module.ident.clone();
    let root_name = args
        .name
        .unwrap_or_else(|| ident_to_segment(&root_ident.to_string()));

    let Some((_, items)) = module.content.as_mut() else {
        return Err(Error::new(
            module.span(),
            "#[suite] requires an inline module body",
        ));
    };

    let mut cases = Vec::new();
    let mut gens = Vec::new();
    walk_items(
        items,
        &root_name,
        &mut Vec::new(),
        &Vec::new(),
        &mut cases,
        &mut gens,
    )?;

    validate(&cases, &gens)?;

    // ---- registry construction, in source order
    let registrations = cases.iter().map(|c| {
        let name = &c.name;
        let tags = &c.tags;
        let path = path_tokens(&c.fn_path);
        let span = c.span;
        let body = match (c.is_async, c.has_ctx) {
            (true, true) => quote!(::std::boxed::Box::pin(#path(ctx))),
            (true, false) => quote!(::std::boxed::Box::pin(async move {
                let _ = ctx;
                #path().await
            })),
            (false, true) => quote!(::std::boxed::Box::pin(async move { #path(ctx) })),
            (false, false) => quote!(::std::boxed::Box::pin(async move {
                let _ = ctx;
                #path()
            })),
        };
        quote::quote_spanned! {span=>
            registry.case(
                ::component_test_sdk::arcstr::literal!(#name),
                &[#(#tags),*],
                move |ctx| #body,
            );
        }
    });
    let gen_registrations = gens.iter().map(|g| {
        let prefix = &g.prefix;
        let tags = &g.tags;
        let path = path_tokens(&g.fn_path);
        let invoke = if g.is_row {
            // The row fn registers directly (pre-split names, shared
            // tags — the corpus fast path).
            quote! {
                #path(&mut registry, &row_prefix, &row_tags);
            }
        } else {
            quote! {
                for generated in #path() {
                    registry.generated(&row_prefix, &row_tags, generated);
                }
            }
        };
        quote! {
            {
                // Row tags parsed once; per-case attachment is
                // refcount bumps.
                let row_tags = ::component_test_sdk::Tags::parse_all::<&str>(&[#(#tags),*])
                    .expect("row tags validated at expansion");
                let row_prefix = ::component_test_sdk::arcstr::literal!(#prefix);
                #invoke
            }
        }
    });

    // ---- inventory section records (exact + prefix)
    let records = cases
        .iter()
        .map(|c| record_line(&c.name, &c.tags))
        .chain(
            gens.iter()
                .map(|g| record_line(&format!("{}/*", g.prefix), &g.tags)),
        )
        .map(|line| {
            // Canonical section name from core (interpolated as a
            // literal — attribute values can't reference consts).
            let section = component_test_core::name::TAGS_SECTION;
            quote! {
                const _: () = {
                    const RECORD: &str = #line;
                    #[link_section = #section]
                    #[used]
                    static TAGS_RECORD: [u8; RECORD.len()] =
                        ::component_test_sdk::str_to_array::<{ RECORD.len() }>(RECORD);
                };
            }
        });

    let glue = quote! {
        use ::component_test_sdk::bindings as __ct_bindings;
        #[allow(unused_imports)]
        use ::component_test_sdk::prelude::*;

        #(#records)*

        fn __ct_registry() -> ::component_test_sdk::Registry {
            let mut registry = ::component_test_sdk::Registry::new();
            #(#registrations)*
            #(#gen_registrations)*
            registry
        }

        ::std::thread_local! {
            static __CT_REGISTRY: ::std::cell::OnceCell<::component_test_sdk::Registry> =
                const { ::std::cell::OnceCell::new() };
        }

        fn __ct_with_registry<R>(
            f: impl FnOnce(&::component_test_sdk::Registry) -> R,
        ) -> R {
            __CT_REGISTRY.with(|cell| f(cell.get_or_init(__ct_registry)))
        }

        struct __CtCase {
            index: usize,
        }

        impl __ct_bindings::exports::polymorph::test::tests::GuestTestCase for __CtCase {
            fn name(&self) -> String {
                __ct_with_registry(|reg| reg.get(self.index).unwrap().0.to_string())
            }

            async fn run(
                &self,
                ctx: &__ct_bindings::polymorph::test::test_context::Context,
            ) -> Result<(), __ct_bindings::exports::polymorph::test::tests::Outcome> {
                use __ct_bindings::exports::polymorph::test::tests::Outcome;
                let ctx = TestContext::from_raw(ctx);
                let verdict = __ct_with_registry(|reg| (reg.get(self.index).unwrap().1.run)(ctx));
                verdict.await.map_err(|failure| match failure {
                    ::component_test_sdk::Failure::Failed(d) => Outcome::Failed(d),
                    ::component_test_sdk::Failure::Skipped(d) => Outcome::Skipped(d),
                })
            }
        }

        struct __CtSuite;

        impl __ct_bindings::exports::polymorph::test::tests::Guest for __CtSuite {
            type TestCase = __CtCase;

            async fn all() -> Vec<__ct_bindings::exports::polymorph::test::tests::TestCase> {
                use __ct_bindings::exports::polymorph::test::tests::TestCase;
                __ct_with_registry(|reg| {
                    (0..reg.len())
                        .map(|index| TestCase::new(__CtCase { index }))
                        .collect()
                })
            }
        }

        ::component_test_sdk::bindings::export!(__CtSuite with_types_in ::component_test_sdk::bindings);
    };

    items.push(Item::Verbatim(glue));

    Ok(quote!(#module))
}

fn record_line(name: &str, tags: &[String]) -> String {
    let mut line = name.to_string();
    for tag in tags {
        line.push(' ');
        line.push_str(tag);
    }
    line.push('\n');
    line
}

fn path_tokens(path: &[Ident]) -> TokenStream2 {
    quote!(#(#path)::*)
}

/// Join a name prefix and a segment; an empty prefix (an unrooted
/// suite's top level) contributes nothing.
fn join_name(prefix: &str, seg: &str) -> String {
    if prefix.is_empty() {
        seg.to_string()
    } else {
        format!("{prefix}/{seg}")
    }
}

// --------------------------------------------------------------- walking

fn walk_items(
    items: &mut [Item],
    name_prefix: &str,
    mod_path: &mut Vec<Ident>,
    inherited: &TagScope,
    cases: &mut Vec<CaseDef>,
    gens: &mut Vec<GenDef>,
) -> Result<()> {
    for item in items.iter_mut() {
        match item {
            Item::Mod(m) => {
                reject_cfg(&m.attrs, m.span())?;
                // The generated registry lives at the suite root and
                // references items through this module.
                m.vis = syn::Visibility::Public(Default::default());
                let mut scope = inherited.clone();
                if let Some(tags) = take_attr_tags(&mut m.attrs, "cases")? {
                    merge_scope(&mut scope, tags, m.span())?;
                }
                let seg = ident_to_segment(&m.ident.to_string());
                // Non-leaf segments must be WIT labels; check eagerly
                // at the module ident so the error points at the
                // offending module, not at whichever case fn happens
                // to inherit the bad prefix (validate() re-checks the
                // whole joined name as a backstop).
                if let Some(reason) = component_test_core::name::is_wit_label(&seg) {
                    return Err(Error::new(
                        m.ident.span(),
                        format!(
                            "module `{}` maps to non-leaf segment `{seg}`, which is not a \
                             WIT label ({reason})",
                            m.ident
                        ),
                    ));
                }
                let child_prefix = join_name(name_prefix, &seg);
                mod_path.push(m.ident.clone());
                if let Some((_, children)) = m.content.as_mut() {
                    // Submodules see the suite-level names and the SDK
                    // prelude without their own imports (glob imports
                    // yield to any explicit user items).
                    children.insert(
                        0,
                        Item::Verbatim(quote! {
                            #[allow(unused_imports)]
                            use ::component_test_sdk::prelude::*;
                        }),
                    );
                    walk_items(children, &child_prefix, mod_path, &scope, cases, gens)?;
                } else {
                    return Err(Error::new(
                        m.span(),
                        "#[suite] modules must have inline bodies",
                    ));
                }
                mod_path.pop();
            }
            Item::Fn(f) => {
                // S1: a foreign test attribute inside a #[suite] module
                // is almost certainly a missed rename during porting —
                // and would otherwise be a SILENTLY dropped case.
                for attr in &f.attrs {
                    let last = attr
                        .path()
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_default();
                    if matches!(last.as_str(), "test" | "rstest" | "test_case" | "bench") {
                        return Err(Error::new(
                            attr.span(),
                            format!(
                                "`#[{last}]` inside a #[suite] module registers nothing \
                                 (the case would be silently dropped): did you mean #[case]?"
                            ),
                        ));
                    }
                }
                if f.attrs.iter().any(|a| {
                    a.path().is_ident("case")
                        || a.path().is_ident("case_generator")
                        || a.path().is_ident("case_row")
                }) {
                    reject_cfg(&f.attrs, f.span())?;
                    if !f.attrs.iter().any(|a| a.path().is_ident("case_row")) {
                        check_case_fn_shape(f)?;
                    }
                    // The generated registry lives at the suite-module
                    // root; collected fns must be reachable from there.
                    f.vis = syn::Visibility::Public(Default::default());
                }
                if let Some(case_attr) = take_case_attr(f)? {
                    let mut scope = inherited.clone();
                    merge_scope(&mut scope, case_attr.tags, f.span())?;
                    let leaf = case_attr
                        .name
                        .unwrap_or_else(|| ident_to_segment(&f.sig.ident.to_string()));
                    let mut fn_path = mod_path.clone();
                    fn_path.push(f.sig.ident.clone());
                    cases.push(CaseDef {
                        name: join_name(name_prefix, &leaf),
                        tags: scope.iter().map(|(_, t)| t.clone()).collect(),
                        fn_path,
                        is_async: f.sig.asyncness.is_some(),
                        has_ctx: !f.sig.inputs.is_empty(),
                        span: f.sig.ident.span(),
                    });
                } else if let Some((gen_attr, is_row)) = take_generator_attr(f)? {
                    let mut scope = inherited.clone();
                    merge_scope(&mut scope, gen_attr.tags, f.span())?;
                    let prefix = match gen_attr.prefix {
                        Some(p) => join_name(name_prefix, &p),
                        None => {
                            return Err(Error::new(
                                f.span(),
                                "#[case_generator]/#[case_row] requires prefix = \"...\"",
                            ))
                        }
                    };
                    let mut fn_path = mod_path.clone();
                    fn_path.push(f.sig.ident.clone());
                    gens.push(GenDef {
                        prefix,
                        tags: scope.iter().map(|(_, t)| t.clone()).collect(),
                        fn_path,
                        span: f.sig.ident.span(),
                        is_row,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

struct CaseAttr {
    name: Option<String>,
    tags: Vec<String>,
}

struct GenAttr {
    prefix: Option<String>,
    tags: Vec<String>,
}

fn take_case_attr(f: &mut ItemFn) -> Result<Option<CaseAttr>> {
    let Some(pos) = f.attrs.iter().position(|a| a.path().is_ident("case")) else {
        return Ok(None);
    };
    let attr = f.attrs.remove(pos);
    let mut out = CaseAttr {
        name: None,
        tags: Vec::new(),
    };
    if matches!(attr.meta, Meta::Path(_)) {
        return Ok(Some(out));
    }
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("name") {
            out.name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            Ok(())
        } else if meta.path.is_ident("tags") {
            parse_tags_list(&meta, &mut out.tags)
        } else {
            Err(meta.error("expected `name = \"...\"` or `tags(\"...\")`"))
        }
    })?;
    Ok(Some(out))
}

fn take_generator_attr(f: &mut ItemFn) -> Result<Option<(GenAttr, bool)>> {
    let Some((pos, is_row)) = f.attrs.iter().enumerate().find_map(|(i, a)| {
        if a.path().is_ident("case_generator") {
            Some((i, false))
        } else if a.path().is_ident("case_row") {
            Some((i, true))
        } else {
            None
        }
    }) else {
        return Ok(None);
    };
    let attr = f.attrs.remove(pos);
    let mut out = GenAttr {
        prefix: None,
        tags: Vec::new(),
    };
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("prefix") {
            out.prefix = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            Ok(())
        } else if meta.path.is_ident("tags") {
            parse_tags_list(&meta, &mut out.tags)
        } else {
            Err(meta.error("expected `prefix = \"...\"` or `tags(\"...\")`"))
        }
    })?;
    Ok(Some((out, is_row)))
}

/// `#[cases(tags("a", "!b"))]` on a module.
fn take_attr_tags(attrs: &mut Vec<syn::Attribute>, ident: &str) -> Result<Option<Vec<String>>> {
    let Some(pos) = attrs.iter().position(|a| a.path().is_ident(ident)) else {
        return Ok(None);
    };
    let attr = attrs.remove(pos);
    let mut tags = Vec::new();
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("tags") {
            parse_tags_list(&meta, &mut tags)
        } else {
            Err(meta.error("expected `tags(\"...\")`"))
        }
    })?;
    Ok(Some(tags))
}

fn parse_tags_list(meta: &syn::meta::ParseNestedMeta<'_>, out: &mut Vec<String>) -> Result<()> {
    let content;
    syn::parenthesized!(content in meta.input);
    let list = content.parse_terminated(|p| p.parse::<syn::LitStr>(), syn::Token![,])?;
    out.extend(list.iter().map(|l| l.value()));
    Ok(())
}

// ------------------------------------------------------------ semantics

/// Merge a declaration level into a scope: nearest declaration per
/// feature wins whole; same-level contradiction/duplicate rejected via
/// the data model.
fn merge_scope(scope: &mut TagScope, level: Vec<String>, span: proc_macro2::Span) -> Result<()> {
    use component_test_core::{Tag, Tags};
    let parsed: Vec<Tag> = level
        .iter()
        .map(|t| Tag::parse(t).map_err(|e| Error::new(span, format!("invalid tag `{t}`: {e}"))))
        .collect::<Result<_>>()?;
    Tags::new(parsed.clone()).map_err(|e| Error::new(span, e.to_string()))?;
    for tag in parsed {
        scope.retain(|(feature, _)| feature != tag.feature());
        scope.push((tag.feature().to_string(), tag.to_string()));
    }
    Ok(())
}

fn validate(cases: &[CaseDef], gens: &[GenDef]) -> Result<()> {
    use component_test_core::{name::is_wit_label, CaseName};
    use std::collections::BTreeSet;

    // An empty suite emits no tags section and reports zero cases as
    // success in every runner ("empty selection is a run error" — the
    // normative fold rule); refuse at compile time instead.
    if cases.is_empty() && gens.is_empty() {
        return Err(Error::new(
            proc_macro2::Span::call_site(),
            "suite has no cases: annotate at least one fn with #[case] \
             (or add a #[case_generator]) — an empty suite would report \
             vacuous success",
        ));
    }

    let mut seen = BTreeSet::new();
    // Tag sets with their declaration spans, parsed through the core
    // model (already validated at merge time; the map_err is a
    // belt-and-braces path). Shared with core's decline-pair helper so
    // the rule cannot drift from the lockfile/runner enforcement.
    let mut tag_sets: Vec<(proc_macro2::Span, Vec<component_test_core::Tag>)> = Vec::new();
    let parse_tags = |tags: &[String], span: proc_macro2::Span| {
        tags.iter()
            .map(|t| {
                component_test_core::Tag::parse(t)
                    .map_err(|e| Error::new(span, format!("invalid tag `{t}`: {e}")))
            })
            .collect::<Result<Vec<_>>>()
    };
    for c in cases {
        CaseName::parse(&c.name).map_err(|e| Error::new(c.span, format!("`{}`: {e}", c.name)))?;
        if !seen.insert(c.name.clone()) {
            return Err(Error::new(c.span, format!("duplicate case `{}`", c.name)));
        }
        tag_sets.push((c.span, parse_tags(&c.tags, c.span)?));
    }
    for g in gens {
        for seg in g.prefix.split('/') {
            if let Some(reason) = is_wit_label(seg) {
                return Err(Error::new(
                    g.span,
                    format!("generator prefix segment `{seg}` is not a WIT label ({reason})"),
                ));
            }
        }
        if !seen.insert(format!("{}/*", g.prefix)) {
            return Err(Error::new(
                g.span,
                format!("duplicate generator prefix `{}`", g.prefix),
            ));
        }
        tag_sets.push((g.span, parse_tags(&g.tags, g.span)?));
    }
    for c in cases {
        for g in gens {
            if component_test_core::name::is_under(&c.name, &g.prefix) {
                return Err(Error::new(
                    c.span,
                    format!(
                        "case `{}` sits under generator prefix `{}/*`: leaves would collide \
                         only at run time",
                        c.name, g.prefix
                    ),
                ));
            }
        }
    }
    for (i, a) in gens.iter().enumerate() {
        for b in gens.iter().skip(i + 1) {
            let (short, long) = if a.prefix.len() <= b.prefix.len() {
                (a, b)
            } else {
                (b, a)
            };
            if component_test_core::name::is_under(&long.prefix, &short.prefix) {
                return Err(Error::new(
                    long.span,
                    format!(
                        "generator prefixes `{}/*` and `{}/*` overlap: leaves would collide \
                         only at run time",
                        short.prefix, long.prefix
                    ),
                ));
            }
        }
    }
    let unpaired = component_test_core::tags::unpaired_positive_features(
        tag_sets.iter().map(|(_, tags)| tags.as_slice()),
    );
    if !unpaired.is_empty() {
        // Anchor at the first case/generator carrying an unpaired
        // positive tag — in a large suite, call_site (the attribute)
        // says nothing about *where* the missing decline is owed.
        let anchor = tag_sets
            .iter()
            .find(|(_, tags)| {
                tags.iter()
                    .any(|t| !t.is_negative() && unpaired.iter().any(|f| f == t.feature()))
            })
            .map(|(span, _)| *span)
            .unwrap_or_else(proc_macro2::Span::call_site);
        return Err(Error::new(
            anchor,
            format!(
                "decline-pair lint: feature(s) {} have positively-tagged cases but no \
                 `!feature` decline-asserting case. Add a case tagged with the negated \
                 feature (e.g. tags(\"!hsm\")) asserting the feature is cleanly refused \
                 on targets that lack it — see README \"Feature tags\"",
                unpaired
                    .iter()
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    Ok(())
}

fn ident_to_segment(ident: &str) -> String {
    ident.trim_start_matches("r#").replace('_', "-")
}

/// S3: inventory must not depend on build configuration — the section
/// records are emitted unconditionally, so cfg-gated cases would drift.
fn reject_cfg(attrs: &[syn::Attribute], span: proc_macro2::Span) -> Result<()> {
    for attr in attrs {
        if attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr") {
            return Err(Error::new(
                span,
                "#[suite] items may not be cfg-gated: the inventory (and its lockfile) \
                 must not depend on build configuration",
            ));
        }
    }
    Ok(())
}

/// M4: `#[case]` fns take no arguments or exactly `(ctx: &TestContext)`.
fn check_case_fn_shape(f: &ItemFn) -> Result<()> {
    if f.sig.inputs.len() > 1 {
        return Err(Error::new(
            f.sig.span(),
            "#[case] functions take no arguments or exactly one (`ctx: &TestContext`)",
        ));
    }
    Ok(())
}
