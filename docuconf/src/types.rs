//! Field types docuconf adds to the app's struct: `Secret<T>`, `Json<T>`
//! and the file input types.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
#[cfg(feature = "tls")]
use std::time::SystemTime;

#[cfg(feature = "tls")]
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer};

/// A secret value. Its `Debug` output is redacted, and so is its
/// `Serialize` output (`"***"`), so a `/config` endpoint or a log line that
/// serializes the whole config struct never shows it. docuconf marks the
/// variable `secret: true` in the contract and never prints its value.
///
/// `Secret` has no `PartialEq`: compare [`expose`](Secret::expose)d values
/// deliberately (with a constant-time comparison for credentials). For
/// zeroize-on-drop, enable the `secrecy` feature and use
/// `secrecy::SecretString` or `secrecy::SecretBox<T>` fields instead.
///
/// ```
/// let s = docuconf::Secret::new("hunter2".to_string());
/// assert_eq!(format!("{s:?}"), "Secret(***)");
/// assert_eq!(serde_json::to_string(&s).unwrap(), r#""***""#);
/// assert_eq!(s.expose(), "hunter2");
/// ```
#[derive(Clone, Default)]
pub struct Secret<T>(T);

impl<T> serde::Serialize for Secret<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("***")
    }
}

impl<T> Secret<T> {
    /// Wraps a value.
    pub fn new(value: T) -> Self {
        Secret(value)
    }

    /// The secret value. Take care not to log it.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Unwraps the secret value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Secret)
    }
}

/// A set of secret keys that are all valid at once, so a key can be
/// rotated without an outage (contract type `keySet`, SPEC §4.3 and §6.1).
/// It is for the side that verifies: webhook signatures, inbound API keys,
/// JWT HMAC verification, cookie-signing fallbacks.
///
/// ```
/// #[derive(serde::Deserialize, docuconf::Docuconf)]
/// struct Config {
///     /// Keys that verify the signature on incoming webhooks.
///     #[docuconf(key_min_length = 32, key_max_length = 256)]
///     webhook_keys: docuconf::KeySet,
/// }
/// ```
///
/// A key set is always secret. The platform supplies it like a secret
/// list, as one Secret key holding `old,new` during a rotation (`csv`, the
/// default; `encoding = "json"` takes a JSON array, and `separator` changes
/// the comma). The attributes are `min_keys` (default 1, at least 1),
/// `max_keys` (default 2), `key_min_length` and `key_max_length`, in
/// characters. Keys are never trimmed, and an empty key (a stray
/// separator) is always `out_of_range`.
///
/// A rotation takes three steps, which the generated docs print: add the
/// new key and roll out; switch the sender to the new key; remove the old
/// key and roll out.
///
/// Like [`Secret`], a key set prints as `***` under `Debug`, `Display`
/// and `Serialize`. Check a candidate with [`contains`](KeySet::contains)
/// or [`verify`](KeySet::verify), which take the same time whichever key
/// matches.
///
/// ```
/// let keys = docuconf::KeySet::new(["old-key", "new-key"]);
/// assert!(keys.contains("new-key"));
/// assert!(!keys.contains("new-ke"));
/// assert_eq!(format!("{keys:?} {keys}"), "KeySet(***) ***");
/// assert_eq!(serde_json::to_string(&keys).unwrap(), r#""***""#);
/// ```
#[derive(Clone)]
pub struct KeySet {
    keys: Vec<String>,
}

impl KeySet {
    /// A key set holding `keys`, in order. Meant for tests: an app gets its
    /// key set from [`load`](crate::load).
    pub fn new<I, K>(keys: I) -> Self
    where
        I: IntoIterator<Item = K>,
        K: Into<String>,
    {
        KeySet {
            keys: keys.into_iter().map(Into::into).collect(),
        }
    }

    /// The keys, in the order the platform gave them. Take care not to
    /// log them.
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// The number of keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the set has no keys (never true for a loaded key set, which
    /// has at least `min_keys`).
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Whether `candidate` is one of the keys, such as an API key a caller
    /// presents. It compares `candidate` with every key in constant time,
    /// so the time taken does not say which key matched, or how much of
    /// one; it depends only on the number of keys and their lengths.
    pub fn contains(&self, candidate: impl AsRef<[u8]>) -> bool {
        let candidate = candidate.as_ref();
        let mut found = 0u8;
        for key in &self.keys {
            found |= u8::from(constant_time_eq(key.as_bytes(), candidate));
        }
        found == 1
    }

    /// Calls `check` with each key and returns whether any call returned
    /// true. It is for checks that need the key itself, such as an HMAC:
    ///
    /// ```
    /// let keys = docuconf::KeySet::new(["old-key", "new-key"]);
    /// let (body, signature) = (&b"payload"[..], &b"new-key:payload"[..]);
    /// let ok = keys.verify(|key| {
    ///     // Real code computes the HMAC of `body` with `key` and compares
    ///     // it with `signature` in constant time (as hmac's verify_slice).
    ///     [key, b":", body].concat() == signature
    /// });
    /// assert!(ok);
    /// ```
    ///
    /// Every key is tried, even after one matches, so the time taken does
    /// not say which key matched. `check` should compare in constant time
    /// itself.
    pub fn verify(&self, mut check: impl FnMut(&[u8]) -> bool) -> bool {
        let mut ok = false;
        for key in &self.keys {
            ok |= check(key.as_bytes());
        }
        ok
    }
}

/// Compares two byte strings in time that depends only on their lengths.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

impl fmt::Debug for KeySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeySet(***)")
    }
}

/// Two key sets are equal when they hold the same keys in the same order;
/// each pair of keys is compared in constant time.
impl PartialEq for KeySet {
    fn eq(&self, other: &Self) -> bool {
        self.keys.len() == other.keys.len()
            && self.keys.iter().zip(&other.keys).fold(true, |ok, (a, b)| {
                constant_time_eq(a.as_bytes(), b.as_bytes()) & ok
            })
    }
}

impl fmt::Display for KeySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl serde::Serialize for KeySet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("***")
    }
}

impl<'de> Deserialize<'de> for KeySet {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<String>::deserialize(d).map(|keys| KeySet { keys })
    }
}

/// A structured value in one variable, sent as compact JSON (contract type
/// `json`). Its schema is generated from `T` with `schemars`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Json<T>(pub T);

impl<T> Deref for Json<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Json<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Json)
    }
}

// ---------------------------------------------------------------------------
// File inputs reach the struct through figment as an opaque token: the loader
// reads and checks each file, parks the result here, and puts the token at
// the field's key. The type's Deserialize impl takes the result back out.

static NEXT: AtomicU64 = AtomicU64::new(1);
static PARKED: Mutex<Option<HashMap<u64, Box<dyn Any + Send>>>> = Mutex::new(None);
const TOKEN: &str = "\u{0}docuconf-file:";

pub(crate) fn park(v: Box<dyn Any + Send>) -> String {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    PARKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(id, v);
    format!("{TOKEN}{id}")
}

pub(crate) fn discard(token: &str) {
    if let Some(id) = token
        .strip_prefix(TOKEN)
        .and_then(|s| s.parse::<u64>().ok())
    {
        if let Some(m) = PARKED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            m.remove(&id);
        }
    }
}

fn unpark<'de, D: Deserializer<'de>, T: 'static>(d: D, what: &str) -> Result<T, D::Error> {
    let token = String::deserialize(d)?;
    let id = token
        .strip_prefix(TOKEN)
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| {
            D::Error::custom(format!(
                "a {what} is loaded by docuconf::load or docuconf::Loader, not deserialized directly"
            ))
        })?;
    let boxed = PARKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|m| m.remove(&id))
        .ok_or_else(|| D::Error::custom(format!("{what} was already taken")))?;
    boxed
        .downcast::<T>()
        .map(|b| *b)
        .map_err(|_| D::Error::custom(format!("docuconf: field type does not match the {what}")))
}

#[cfg(feature = "tls")]
/// A TLS key pair in the `kubernetes.io/tls` layout (contract type `tls`):
/// a directory holding `tls.crt`, `tls.key` and optionally `ca.crt`.
/// Checked at boot: the certificate parses, matches the key, is valid for
/// at least `min_remaining`, covers `dns_names`, uses an allowed key
/// algorithm and, with `require_ca`, chains to `ca.crt`.
pub struct TlsKeyPair {
    pub(crate) dir: PathBuf,
    pub(crate) chain: Vec<CertificateDer<'static>>,
    pub(crate) key: PrivateKeyDer<'static>,
    pub(crate) ca: Vec<CertificateDer<'static>>,
    pub(crate) not_after: SystemTime,
}

#[cfg(feature = "tls")]
impl TlsKeyPair {
    /// The directory the key pair was read from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    /// The certificate chain from `tls.crt`, leaf first.
    pub fn cert_chain(&self) -> &[CertificateDer<'static>] {
        &self.chain
    }
    /// The private key from `tls.key`.
    pub fn private_key(&self) -> PrivateKeyDer<'static> {
        self.key.clone_key()
    }
    /// The certificates in `ca.crt`, empty when there is none.
    pub fn ca_certificates(&self) -> &[CertificateDer<'static>] {
        &self.ca
    }
    /// When the leaf certificate expires.
    pub fn not_after(&self) -> SystemTime {
        self.not_after
    }
}

#[cfg(feature = "tls")]
impl fmt::Debug for TlsKeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsKeyPair")
            .field("dir", &self.dir)
            .field("certificates", &self.chain.len())
            .field("ca_certificates", &self.ca.len())
            .field(
                "not_after",
                &humantime::format_rfc3339_seconds(self.not_after).to_string(),
            )
            .field("key", &"***")
            .finish()
    }
}

#[cfg(feature = "tls")]
impl<'de> Deserialize<'de> for TlsKeyPair {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        unpark(d, "TLS key pair")
    }
}

#[cfg(feature = "tls")]
/// A PEM file of one or more CA certificates (contract type `caBundle`).
#[derive(Clone)]
pub struct CaBundle {
    pub(crate) path: PathBuf,
    pub(crate) certs: Vec<CertificateDer<'static>>,
}

#[cfg(feature = "tls")]
impl CaBundle {
    /// The file the bundle was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The certificates, in file order.
    pub fn certificates(&self) -> &[CertificateDer<'static>] {
        &self.certs
    }
}

#[cfg(feature = "tls")]
impl fmt::Debug for CaBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaBundle")
            .field("path", &self.path)
            .field("certificates", &self.certs.len())
            .finish()
    }
}

#[cfg(feature = "tls")]
impl<'de> Deserialize<'de> for CaBundle {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        unpark(d, "CA bundle")
    }
}

#[cfg(feature = "keystore")]
/// A PKCS#12 keystore (contract type `keystore`), opened at boot with the
/// password from its `password_var`.
pub struct Keystore {
    pub(crate) path: PathBuf,
    pub(crate) key: Option<PrivateKeyDer<'static>>,
    pub(crate) chain: Vec<CertificateDer<'static>>,
    pub(crate) trusted: Vec<CertificateDer<'static>>,
}

#[cfg(feature = "keystore")]
impl Keystore {
    /// The file the keystore was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The private key of the first key entry, if the store has one.
    pub fn private_key(&self) -> Option<PrivateKeyDer<'static>> {
        self.key.as_ref().map(|k| k.clone_key())
    }
    /// The certificate chain of the first key entry, leaf first.
    pub fn cert_chain(&self) -> &[CertificateDer<'static>] {
        &self.chain
    }
    /// Trusted certificate entries (a truststore's CAs).
    pub fn trusted_certificates(&self) -> &[CertificateDer<'static>] {
        &self.trusted
    }
}

#[cfg(feature = "keystore")]
impl fmt::Debug for Keystore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keystore")
            .field("path", &self.path)
            .field("certificates", &self.chain.len())
            .field("trusted", &self.trusted.len())
            .field("key", &self.key.as_ref().map(|_| "***"))
            .finish()
    }
}

#[cfg(feature = "keystore")]
impl<'de> Deserialize<'de> for Keystore {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        unpark(d, "keystore")
    }
}

/// A UTF-8 text file (contract type `text`), such as a licence key. Its
/// `Debug` output shows the path and length, never the text.
#[derive(Clone)]
pub struct TextFile {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
}

impl TextFile {
    /// The file the text was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The text, unmodified (a trailing newline is kept).
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Deref for TextFile {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl fmt::Debug for TextFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextFile")
            .field("path", &self.path)
            .field("len", &self.text.len())
            .finish()
    }
}

impl<'de> Deserialize<'de> for TextFile {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        unpark(d, "text file")
    }
}

/// Opaque bytes (contract type `binary`), such as a GeoIP database. Only
/// its size is checked.
#[derive(Clone)]
pub struct BinaryFile {
    pub(crate) path: PathBuf,
    pub(crate) bytes: Vec<u8>,
}

impl BinaryFile {
    /// The file the bytes were read from.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The file's contents.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Deref for BinaryFile {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for BinaryFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BinaryFile")
            .field("path", &self.path)
            .field("len", &self.bytes.len())
            .finish()
    }
}

impl<'de> Deserialize<'de> for BinaryFile {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        unpark(d, "binary file")
    }
}

/// A structured config file (contract type `config`) in JSON, YAML or
/// TOML, bound to the app's type `T`. The contract carries the JSON Schema
/// `schemars` generates for `T`, and the file is checked against it at boot.
#[derive(Clone, Debug)]
pub struct ConfigFile<T> {
    path: PathBuf,
    value: T,
}

impl<T> ConfigFile<T> {
    /// The file the value was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Unwraps the value.
    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T> Deref for ConfigFile<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

pub(crate) struct ConfigDoc {
    pub path: PathBuf,
    pub doc: serde_json::Value,
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for ConfigFile<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let doc: ConfigDoc = unpark(d, "config file")?;
        let value = T::deserialize(&doc.doc).map_err(D::Error::custom)?;
        Ok(ConfigFile {
            path: doc.path,
            value,
        })
    }
}
