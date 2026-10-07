#![allow(missing_docs)]
//! Support code for the derive macros. Not a public API: names here may
//! change in any release.

pub use crate::decl::{DeclCx, FileKind, ItemKind, VarKind};

/// A literal written in a `#[docuconf(...)]` attribute.
#[derive(Debug, Clone, Copy)]
pub enum Lit {
    Int(i128),
    Float(f64),
    Str(&'static str),
    Bool(bool),
    List(&'static [Lit]),
}

impl Lit {
    /// The literal as it is written in the attribute, for messages.
    pub fn show(&self) -> String {
        match self {
            Lit::Int(i) => i.to_string(),
            Lit::Float(f) => format!("{f:?}"),
            Lit::Str(s) => format!("{s:?}"),
            Lit::Bool(b) => b.to_string(),
            Lit::List(items) => format!(
                "[{}]",
                items.iter().map(Lit::show).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// Everything the derive macro knows about one field.
#[derive(Debug)]
pub struct FieldAttrs {
    /// `Struct.field`, for messages.
    pub field: &'static str,
    /// The serde key the field deserializes from.
    pub key: &'static str,
    /// The `///` doc comment or `description` attribute.
    pub description: &'static str,
    /// Names of the docuconf attributes written on the field.
    pub set: &'static [&'static str],
    pub env: Option<&'static str>,
    pub name: Option<&'static str>,
    pub required: bool,
    pub secret: bool,
    pub url: bool,
    pub default: Option<Lit>,
    pub min: Option<Lit>,
    pub max: Option<Lit>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub pattern: Option<&'static str>,
    pub values: &'static [&'static str],
    pub schemes: &'static [&'static str],
    pub min_items: Option<u64>,
    pub max_items: Option<u64>,
    pub item_min: Option<Lit>,
    pub item_max: Option<Lit>,
    pub item_min_length: Option<u64>,
    pub item_max_length: Option<u64>,
    pub group: Option<&'static str>,
    pub examples: &'static [&'static str],
    pub deprecated: Option<&'static str>,
    pub replaced_by: Option<&'static str>,
    pub config_key: Option<&'static str>,
    pub path: Option<&'static str>,
    pub path_env: Option<&'static str>,
    pub reload: Option<&'static str>,
    pub max_size: Option<Lit>,
    pub format: Option<&'static str>,
    pub dns_names: &'static [&'static str],
    pub key_algorithms: &'static [&'static str],
    pub min_remaining: Option<&'static str>,
    pub require_ca: bool,
    pub min_certificates: Option<u64>,
    pub password_var: Option<&'static str>,
    pub serde_with: Option<&'static str>,
    pub serde_default: bool,
}

/// What a field declares, for compile-time checks in the derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Var,
    File,
    Group,
}

/// The kind of `default = ...` literal a field takes, for compile-time
/// checks in the derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Literal {
    Int,
    /// A float field takes an integer or a float literal.
    Float,
    Str,
    /// A Go duration, written as a string.
    Duration,
    Bool,
    List,
    /// Files and nested groups take no default.
    None,
}

/// Whether a field whose defaults are `expected` accepts a literal of kind
/// `got`.
pub const fn accepts(expected: Literal, got: Literal) -> bool {
    matches!(
        (expected, got),
        (Literal::Int, Literal::Int)
            | (Literal::Float, Literal::Int | Literal::Float)
            | (Literal::Str | Literal::Duration, Literal::Str)
            | (Literal::Bool, Literal::Bool)
            | (Literal::List, Literal::List)
    )
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `values.contains(&s)`, usable in a `const`.
pub const fn contains(values: &[&str], s: &str) -> bool {
    let mut i = 0;
    while i < values.len() {
        if str_eq(values[i], s) {
            return true;
        }
        i += 1;
    }
    false
}

/// Implemented by every type that can be a field of a `#[derive(Docuconf)]`
/// struct: variable types, file input types, nested structs, `Option<T>`
/// and `Secret<T>`.
///
/// The associated consts let the derive reject declaration mistakes at
/// compile time.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a docuconf input type",
    label = "docuconf cannot declare a field of this type",
    note = "use String, an integer, f32/f64, bool, std::time::Duration, url::Url, Vec<String> or Vec<integer>, docuconf::Json<T>, docuconf::Secret<T>, Option<T>, a #[derive(DocuconfEnum)] enum, a file input type or a nested #[derive(Docuconf)] struct",
    note = "for a map, use docuconf::Json<HashMap<..>>; to load the field some other way, mark it #[docuconf(skip)]"
)]
pub trait Input {
    const SHAPE: Shape = Shape::Var;
    const SECRET: bool = false;
    const OPTIONAL: bool = false;
    const DURATION: bool = false;
    const LITERAL: Literal;
    const INT_RANGE: (i128, i128) = (i128::MIN, i128::MAX);
    const ENUM_VALUES: Option<&'static [&'static str]> = None;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx);
}

impl<T: Input> Input for Option<T> {
    const SHAPE: Shape = T::SHAPE;
    const SECRET: bool = T::SECRET;
    const OPTIONAL: bool = true;
    const DURATION: bool = T::DURATION;
    const LITERAL: Literal = T::LITERAL;
    const INT_RANGE: (i128, i128) = T::INT_RANGE;
    const ENUM_VALUES: Option<&'static [&'static str]> = T::ENUM_VALUES;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.with_optional(|cx| T::declare(attrs, cx));
    }
}

impl<T: Input> Input for crate::Secret<T> {
    const SHAPE: Shape = T::SHAPE;
    const SECRET: bool = true;
    const OPTIONAL: bool = T::OPTIONAL;
    const DURATION: bool = T::DURATION;
    const LITERAL: Literal = T::LITERAL;
    const INT_RANGE: (i128, i128) = T::INT_RANGE;
    const ENUM_VALUES: Option<&'static [&'static str]> = T::ENUM_VALUES;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.with_secret(|cx| T::declare(attrs, cx));
    }
}

/// `secrecy::SecretBox<T>`, the ecosystem's secret wrapper (zeroized on
/// drop), declares a secret variable just like [`Secret<T>`](crate::Secret).
#[cfg(feature = "secrecy")]
impl<T: Input + secrecy::zeroize::Zeroize> Input for secrecy::SecretBox<T> {
    const SHAPE: Shape = T::SHAPE;
    const SECRET: bool = true;
    const OPTIONAL: bool = T::OPTIONAL;
    const DURATION: bool = T::DURATION;
    const LITERAL: Literal = T::LITERAL;
    const INT_RANGE: (i128, i128) = T::INT_RANGE;
    const ENUM_VALUES: Option<&'static [&'static str]> = T::ENUM_VALUES;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.with_secret(|cx| T::declare(attrs, cx));
    }
}

/// `secrecy::SecretString` declares a secret string variable.
#[cfg(feature = "secrecy")]
impl Input for secrecy::SecretBox<str> {
    const SECRET: bool = true;
    const LITERAL: Literal = Literal::Str;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.with_secret(|cx| cx.var(attrs, VarKind::String));
    }
}

macro_rules! int_input {
    ($($t:ty),*) => {$(
        impl Input for $t {
            const LITERAL: Literal = Literal::Int;
            const INT_RANGE: (i128, i128) = (<$t>::MIN as i128, <$t>::MAX as i128);
            fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
                cx.var(attrs, VarKind::Int {
                    min: (<$t>::MIN as i128).max(i64::MIN as i128) as i64,
                    max: (<$t>::MAX as i128).min(i64::MAX as i128) as i64,
                });
            }
        }
        impl ListItem for $t {
            fn item() -> ItemKind {
                ItemKind::Int {
                    min: (<$t>::MIN as i128).max(i64::MIN as i128) as i64,
                    max: (<$t>::MAX as i128).min(i64::MAX as i128) as i64,
                }
            }
        }
    )*};
}
int_input!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl Input for f32 {
    const LITERAL: Literal = Literal::Float;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Float);
    }
}
impl Input for f64 {
    const LITERAL: Literal = Literal::Float;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Float);
    }
}
impl Input for bool {
    const LITERAL: Literal = Literal::Bool;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Bool);
    }
}
impl Input for String {
    const LITERAL: Literal = Literal::Str;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::String);
    }
}
impl Input for std::time::Duration {
    const DURATION: bool = true;
    const LITERAL: Literal = Literal::Duration;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Duration);
    }
}
impl Input for url::Url {
    const LITERAL: Literal = Literal::Str;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Url);
    }
}

/// Element types allowed in a `list` variable.
pub trait ListItem {
    fn item() -> ItemKind;
}
impl ListItem for String {
    fn item() -> ItemKind {
        ItemKind::String
    }
}
impl<T: ListItem> Input for Vec<T> {
    const LITERAL: Literal = Literal::List;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::List(T::item()));
    }
}

impl<T> Input for crate::Json<T>
where
    T: serde::de::DeserializeOwned + schemars::JsonSchema,
{
    const LITERAL: Literal = Literal::Str;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(
            attrs,
            VarKind::Json {
                schema: crate::schema::schema_for::<T>(),
                bind: crate::schema::bind::<T>,
            },
        );
    }
}

impl<T> Input for crate::ConfigFile<T>
where
    T: serde::de::DeserializeOwned + schemars::JsonSchema,
{
    const SHAPE: Shape = Shape::File;
    const LITERAL: Literal = Literal::None;
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.file(
            attrs,
            FileKind::Config {
                schema: crate::schema::schema_for::<T>(),
                bind: crate::schema::bind::<T>,
            },
        );
    }
}

macro_rules! file_input {
    ($($(#[$m:meta])* $t:ty => $k:ident),*) => {$(
        $(#[$m])*
        impl Input for $t {
            const SHAPE: Shape = Shape::File;
            const LITERAL: Literal = Literal::None;
            fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
                cx.file(attrs, FileKind::$k);
            }
        }
    )*};
}
file_input!(
    #[cfg(feature = "tls")]
    crate::TlsKeyPair => Tls,
    #[cfg(feature = "tls")]
    crate::CaBundle => CaBundle,
    #[cfg(feature = "keystore")]
    crate::Keystore => Keystore,
    crate::TextFile => Text,
    crate::BinaryFile => Binary
);
