//! Derive macros for [docuconf](https://docs.rs/docuconf).
//!
//! Use them through the `docuconf` crate, which re-exports them:
//! `#[derive(serde::Deserialize, docuconf::Docuconf)]` on a configuration
//! struct, and `#[derive(serde::Deserialize, docuconf::DocuconfEnum)]` on a
//! unit-only enum used as an `enum` variable.

mod doc;

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::{
    parse_macro_input, spanned::Spanned, Attribute, Data, DeriveInput, Error, Expr, ExprLit,
    ExprUnary, Fields, Lit, LitStr, UnOp,
};

/// Declares a configuration struct as a docuconf contract.
///
/// See the `docuconf` crate documentation for the attributes.
#[proc_macro_derive(Docuconf, attributes(docuconf))]
pub fn derive_docuconf(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_struct(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Makes a unit-only enum usable as an `enum` variable. The values are the
/// variant names after serde's `rename` and `rename_all`.
#[proc_macro_derive(DocuconfEnum, attributes(docuconf))]
pub fn derive_docuconf_enum(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_enum(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

// ---------------------------------------------------------------------------
// serde attributes we need to agree with

#[derive(Default)]
struct SerdeContainer {
    rename_all: Option<String>,
    default: bool,
}

#[derive(Default)]
struct SerdeField {
    rename: Option<String>,
    skip: bool,
    flatten: bool,
    default: bool,
    with: Option<String>,
}

fn serde_container(attrs: &[Attribute]) -> syn::Result<SerdeContainer> {
    let mut out = SerdeContainer::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                if meta.input.peek(syn::Token![=]) {
                    out.rename_all = Some(meta.value()?.parse::<LitStr>()?.value());
                } else {
                    meta.parse_nested_meta(|inner| {
                        let v = inner.value()?.parse::<LitStr>()?.value();
                        if inner.path.is_ident("deserialize") {
                            out.rename_all = Some(v);
                        }
                        Ok(())
                    })?;
                }
            } else if meta.path.is_ident("default") {
                out.default = true;
                skip_value(&meta)?;
            } else {
                skip_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(out)
}

fn serde_field(attrs: &[Attribute]) -> syn::Result<SerdeField> {
    let mut out = SerdeField::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            let p = &meta.path;
            if p.is_ident("rename") {
                if meta.input.peek(syn::Token![=]) {
                    out.rename = Some(meta.value()?.parse::<LitStr>()?.value());
                } else {
                    meta.parse_nested_meta(|inner| {
                        let v = inner.value()?.parse::<LitStr>()?.value();
                        if inner.path.is_ident("deserialize") {
                            out.rename = Some(v);
                        }
                        Ok(())
                    })?;
                }
            } else if p.is_ident("skip") || p.is_ident("skip_deserializing") {
                out.skip = true;
            } else if p.is_ident("flatten") {
                out.flatten = true;
            } else if p.is_ident("default") {
                out.default = true;
                skip_value(&meta)?;
            } else if p.is_ident("with") || p.is_ident("deserialize_with") {
                out.with = Some(meta.value()?.parse::<LitStr>()?.value());
            } else {
                skip_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Consumes `= value` or `(...)` after a meta path we do not care about.
fn skip_value(meta: &syn::meta::ParseNestedMeta) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        let _: Expr = meta.value()?.parse()?;
    } else if meta.input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in meta.input);
        let _: TokenStream2 = content.parse()?;
    }
    Ok(())
}

fn rename(name: &str, rule: Option<&str>, span: Span) -> syn::Result<String> {
    let words: Vec<String> = split_words(name);
    let lower = || words.iter().map(|w| w.to_lowercase()).collect::<Vec<_>>();
    let cap = |w: &str| {
        let mut c = w.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
            None => String::new(),
        }
    };
    Ok(match rule {
        None => name.to_string(),
        Some("lowercase") => name.to_lowercase(),
        Some("UPPERCASE") => name.to_uppercase(),
        Some("snake_case") => lower().join("_"),
        Some("SCREAMING_SNAKE_CASE") => lower().join("_").to_uppercase(),
        Some("kebab-case") => lower().join("-"),
        Some("SCREAMING-KEBAB-CASE") => lower().join("-").to_uppercase(),
        Some("PascalCase") => words.iter().map(|w| cap(w)).collect(),
        Some("camelCase") => {
            let mut s = String::new();
            for (i, w) in words.iter().enumerate() {
                if i == 0 {
                    s.push_str(&w.to_lowercase());
                } else {
                    s.push_str(&cap(w));
                }
            }
            s
        }
        Some(other) => {
            return Err(Error::new(
                span,
                format!("docuconf: unsupported serde rename_all rule {other:?}"),
            ))
        }
    })
}

/// Splits a snake_case field name or a PascalCase variant name into words.
fn split_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in name.split('_').filter(|p| !p.is_empty()) {
        let mut cur = String::new();
        for c in part.chars() {
            if c.is_uppercase() && !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            cur.push(c);
        }
        if !cur.is_empty() {
            words.push(cur);
        }
    }
    words
}

// ---------------------------------------------------------------------------
// docuconf attributes

#[derive(Default)]
struct DocAttrs {
    set: Vec<String>,
    description: Option<String>,
    details: Option<String>,
    env: Option<String>,
    name: Option<String>,
    required: bool,
    secret: bool,
    url: bool,
    skip: bool,
    default: Option<(DLit, Span)>,
    min: Option<(DLit, Span)>,
    max: Option<(DLit, Span)>,
    min_length: Option<TokenStream2>,
    max_length: Option<TokenStream2>,
    pattern: Option<String>,
    values: Vec<String>,
    schemes: Vec<String>,
    min_items: Option<TokenStream2>,
    max_items: Option<TokenStream2>,
    item_min: Option<(DLit, Span)>,
    item_max: Option<(DLit, Span)>,
    item_min_length: Option<TokenStream2>,
    item_max_length: Option<TokenStream2>,
    group: Option<String>,
    examples: Vec<String>,
    deprecated: Option<String>,
    replaced_by: Option<String>,
    config_key: Option<String>,
    path: Option<String>,
    path_env: Option<String>,
    reload: Option<String>,
    max_size: Option<(DLit, Span)>,
    format: Option<String>,
    dns_names: Vec<String>,
    key_algorithms: Vec<String>,
    min_remaining: Option<String>,
    require_ca: bool,
    min_certificates: Option<TokenStream2>,
    password_var: Option<String>,
    prefix: Option<String>,
}

/// Parses `= "a,b"` (comma separated) or `("a", "b")`.
fn str_list(meta: &syn::meta::ParseNestedMeta) -> syn::Result<Vec<String>> {
    if meta.input.peek(syn::Token![=]) {
        let s = meta.value()?.parse::<LitStr>()?.value();
        return Ok(s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect());
    }
    let content;
    syn::parenthesized!(content in meta.input);
    let lits = content.parse_terminated(|p| p.parse::<LitStr>(), syn::Token![,])?;
    Ok(lits.into_iter().map(|l| l.value()).collect())
}

/// A literal written in a `#[docuconf(...)]` attribute.
#[derive(Clone, Debug, PartialEq)]
enum DLit {
    Int(i128),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<DLit>),
}

impl DLit {
    fn tokens(&self) -> TokenStream2 {
        match self {
            DLit::Int(v) => quote!(::docuconf::__private::Lit::Int(#v)),
            DLit::Float(v) => quote!(::docuconf::__private::Lit::Float(#v)),
            DLit::Str(s) => quote!(::docuconf::__private::Lit::Str(#s)),
            DLit::Bool(v) => quote!(::docuconf::__private::Lit::Bool(#v)),
            DLit::List(items) => {
                let items = items.iter().map(DLit::tokens);
                quote!(::docuconf::__private::Lit::List(&[#(#items),*]))
            }
        }
    }

    /// The literal as written, for messages.
    fn show(&self) -> String {
        match self {
            DLit::Int(i) => i.to_string(),
            DLit::Float(f) => format!("{f:?}"),
            DLit::Str(s) => format!("{s:?}"),
            DLit::Bool(b) => b.to_string(),
            DLit::List(items) => format!(
                "[{}]",
                items.iter().map(DLit::show).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// The literal's kind, as `docuconf::__private::Literal`.
    fn kind(&self) -> TokenStream2 {
        match self {
            DLit::Int(_) => quote!(::docuconf::__private::Literal::Int),
            DLit::Float(_) => quote!(::docuconf::__private::Literal::Float),
            DLit::Str(_) => quote!(::docuconf::__private::Literal::Str),
            DLit::Bool(_) => quote!(::docuconf::__private::Literal::Bool),
            DLit::List(_) => quote!(::docuconf::__private::Literal::List),
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            DLit::Int(i) => Some(*i as f64),
            DLit::Float(f) => Some(*f),
            _ => None,
        }
    }
}

fn parse_lit(expr: &Expr) -> syn::Result<DLit> {
    match expr {
        Expr::Lit(ExprLit { lit, .. }) => match lit {
            Lit::Int(i) => Ok(DLit::Int(i.base10_parse()?)),
            Lit::Float(f) => Ok(DLit::Float(f.base10_parse()?)),
            Lit::Str(s) => Ok(DLit::Str(s.value())),
            Lit::Bool(b) => Ok(DLit::Bool(b.value)),
            other => Err(Error::new(other.span(), "docuconf: unsupported literal")),
        },
        Expr::Unary(ExprUnary {
            op: UnOp::Neg(_),
            expr,
            ..
        }) => match &**expr {
            Expr::Lit(ExprLit {
                lit: Lit::Int(i), ..
            }) => Ok(DLit::Int(-i.base10_parse::<i128>()?)),
            Expr::Lit(ExprLit {
                lit: Lit::Float(f), ..
            }) => Ok(DLit::Float(-f.base10_parse::<f64>()?)),
            other => Err(Error::new(other.span(), "docuconf: expected a number")),
        },
        Expr::Array(arr) => Ok(DLit::List(
            arr.elems
                .iter()
                .map(parse_lit)
                .collect::<syn::Result<_>>()?,
        )),
        Expr::Group(g) => parse_lit(&g.expr),
        other => Err(Error::new(
            other.span(),
            "docuconf: expected a literal (string, number, bool or [list])",
        )),
    }
}

fn lit_attr(meta: &syn::meta::ParseNestedMeta) -> syn::Result<(DLit, Span)> {
    let e: Expr = meta.value()?.parse()?;
    Ok((parse_lit(&e)?, e.span()))
}

fn int_tokens(meta: &syn::meta::ParseNestedMeta) -> syn::Result<TokenStream2> {
    let e: Expr = meta.value()?.parse()?;
    match &e {
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => {
            let v: u64 = i.base10_parse()?;
            Ok(quote!(#v))
        }
        _ => Err(Error::new(
            e.span(),
            "docuconf: expected a non-negative integer",
        )),
    }
}

fn parse_doc_attrs(attrs: &[Attribute]) -> syn::Result<DocAttrs> {
    let mut a = DocAttrs::default();
    for attr in attrs.iter().filter(|x| x.path().is_ident("docuconf")) {
        attr.parse_nested_meta(|meta| {
            let key = meta
                .path
                .get_ident()
                .map(|i| i.to_string())
                .ok_or_else(|| meta.error("docuconf: expected an attribute name"))?;
            let s = |meta: &syn::meta::ParseNestedMeta| -> syn::Result<String> {
                Ok(meta.value()?.parse::<LitStr>()?.value())
            };
            match key.as_str() {
                "description" | "desc" => a.description = Some(s(&meta)?),
                "details" => a.details = Some(s(&meta)?),
                "env" => a.env = Some(s(&meta)?),
                "name" => a.name = Some(s(&meta)?),
                "required" => a.required = true,
                "secret" => a.secret = true,
                "url" => a.url = true,
                "skip" => a.skip = true,
                "default" => a.default = Some(lit_attr(&meta)?),
                "min" => a.min = Some(lit_attr(&meta)?),
                "max" => a.max = Some(lit_attr(&meta)?),
                "min_length" => a.min_length = Some(int_tokens(&meta)?),
                "max_length" => a.max_length = Some(int_tokens(&meta)?),
                "pattern" => a.pattern = Some(s(&meta)?),
                "values" => a.values = str_list(&meta)?,
                "schemes" => a.schemes = str_list(&meta)?,
                "min_items" => a.min_items = Some(int_tokens(&meta)?),
                "max_items" => a.max_items = Some(int_tokens(&meta)?),
                "item_min" => a.item_min = Some(lit_attr(&meta)?),
                "item_max" => a.item_max = Some(lit_attr(&meta)?),
                "item_min_length" => a.item_min_length = Some(int_tokens(&meta)?),
                "item_max_length" => a.item_max_length = Some(int_tokens(&meta)?),
                "group" => a.group = Some(s(&meta)?),
                "examples" => a.examples = str_list(&meta)?,
                "deprecated" => a.deprecated = Some(s(&meta)?),
                "replaced_by" => a.replaced_by = Some(s(&meta)?),
                "config_key" => a.config_key = Some(s(&meta)?),
                "path" => a.path = Some(s(&meta)?),
                "path_env" => a.path_env = Some(s(&meta)?),
                "reload" => a.reload = Some(s(&meta)?),
                "max_size" => a.max_size = Some(lit_attr(&meta)?),
                "format" => a.format = Some(s(&meta)?),
                "dns_names" => a.dns_names = str_list(&meta)?,
                "key_algorithms" => a.key_algorithms = str_list(&meta)?,
                "min_remaining" => a.min_remaining = Some(s(&meta)?),
                "require_ca" => a.require_ca = true,
                "min_certificates" => a.min_certificates = Some(int_tokens(&meta)?),
                "password_var" => a.password_var = Some(s(&meta)?),
                "prefix" => a.prefix = Some(s(&meta)?),
                other => {
                    return Err(meta.error(format!("docuconf: unknown attribute `{other}`")));
                }
            }
            a.set.push(key);
            Ok(())
        })?;
    }
    Ok(a)
}

/// The description and details from a field's `///` doc comment and its
/// `description` and `details` attributes: an attribute wins over the
/// comment, and the comment's first paragraph is the description and the
/// rest the details (see the `doc` module).
fn describe(a: &DocAttrs, attrs: &[Attribute]) -> (String, Option<String>) {
    let (desc, details) = doc::split_doc(&doc::doc_lines(attrs));
    let description = a.description.clone().unwrap_or(desc);
    let details = a
        .details
        .clone()
        .or_else(|| (!details.is_empty()).then_some(details));
    (description, details)
}

fn opt_str(v: &Option<String>) -> TokenStream2 {
    match v {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    }
}

fn opt_lit(v: &Option<(DLit, Span)>) -> TokenStream2 {
    match v {
        Some((l, _)) => {
            let t = l.tokens();
            quote!(::core::option::Option::Some(#t))
        }
        None => quote!(::core::option::Option::None),
    }
}

fn opt_tokens(v: &Option<TokenStream2>) -> TokenStream2 {
    match v {
        Some(t) => quote!(::core::option::Option::Some(#t)),
        None => quote!(::core::option::Option::None),
    }
}

// ---------------------------------------------------------------------------
// Declaration mistakes caught at compile time

/// A `panic!` with `msg` (escaped as a format string), spanned on the
/// field so the compile error points at it.
fn panic_at(span: Span, msg: String) -> TokenStream2 {
    let m = msg.replace('{', "{{").replace('}', "}}");
    quote_spanned!(span=> ::core::panic!(#m))
}

/// Checks one field's attributes. Mistakes visible in the attributes alone
/// are pushed to `errors`; the returned tokens are a `const` item whose
/// evaluation fails (a compile error) for mistakes that also need the
/// field's type, such as a string default on an integer field.
#[allow(clippy::too_many_arguments)]
fn field_checks(
    field: &str,
    ty: &syn::Type,
    span: Span,
    a: &DocAttrs,
    sf: &SerdeField,
    description: &str,
    details: Option<&str>,
    errors: &mut Vec<Error>,
) -> TokenStream2 {
    let mut err = |span: Span, msg: String| {
        errors.push(Error::new(span, format!("docuconf: {field}: {msg}")))
    };
    if let Some((d, dspan)) = &a.default {
        if a.required {
            err(*dspan, "`required` and `default` contradict each other: a variable with a default is never missing; remove one".into());
        }
        if a.secret {
            err(*dspan, "a secret variable must not have a default (it would be written into the contract and the binary); remove `default` and set the value through the environment".into());
        }
        if let Some(dv) = d.number() {
            if let Some((m, _)) = &a.min {
                if m.number().is_some_and(|mv| dv < mv) {
                    err(
                        *dspan,
                        format!(
                            "default {} is below min {}; raise the default or lower min",
                            d.show(),
                            m.show()
                        ),
                    );
                }
            }
            if let Some((m, _)) = &a.max {
                if m.number().is_some_and(|mv| dv > mv) {
                    err(
                        *dspan,
                        format!(
                            "default {} is above max {}; lower the default or raise max",
                            d.show(),
                            m.show()
                        ),
                    );
                }
            }
        }
        if let DLit::Str(v) = d {
            if !a.values.is_empty() && !a.values.contains(v) {
                err(
                    *dspan,
                    format!(
                        "default {} is not one of values({})",
                        d.show(),
                        a.values
                            .iter()
                            .map(|v| format!("{v:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
    }
    if let (Some((lo, lspan)), Some((hi, _))) = (&a.min, &a.max) {
        if let (Some(l), Some(h)) = (lo.number(), hi.number()) {
            if l > h {
                err(
                    *lspan,
                    format!("min {} is above max {}", lo.show(), hi.show()),
                );
            }
        }
    }
    if let (Some((lo, lspan)), Some((hi, _))) = (&a.item_min, &a.item_max) {
        if let (Some(l), Some(h)) = (lo.number(), hi.number()) {
            if l > h {
                err(
                    *lspan,
                    format!("item_min {} is above item_max {}", lo.show(), hi.show()),
                );
            }
        }
    }

    let panic_msg = |msg: String| panic_at(span, msg);
    // Checks that need the field's type, through the associated consts of
    // `docuconf::__private::Input`.
    let ty_name = quote!(#ty).to_string().replace(' ', "");
    let mut conds = Vec::new();
    let desc_len = description.trim().chars().count();
    if desc_len < 5 {
        let msg = if desc_len == 0 {
            format!("docuconf: {field} needs a description: add a /// doc comment above the field (at least 5 characters)")
        } else {
            format!("docuconf: {field}: description {description:?} is shorter than 5 characters; describe the input in a short phrase")
        };
        let p = panic_msg(msg);
        conds.push(quote!(if !::core::matches!(<T as I>::SHAPE, S::Group) { #p }));
    }
    // Details (SPEC §4.2): docs only, but not blank and at most 4000
    // characters, as the meta-schema's #Details says.
    if let Some(d) = details {
        let n = d.chars().count();
        let msg = if d.trim().is_empty() {
            Some(format!(
                "docuconf: {field}: details must not be blank; write them or remove `details`"
            ))
        } else if n > 4000 {
            Some(format!("docuconf: {field}: details are {n} characters (the doc comment after its first paragraph, or the details attribute); details may have at most 4000"))
        } else {
            None
        };
        if let Some(msg) = msg {
            let p = panic_msg(msg);
            conds.push(quote!(if !::core::matches!(<T as I>::SHAPE, S::Group) { #p }));
        }
    }
    if let Some((d, _)) = &a.default {
        let show = d.show();
        let p = panic_msg(format!("docuconf: {field}: `default` applies to variables; a file input or a nested group has none"));
        conds.push(quote!(if !::core::matches!(<T as I>::SHAPE, S::Var) { #p }));
        let p = panic_msg(format!("docuconf: {field}: a secret variable must not have a default (it would be written into the contract and the binary); remove `default` and set the value through the environment"));
        conds.push(quote!(if <T as I>::SECRET { #p }));
        let got = d.kind();
        let m = |what: &str| panic_msg(format!("docuconf: {field}: default {show} is not {what}"));
        let (pi, pf, ps, pd, pb, pl) = (
            m(&format!(
                "an integer; {ty_name} needs a default such as 8080"
            )),
            m(&format!("a number; {ty_name} needs a default such as 0.5")),
            m(&format!(
                "a string; {ty_name} needs a default such as \"text\""
            )),
            m("a duration; write it as a string such as \"30s\""),
            m("true or false"),
            m("a list; write it as [\"a\", \"b\"]"),
        );
        conds.push(quote! {
            if !::docuconf::__private::accepts(<T as I>::LITERAL, #got) {
                match <T as I>::LITERAL {
                    L::Int => #pi,
                    L::Float => #pf,
                    L::Str => #ps,
                    L::Duration => #pd,
                    L::Bool => #pb,
                    _ => #pl,
                }
            }
        });
        if let DLit::Str(v) = d {
            let p = panic_msg(format!("docuconf: {field}: default {show} is not one of the enum's values (its variant names after serde renames)"));
            conds.push(quote! {
                if let ::core::option::Option::Some(values) = <T as I>::ENUM_VALUES {
                    if !::docuconf::__private::contains(values, #v) { #p }
                }
            });
        }
    }
    for (which, l) in [("default", &a.default), ("min", &a.min), ("max", &a.max)] {
        if let Some((DLit::Int(v), _)) = l {
            let p = panic_msg(format!(
                "docuconf: {field}: {which} {v} is outside the range of {ty_name}"
            ));
            conds.push(quote! {
                if ::core::matches!(<T as I>::LITERAL, L::Int)
                    && (#v < <T as I>::INT_RANGE.0 || #v > <T as I>::INT_RANGE.1) { #p }
            });
        }
    }
    let with = sf.with.as_deref().unwrap_or("");
    let has_with = with.contains("humantime");
    let has_option = has_with && with.contains("option") && sf.default;
    let p = panic_msg(format!("docuconf: {field}: a Duration field needs #[serde(with = \"docuconf::humantime_serde\")] so values such as \"1m30s\" deserialize"));
    let po = panic_msg(format!("docuconf: {field}: an Option<Duration> field needs #[serde(default, with = \"docuconf::humantime_serde::option\")] so values such as \"1m30s\" deserialize and an unset variable is None"));
    conds.push(quote! {
        if <T as I>::DURATION {
            if <T as I>::OPTIONAL {
                if !#has_option { #po }
            } else if !#has_with { #p }
        }
    });

    quote_spanned! {span=>
        const _: () = {
            #[allow(unused_imports)]
            use ::docuconf::__private::{Input as I, Literal as L, Shape as S};
            type T = #ty;
            #(#conds)*
        };
    }
}

// ---------------------------------------------------------------------------

fn expand_struct(input: &DeriveInput) -> syn::Result<TokenStream2> {
    if !input.generics.params.is_empty() {
        return Err(Error::new(
            input.generics.span(),
            "docuconf: generic configuration structs are not supported",
        ));
    }
    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(n) => &n.named,
            _ => {
                return Err(Error::new(
                    input.span(),
                    "docuconf: #[derive(Docuconf)] needs a struct with named fields",
                ))
            }
        },
        _ => return Err(Error::new(
            input.span(),
            "docuconf: #[derive(Docuconf)] works on structs; use #[derive(DocuconfEnum)] for enums",
        )),
    };
    let container = serde_container(&input.attrs)?;
    if container.default {
        return Err(Error::new(
            input.span(),
            "docuconf: #[serde(default)] on the struct hides defaults from the contract; \
             give each field #[docuconf(default = ...)] instead",
        ));
    }
    let cattrs = parse_doc_attrs(&input.attrs)?;
    if let Some(bad) = cattrs.set.iter().find(|k| k.as_str() != "prefix") {
        return Err(Error::new(
            input.span(),
            format!(
                "docuconf: `{bad}` is a field attribute; on the struct only `prefix` is allowed"
            ),
        ));
    }
    let prefix = cattrs.prefix.clone().unwrap_or_default();
    let ident = &input.ident;
    let struct_name = ident.to_string();

    let mut decls = Vec::new();
    let mut checks = Vec::new();
    let mut errors: Vec<Error> = Vec::new();
    for f in fields {
        let fident = f.ident.as_ref().expect("named field");
        let rust_name = fident.to_string();
        let rust_name = rust_name
            .strip_prefix("r#")
            .unwrap_or(&rust_name)
            .to_string();
        let sf = serde_field(&f.attrs)?;
        let a = parse_doc_attrs(&f.attrs)?;
        if sf.skip || a.skip {
            continue;
        }
        if sf.flatten {
            return Err(Error::new(
                f.span(),
                "docuconf: #[serde(flatten)] is not supported; nest the struct as a field instead",
            ));
        }
        let key = match &sf.rename {
            Some(r) => r.clone(),
            None => rename(&rust_name, container.rename_all.as_deref(), f.span())?,
        };
        let (description, doc_details) = describe(&a, &f.attrs);
        let details = opt_str(&doc_details);
        let ty = &f.ty;
        let set = &a.set;
        let required = a.required;
        let secret = a.secret;
        let url = a.url;
        let require_ca = a.require_ca;
        let env = opt_str(&a.env);
        let name = opt_str(&a.name);
        let default = opt_lit(&a.default);
        let min = opt_lit(&a.min);
        let max = opt_lit(&a.max);
        let min_length = opt_tokens(&a.min_length);
        let max_length = opt_tokens(&a.max_length);
        let pattern = opt_str(&a.pattern);
        let values = &a.values;
        let schemes = &a.schemes;
        let min_items = opt_tokens(&a.min_items);
        let max_items = opt_tokens(&a.max_items);
        let item_min = opt_lit(&a.item_min);
        let item_max = opt_lit(&a.item_max);
        let item_min_length = opt_tokens(&a.item_min_length);
        let item_max_length = opt_tokens(&a.item_max_length);
        let group = opt_str(&a.group);
        let examples = &a.examples;
        let deprecated = opt_str(&a.deprecated);
        let replaced_by = opt_str(&a.replaced_by);
        let config_key = opt_str(&a.config_key);
        let path = opt_str(&a.path);
        let path_env = opt_str(&a.path_env);
        let reload = opt_str(&a.reload);
        let max_size = opt_lit(&a.max_size);
        let format = opt_str(&a.format);
        let dns_names = &a.dns_names;
        let key_algorithms = &a.key_algorithms;
        let min_remaining = opt_str(&a.min_remaining);
        let min_certificates = opt_tokens(&a.min_certificates);
        let password_var = opt_str(&a.password_var);
        let serde_with = opt_str(&sf.with);
        let serde_default = sf.default;
        let field_path = format!("{struct_name}.{rust_name}");
        let span = f.ty.span();
        checks.push(field_checks(
            &field_path,
            ty,
            f.span(),
            &a,
            &sf,
            &description,
            doc_details.as_deref(),
            &mut errors,
        ));
        decls.push(quote_spanned! {span=>
            {
                const ATTRS: ::docuconf::__private::FieldAttrs = ::docuconf::__private::FieldAttrs {
                    field: #field_path,
                    key: #key,
                    description: #description,
                    details: #details,
                    set: &[#(#set),*],
                    env: #env,
                    name: #name,
                    required: #required,
                    secret: #secret,
                    url: #url,
                    default: #default,
                    min: #min,
                    max: #max,
                    min_length: #min_length,
                    max_length: #max_length,
                    pattern: #pattern,
                    values: &[#(#values),*],
                    schemes: &[#(#schemes),*],
                    min_items: #min_items,
                    max_items: #max_items,
                    item_min: #item_min,
                    item_max: #item_max,
                    item_min_length: #item_min_length,
                    item_max_length: #item_max_length,
                    group: #group,
                    examples: &[#(#examples),*],
                    deprecated: #deprecated,
                    replaced_by: #replaced_by,
                    config_key: #config_key,
                    path: #path,
                    path_env: #path_env,
                    reload: #reload,
                    max_size: #max_size,
                    format: #format,
                    dns_names: &[#(#dns_names),*],
                    key_algorithms: &[#(#key_algorithms),*],
                    min_remaining: #min_remaining,
                    require_ca: #require_ca,
                    min_certificates: #min_certificates,
                    password_var: #password_var,
                    serde_with: #serde_with,
                    serde_default: #serde_default,
                };
                <#ty as ::docuconf::__private::Input>::declare(&ATTRS, cx);
            }
        });
    }

    // Report attribute mistakes, and still emit the impls so that uses of
    // the struct elsewhere do not add unrelated errors.
    let errors = errors.into_iter().map(Error::into_compile_error);

    Ok(quote! {
        #(#errors)*
        #(#checks)*

        impl ::docuconf::Docuconf for #ident {
            const PREFIX: &'static str = #prefix;
            fn declare_fields(cx: &mut ::docuconf::__private::DeclCx) {
                #(#decls)*
            }
        }
        impl ::docuconf::__private::Input for #ident {
            const SHAPE: ::docuconf::__private::Shape = ::docuconf::__private::Shape::Group;
            const LITERAL: ::docuconf::__private::Literal = ::docuconf::__private::Literal::None;
            fn declare(attrs: &::docuconf::__private::FieldAttrs, cx: &mut ::docuconf::__private::DeclCx) {
                cx.nested(attrs, <#ident as ::docuconf::Docuconf>::declare_fields);
            }
        }
    })
}

fn expand_enum(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let data = match &input.data {
        Data::Enum(e) => e,
        _ => {
            return Err(Error::new(
                input.span(),
                "docuconf: #[derive(DocuconfEnum)] works on enums",
            ))
        }
    };
    if !input.generics.params.is_empty() {
        return Err(Error::new(
            input.generics.span(),
            "docuconf: generic enums are not supported",
        ));
    }
    let container = serde_container(&input.attrs)?;
    let mut values = Vec::new();
    for v in &data.variants {
        if !matches!(v.fields, Fields::Unit) {
            return Err(Error::new(
                v.span(),
                "docuconf: enum variables need unit variants only",
            ));
        }
        let sf = serde_field(&v.attrs)?;
        if sf.skip {
            continue;
        }
        let name = match sf.rename {
            Some(r) => r,
            None => rename(
                &v.ident.to_string(),
                container.rename_all.as_deref(),
                v.span(),
            )?,
        };
        values.push(name);
    }
    let ident = &input.ident;
    Ok(quote! {
        impl ::docuconf::__private::Input for #ident {
            const LITERAL: ::docuconf::__private::Literal = ::docuconf::__private::Literal::Str;
            const ENUM_VALUES: ::core::option::Option<&'static [&'static str]> =
                ::core::option::Option::Some(&[#(#values),*]);
            fn declare(attrs: &::docuconf::__private::FieldAttrs, cx: &mut ::docuconf::__private::DeclCx) {
                cx.var(attrs, ::docuconf::__private::VarKind::Enum(
                    ::std::vec![#(::std::string::String::from(#values)),*]
                ));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compile errors `#[derive(Docuconf)]` reports for `input`.
    fn errors(input: DeriveInput) -> String {
        let out = expand_struct(&input).unwrap_or_else(Error::into_compile_error);
        out.to_string()
    }

    /// The `const` checks generated for `input`, as text.
    fn checks(input: DeriveInput) -> String {
        expand_struct(&input).unwrap().to_string()
    }

    #[test]
    fn attribute_mistakes_are_compile_errors() {
        let out = errors(syn::parse_quote! {
            struct Config {
                /// HTTP listen port.
                #[docuconf(required, default = 8080)]
                port: u16,
                /// Database password.
                #[docuconf(secret, default = "changeme")]
                db_password: String,
                /// Threads serving requests.
                #[docuconf(default = 100, min = 1, max = 64)]
                workers: u8,
                /// Retry ratio.
                #[docuconf(default = 0.5, min = 0.75)]
                ratio: f64,
                /// Deployment environment.
                #[docuconf(values("dev", "prod"), default = "staging")]
                environment: String,
                /// Window.
                #[docuconf(min = 10, max = 5)]
                window: i32,
                /// Shards.
                #[docuconf(item_min = 9, item_max = 2)]
                shards: Vec<u16>,
            }
        });
        for want in [
            "docuconf: Config.port: `required` and `default` contradict each other",
            "docuconf: Config.db_password: a secret variable must not have a default",
            "docuconf: Config.workers: default 100 is above max 64; lower the default or raise max",
            "docuconf: Config.ratio: default 0.5 is below min 0.75",
            "docuconf: Config.environment: default \\\"staging\\\" is not one of values(\\\"dev\\\", \\\"prod\\\")",
            "docuconf: Config.window: min 10 is above max 5",
            "docuconf: Config.shards: item_min 9 is above item_max 2",
        ] {
            assert!(out.contains(want), "missing {want:?} in\n{out}");
        }
        // The impls are still emitted, so uses elsewhere add no errors.
        assert!(out.contains("impl :: docuconf :: Docuconf for Config"));
    }

    #[test]
    fn type_dependent_mistakes_become_const_checks() {
        let out = checks(syn::parse_quote! {
            struct Config {
                #[docuconf(default = "abc")]
                port: u16,
                /// Upstream request timeout.
                timeout: std::time::Duration,
            }
        });
        for want in [
            "docuconf: Config.port needs a description: add a /// doc comment above the field",
            "docuconf: Config.port: default \\\"abc\\\" is not an integer; u16 needs a default such as 8080",
            "docuconf: Config.timeout: a Duration field needs #[serde(with = \\\"docuconf::humantime_serde\\\")]",
            "docuconf: Config.timeout: an Option<Duration> field needs #[serde(default, with = \\\"docuconf::humantime_serde::option\\\")]",
        ] {
            assert!(out.contains(want), "missing {want:?} in\n{out}");
        }
        assert!(!out.contains("compile_error"), "{out}");
    }

    #[test]
    fn details_mistakes_become_const_checks() {
        let long = format!("Too much to say.\n\n{}", "日本".repeat(2000) + "日");
        let out = checks(syn::parse_quote! {
            struct Config {
                /// Blank details.
                #[docuconf(details = " \n\t")]
                blank: String,
                #[doc = #long]
                too_long: String,
                /// Just enough to say.
                #[docuconf(details = "Fine.")]
                fine: String,
            }
        });
        for want in [
            "docuconf: Config.blank: details must not be blank",
            "docuconf: Config.too_long: details are 4001 characters (the doc comment after its first paragraph, or the details attribute); details may have at most 4000",
        ] {
            assert!(out.contains(want), "missing {want:?} in\n{out}");
        }
        assert!(!out.contains("Config.fine: details"), "{out}");
    }

    #[test]
    fn well_formed_fields_have_no_errors() {
        let out = checks(syn::parse_quote! {
            struct Config {
                /// HTTP listen port.
                #[docuconf(default = 8080, min = 1)]
                port: u16,
                /// Deployment environment.
                #[docuconf(values("dev", "prod"), default = "dev")]
                environment: String,
            }
        });
        assert!(!out.contains("compile_error"), "{out}");
        assert!(!out.contains("needs a description"), "{out}");
    }
}
