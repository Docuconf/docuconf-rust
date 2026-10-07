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

/// Implemented by every type that can be a field of a `#[derive(Docuconf)]`
/// struct: variable types, file input types, nested structs, `Option<T>`
/// and `Secret<T>`.
pub trait Input {
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx);
}

impl<T: Input> Input for Option<T> {
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.with_optional(|cx| T::declare(attrs, cx));
    }
}

impl<T: Input> Input for crate::Secret<T> {
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.with_secret(|cx| T::declare(attrs, cx));
    }
}

macro_rules! int_input {
    ($($t:ty),*) => {$(
        impl Input for $t {
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
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Float);
    }
}
impl Input for f64 {
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Float);
    }
}
impl Input for bool {
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Bool);
    }
}
impl Input for String {
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::String);
    }
}
impl Input for std::time::Duration {
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::Duration);
    }
}
impl Input for url::Url {
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
    fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
        cx.var(attrs, VarKind::List(T::item()));
    }
}

impl<T> Input for crate::Json<T>
where
    T: serde::de::DeserializeOwned + schemars::JsonSchema,
{
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
    ($($t:ty => $k:ident),*) => {$(
        impl Input for $t {
            fn declare(attrs: &FieldAttrs, cx: &mut DeclCx) {
                cx.file(attrs, FileKind::$k);
            }
        }
    )*};
}
file_input!(
    crate::TlsKeyPair => Tls,
    crate::CaBundle => CaBundle,
    crate::Keystore => Keystore,
    crate::TextFile => Text,
    crate::BinaryFile => Binary
);
