//! Derive macros for [docuconf](https://docs.rs/docuconf).
//!
//! Use them through the `docuconf` crate, which re-exports them:
//! `#[derive(serde::Deserialize, docuconf::Docuconf)]` on a configuration
//! struct, and `#[derive(serde::Deserialize, docuconf::DocuconfEnum)]` on a
//! unit-only enum used as an `enum` variable.

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
    env: Option<String>,
    name: Option<String>,
    required: bool,
    secret: bool,
    url: bool,
    skip: bool,
    default: Option<TokenStream2>,
    min: Option<TokenStream2>,
    max: Option<TokenStream2>,
    min_length: Option<TokenStream2>,
    max_length: Option<TokenStream2>,
    pattern: Option<String>,
    values: Vec<String>,
    schemes: Vec<String>,
    min_items: Option<TokenStream2>,
    max_items: Option<TokenStream2>,
    item_min: Option<TokenStream2>,
    item_max: Option<TokenStream2>,
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
    max_size: Option<TokenStream2>,
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

fn lit_tokens(expr: &Expr) -> syn::Result<TokenStream2> {
    match expr {
        Expr::Lit(ExprLit { lit, .. }) => match lit {
            Lit::Int(i) => {
                let v: i128 = i.base10_parse()?;
                Ok(quote!(::docuconf::__private::Lit::Int(#v)))
            }
            Lit::Float(f) => {
                let v: f64 = f.base10_parse()?;
                Ok(quote!(::docuconf::__private::Lit::Float(#v)))
            }
            Lit::Str(s) => Ok(quote!(::docuconf::__private::Lit::Str(#s))),
            Lit::Bool(b) => {
                let v = b.value;
                Ok(quote!(::docuconf::__private::Lit::Bool(#v)))
            }
            other => Err(Error::new(other.span(), "docuconf: unsupported literal")),
        },
        Expr::Unary(ExprUnary {
            op: UnOp::Neg(_),
            expr,
            ..
        }) => match &**expr {
            Expr::Lit(ExprLit {
                lit: Lit::Int(i), ..
            }) => {
                let v: i128 = -i.base10_parse::<i128>()?;
                Ok(quote!(::docuconf::__private::Lit::Int(#v)))
            }
            Expr::Lit(ExprLit {
                lit: Lit::Float(f), ..
            }) => {
                let v: f64 = -f.base10_parse::<f64>()?;
                Ok(quote!(::docuconf::__private::Lit::Float(#v)))
            }
            other => Err(Error::new(other.span(), "docuconf: expected a number")),
        },
        Expr::Array(arr) => {
            let items = arr
                .elems
                .iter()
                .map(lit_tokens)
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(quote!(::docuconf::__private::Lit::List(&[#(#items),*])))
        }
        Expr::Group(g) => lit_tokens(&g.expr),
        other => Err(Error::new(
            other.span(),
            "docuconf: expected a literal (string, number, bool or [list])",
        )),
    }
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
                "env" => a.env = Some(s(&meta)?),
                "name" => a.name = Some(s(&meta)?),
                "required" => a.required = true,
                "secret" => a.secret = true,
                "url" => a.url = true,
                "skip" => a.skip = true,
                "default" => a.default = Some(lit_tokens(&meta.value()?.parse()?)?),
                "min" => a.min = Some(lit_tokens(&meta.value()?.parse()?)?),
                "max" => a.max = Some(lit_tokens(&meta.value()?.parse()?)?),
                "min_length" => a.min_length = Some(int_tokens(&meta)?),
                "max_length" => a.max_length = Some(int_tokens(&meta)?),
                "pattern" => a.pattern = Some(s(&meta)?),
                "values" => a.values = str_list(&meta)?,
                "schemes" => a.schemes = str_list(&meta)?,
                "min_items" => a.min_items = Some(int_tokens(&meta)?),
                "max_items" => a.max_items = Some(int_tokens(&meta)?),
                "item_min" => a.item_min = Some(lit_tokens(&meta.value()?.parse()?)?),
                "item_max" => a.item_max = Some(lit_tokens(&meta.value()?.parse()?)?),
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
                "max_size" => a.max_size = Some(lit_tokens(&meta.value()?.parse()?)?),
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

/// Joins `///` doc comment lines into one description, dropping a single
/// trailing period so "HTTP listen port." becomes "HTTP listen port".
fn doc_comment(attrs: &[Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("doc")) {
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
            {
                for line in s.value().lines() {
                    let t = line.trim();
                    if !t.is_empty() {
                        lines.push(t.to_string());
                    }
                }
            }
        }
    }
    let mut s = lines.join(" ");
    if s.ends_with('.') && !s.ends_with("..") {
        s.pop();
    }
    s
}

fn opt_str(v: &Option<String>) -> TokenStream2 {
    match v {
        Some(s) => quote!(::core::option::Option::Some(#s)),
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
        let description = a
            .description
            .clone()
            .unwrap_or_else(|| doc_comment(&f.attrs));
        let ty = &f.ty;
        let set = &a.set;
        let required = a.required;
        let secret = a.secret;
        let url = a.url;
        let require_ca = a.require_ca;
        let env = opt_str(&a.env);
        let name = opt_str(&a.name);
        let default = opt_tokens(&a.default);
        let min = opt_tokens(&a.min);
        let max = opt_tokens(&a.max);
        let min_length = opt_tokens(&a.min_length);
        let max_length = opt_tokens(&a.max_length);
        let pattern = opt_str(&a.pattern);
        let values = &a.values;
        let schemes = &a.schemes;
        let min_items = opt_tokens(&a.min_items);
        let max_items = opt_tokens(&a.max_items);
        let item_min = opt_tokens(&a.item_min);
        let item_max = opt_tokens(&a.item_max);
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
        let max_size = opt_tokens(&a.max_size);
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
        decls.push(quote_spanned! {span=>
            {
                const ATTRS: ::docuconf::__private::FieldAttrs = ::docuconf::__private::FieldAttrs {
                    field: #field_path,
                    key: #key,
                    description: #description,
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

    Ok(quote! {
        impl ::docuconf::Docuconf for #ident {
            const PREFIX: &'static str = #prefix;
            fn declare_fields(cx: &mut ::docuconf::__private::DeclCx) {
                #(#decls)*
            }
        }
        impl ::docuconf::__private::Input for #ident {
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
            fn declare(attrs: &::docuconf::__private::FieldAttrs, cx: &mut ::docuconf::__private::DeclCx) {
                cx.var(attrs, ::docuconf::__private::VarKind::Enum(
                    ::std::vec![#(::std::string::String::from(#values)),*]
                ));
            }
        }
    })
}
