//! `#[component_test_sdk::suite]`: the module-attribute suite macro.
//!
//! Walks the annotated module tree collecting `#[case]` functions and
//! `#[case_generator]` rows; derives case names from the module path
//! (idents mapped `_` → `-`); resolves tag inheritance
//! (nearest-declaration-per-feature wins); validates names/tags and the
//! decline-pair lint at expansion; and emits the contract bindings
//! (WIT embedded at macro build time — consumers never vendor it),
//! the registry, the glue impls, the `export!`, and the
//! `component-test:tags@0.1` inventory records.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, spanned::Spanned, Error, Expr, ExprLit, Ident, Item, ItemFn, ItemMod, Lit,
    Meta, Result,
};

/// The contract WIT, embedded at macro build time.
const CONTRACT_WIT: &str = include_str!("../tests.wit");

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
        quote! {
            registry.case(#name, &[#(#tags),*], move |ctx| #body);
        }
    });
    let gen_registrations = gens.iter().map(|g| {
        let prefix = &g.prefix;
        let tags = &g.tags;
        let path = path_tokens(&g.fn_path);
        quote! {
            for generated in #path() {
                registry.generated(#prefix, &[#(#tags),*], generated);
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
            quote! {
                const _: () = {
                    const RECORD: &str = #line;
                    #[link_section = "component-test:tags@0.1"]
                    #[used]
                    static TAGS_RECORD: [u8; RECORD.len()] =
                        ::component_test_sdk::str_to_array::<{ RECORD.len() }>(RECORD);
                };
            }
        });

    let wit = CONTRACT_WIT;
    let glue = quote! {
        #[allow(warnings)]
        mod __ct_bindings {
            wit_bindgen::generate!({
                inline: #wit,
                world: "suite",
                generate_all,
            });
        }

        pub use __ct_bindings::lann::component_test::test_context::Context;

        #(#records)*

        fn __ct_registry() -> ::component_test_sdk::Registry<Context> {
            let mut registry = ::component_test_sdk::Registry::new();
            #(#registrations)*
            #(#gen_registrations)*
            registry
        }

        ::std::thread_local! {
            static __CT_REGISTRY: ::std::cell::OnceCell<::component_test_sdk::Registry<Context>> =
                const { ::std::cell::OnceCell::new() };
        }

        fn __ct_with_registry<R>(
            f: impl FnOnce(&::component_test_sdk::Registry<Context>) -> R,
        ) -> R {
            __CT_REGISTRY.with(|cell| f(cell.get_or_init(__ct_registry)))
        }

        struct __CtCase {
            index: usize,
        }

        impl __ct_bindings::exports::lann::component_test::tests::GuestTestCase for __CtCase {
            fn name(&self) -> String {
                __ct_with_registry(|reg| reg.get(self.index).unwrap().name.as_str().to_string())
            }

            async fn run(
                &self,
                ctx: &Context,
            ) -> Result<(), __ct_bindings::exports::lann::component_test::tests::Outcome> {
                use __ct_bindings::exports::lann::component_test::tests::Outcome;
                let verdict = __ct_with_registry(|reg| (reg.get(self.index).unwrap().run)(ctx));
                verdict.await.map_err(|failure| match failure {
                    ::component_test_sdk::Failure::Failed(d) => Outcome::Failed(d),
                    ::component_test_sdk::Failure::Skipped(d) => Outcome::Skipped(d),
                })
            }
        }

        struct __CtSuite;

        impl __ct_bindings::exports::lann::component_test::tests::Guest for __CtSuite {
            type TestCase = __CtCase;

            async fn all() -> Vec<__ct_bindings::exports::lann::component_test::tests::TestCase> {
                use __ct_bindings::exports::lann::component_test::tests::TestCase;
                __ct_with_registry(|reg| {
                    (0..reg.len())
                        .map(|index| TestCase::new(__CtCase { index }))
                        .collect()
                })
            }
        }

        __ct_bindings::export!(__CtSuite with_types_in __ct_bindings);
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
                let mut scope = inherited.clone();
                if let Some(tags) = take_attr_tags(&mut m.attrs, "cases")? {
                    merge_scope(&mut scope, tags, m.span())?;
                }
                let seg = ident_to_segment(&m.ident.to_string());
                let child_prefix = format!("{name_prefix}/{seg}");
                mod_path.push(m.ident.clone());
                if let Some((_, children)) = m.content.as_mut() {
                    // Submodules see the suite-level names via `super`.
                    children.insert(
                        0,
                        Item::Verbatim(quote! {
                            #[allow(unused_imports)]
                            use super::Context;
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
                if f.attrs
                    .iter()
                    .any(|a| a.path().is_ident("case") || a.path().is_ident("case_generator"))
                {
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
                        name: format!("{name_prefix}/{leaf}"),
                        tags: scope.iter().map(|(_, t)| t.clone()).collect(),
                        fn_path,
                        is_async: f.sig.asyncness.is_some(),
                        has_ctx: !f.sig.inputs.is_empty(),
                        span: f.span(),
                    });
                } else if let Some(gen_attr) = take_generator_attr(f)? {
                    let mut scope = inherited.clone();
                    merge_scope(&mut scope, gen_attr.tags, f.span())?;
                    let prefix = match gen_attr.prefix {
                        Some(p) => format!("{name_prefix}/{p}"),
                        None => {
                            return Err(Error::new(
                                f.span(),
                                "#[case_generator] requires prefix = \"...\"",
                            ))
                        }
                    };
                    let mut fn_path = mod_path.clone();
                    fn_path.push(f.sig.ident.clone());
                    gens.push(GenDef {
                        prefix,
                        tags: scope.iter().map(|(_, t)| t.clone()).collect(),
                        fn_path,
                        span: f.span(),
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

fn take_generator_attr(f: &mut ItemFn) -> Result<Option<GenAttr>> {
    let Some(pos) = f
        .attrs
        .iter()
        .position(|a| a.path().is_ident("case_generator"))
    else {
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
    Ok(Some(out))
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

    let mut seen = BTreeSet::new();
    let mut positive: BTreeSet<String> = BTreeSet::new();
    let mut negative: BTreeSet<String> = BTreeSet::new();
    let sort = |tags: &[String], pos: &mut BTreeSet<String>, neg: &mut BTreeSet<String>| {
        for t in tags {
            match t.strip_prefix('!') {
                Some(f) => neg.insert(f.to_string()),
                None => pos.insert(t.clone()),
            };
        }
    };
    for c in cases {
        CaseName::parse(&c.name).map_err(|e| Error::new(c.span, format!("`{}`: {e}", c.name)))?;
        if !seen.insert(c.name.clone()) {
            return Err(Error::new(c.span, format!("duplicate case `{}`", c.name)));
        }
        sort(&c.tags, &mut positive, &mut negative);
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
        sort(&g.tags, &mut positive, &mut negative);
    }
    let unpaired: Vec<&String> = positive.difference(&negative).collect();
    if !unpaired.is_empty() {
        return Err(Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "decline-pair lint: feature(s) {} have positively-tagged cases but no \
                 `!feature` decline-asserting case",
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
    ident.replace('_', "-")
}
