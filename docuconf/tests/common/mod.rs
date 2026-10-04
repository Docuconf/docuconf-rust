//! Shared test fixtures: a declaration with one input of every kind, and
//! certificates generated with rcgen.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use docuconf::url::Url;
use docuconf::{
    BinaryFile, CaBundle, ConfigFile, Docuconf, DocuconfEnum, Json, Keystore, Secret, TextFile,
    TlsKeyPair,
};
use rcgen::{
    date_time_ymd, BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer,
    KeyPair,
};
use serde::Deserialize;

/// Gateway declares one input of every kind: each variable type and each
/// file type. It is exported to tests/golden/gateway.cue.
#[derive(Debug, Deserialize, Docuconf)]
pub struct Gateway {
    /// Minimum log level emitted.
    #[docuconf(default = "info", group = "logging")]
    pub log_level: LogLevel,

    /// Namespace the gateway runs in, for metrics labels.
    pub pod_namespace: String,

    /// Soft memory limit, in bytes.
    #[docuconf(env = "MEMORY_LIMIT", min = 1)]
    pub mem_limit: Option<i64>,

    /// Default per-client rate limits.
    #[docuconf(default = r#"{"perMinute":60}"#)]
    pub rate_limits: Json<RateLimits>,

    /// Password for the partner mTLS keystore.
    pub partner_keystore_password: Secret<String>,

    /// Primary Postgres connection string.
    #[docuconf(schemes("postgres", "postgresql"))]
    pub database_url: Secret<String>,

    /// HTTP listen port.
    #[docuconf(default = 8080, min = 1)]
    pub port: u16,

    /// Upstream request timeout.
    #[docuconf(default = "30s", min = "1s", max = "5m")]
    #[serde(with = "docuconf::humantime_serde")]
    pub request_timeout: Duration,

    /// CORS origins allowed to call the API.
    #[docuconf(min_items = 1, examples("https://app.example.com"))]
    pub allowed_origins: Vec<String>,

    /// Extra ports to listen on.
    #[docuconf(max_items = 4)]
    pub extra_ports: Option<Vec<u16>>,

    /// Stripe API base URL.
    #[docuconf(default = "https://api.stripe.com", schemes("https"))]
    pub stripe_api_base: Url,

    /// Fraction of requests traced.
    #[docuconf(default = 0.1, min = 0, max = 1)]
    pub trace_sample_ratio: f64,

    /// Serve the debug endpoints.
    #[docuconf(default = false)]
    pub debug: bool,

    /// Cloud region, such as eu-west-1.
    #[docuconf(
        pattern = "^[a-z]{2}-[a-z]+-[0-9]$",
        min_length = 4,
        max_length = 32,
        deprecated = "Read from the node's topology labels instead"
    )]
    pub region: Option<String>,

    /// API token for the metrics backend.
    #[docuconf(secret, min_length = 20)]
    pub metrics_token: Option<String>,

    pub cache: Cache,

    /// Routing table: path prefixes and their upstreams.
    #[docuconf(
        path = "/etc/gateway/routes/routes.yaml",
        path_env = "ROUTES_FILE",
        max_size = "64Ki"
    )]
    pub routes: ConfigFile<Routes>,

    /// Certificate the gateway serves HTTPS with.
    #[docuconf(
        name = "serving-tls",
        path = "/etc/gateway/tls",
        dns_names("gateway.internal", "api.example.com"),
        key_algorithms("ECDSA", "RSA"),
        min_remaining = "720h"
    )]
    pub serving_tls: TlsKeyPair,

    /// Private CAs the gateway trusts for upstream TLS.
    #[docuconf(path = "/etc/gateway/ca/bundle.pem", path_env = "SSL_CERT_FILE")]
    pub upstream_ca: Option<CaBundle>,

    /// Client certificate for mTLS to the partner API.
    #[docuconf(
        path = "/etc/gateway/partner/keystore.p12",
        password_var = "PARTNER_KEYSTORE_PASSWORD"
    )]
    pub partner_keystore: Option<Keystore>,

    /// Gateway licence key.
    #[docuconf(
        path = "/etc/gateway/license/license.key",
        pattern = "^[A-Z0-9]{5}(-[A-Z0-9]{5}){3}\\n?$"
    )]
    pub license: TextFile,

    /// GeoIP database for country-based routing.
    #[docuconf(path = "/data/geoip/GeoLite2-City.mmdb", max_size = "128Mi")]
    pub geoip: Option<BinaryFile>,
}

#[derive(Debug, Deserialize, Docuconf)]
pub struct Cache {
    /// Cache entry lifetime.
    #[docuconf(default = "5m")]
    #[serde(with = "docuconf::humantime_serde")]
    pub ttl: Duration,

    /// Maximum cached entries.
    #[docuconf(default = 1000)]
    pub size: u32,
}

#[derive(Debug, Deserialize, DocuconfEnum, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// The gateway's routing table file.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Routes {
    /// Routes in match order.
    #[schemars(length(min = 1))]
    pub routes: Vec<Route>,
}

/// Sends requests under a path prefix to an upstream.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// Path prefix the route matches.
    #[schemars(regex(pattern = "^/"))]
    pub r#match: String,
    /// Upstream base URL.
    #[schemars(regex(pattern = "^https?://"))]
    pub upstream: String,
    /// Per-request timeout, such as 5s.
    #[serde(default)]
    pub timeout: Option<String>,
}

/// Bounds each client's request rate.
#[derive(Debug, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RateLimits {
    /// Requests allowed per minute.
    #[schemars(range(min = 1))]
    pub per_minute: u32,
    /// Extra requests allowed in a burst.
    #[serde(default)]
    pub burst: Option<u32>,
}

// ---------------------------------------------------------------------------

/// 2026-06-01, inside the validity of every generated certificate.
pub fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_780_272_000)
}

pub fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

pub struct Ca {
    pub params: CertificateParams,
    pub key: KeyPair,
    pub pem: String,
}

pub fn ca(name: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.not_before = date_time_ymd(2025, 1, 1);
    params.not_after = date_time_ymd(2030, 1, 1);
    let pem = params.self_signed(&key).unwrap().pem();
    Ca { params, key, pem }
}

pub struct Leaf {
    pub cert_pem: String,
    pub key_pem: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

/// A leaf certificate for `names`, valid 2026-01-01 to 2027-01-01, signed
/// by `ca` or self-signed.
pub fn leaf(names: &[&str], ca: Option<&Ca>) -> Leaf {
    leaf_with(names, ca, KeyPair::generate().unwrap())
}

pub fn leaf_with(names: &[&str], ca: Option<&Ca>, key: KeyPair) -> Leaf {
    let mut params =
        CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    params.not_before = date_time_ymd(2026, 1, 1);
    params.not_after = date_time_ymd(2027, 1, 1);
    let cert = match ca {
        Some(ca) => {
            let issuer = Issuer::from_params(&ca.params, &ca.key);
            params.signed_by(&key, &issuer).unwrap()
        }
        None => params.self_signed(&key).unwrap(),
    };
    Leaf {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        cert_der: cert.der().to_vec(),
        key_der: key.serialize_der(),
    }
}

pub fn keystore_bytes(leaf: &Leaf, password: &str) -> Vec<u8> {
    use p12_keystore::{Certificate, KeyStore, KeyStoreEntry, PrivateKey, PrivateKeyChain};
    let mut ks = KeyStore::new();
    let chain = PrivateKeyChain::new(
        b"1".to_vec(),
        PrivateKey::from_der(&leaf.key_der).unwrap(),
        [Certificate::from_der(&leaf.cert_der).unwrap()],
    );
    ks.add_entry("partner", KeyStoreEntry::PrivateKeyChain(chain));
    ks.writer(password).write().unwrap()
}

/// A file root holding a valid set of files for Gateway, and the matching
/// environment.
pub struct World {
    pub dir: tempfile::TempDir,
    pub env: HashMap<String, String>,
    pub ca: Ca,
}

pub const LICENSE: &str = "ABCDE-12345-FGHIJ-67890\n";

pub const ROUTES: &str =
    "routes:\n  - match: /api\n    upstream: https://api.internal\n    timeout: 5s\n";

impl World {
    pub fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let ca = ca("Test CA");
        let w = World {
            env: HashMap::new(),
            dir,
            ca,
        };
        let l = leaf(&["gateway.internal", "api.example.com"], Some(&w.ca));
        w.write_tls(&l);
        w.write("/etc/gateway/ca/bundle.pem", w.ca.pem.as_bytes());
        w.write("/etc/gateway/routes/routes.yaml", ROUTES.as_bytes());
        w.write("/etc/gateway/license/license.key", LICENSE.as_bytes());
        w.write("/data/geoip/GeoLite2-City.mmdb", &[0u8, 1, 2, 3]);
        let partner = leaf(&["partner-client"], Some(&w.ca));
        w.write(
            "/etc/gateway/partner/keystore.p12",
            &keystore_bytes(&partner, "s3cret-pass"),
        );
        let mut w = w;
        for (k, v) in [
            ("DOCUCONF_FILE_ROOT", w.dir.path().to_str().unwrap()),
            ("POD_NAMESPACE", "edge"),
            ("PARTNER_KEYSTORE_PASSWORD", "s3cret-pass"),
            ("DATABASE_URL", "postgres://app:hunter2@db:5432/app"),
            (
                "ALLOWED_ORIGINS",
                r#"["https://app.example.com","https://admin.example.com"]"#,
            ),
        ] {
            w.env.insert(k.to_string(), v.to_string());
        }
        w
    }

    pub fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p.trim_start_matches('/'))
    }

    pub fn write(&self, p: &str, content: &[u8]) {
        let path = self.path(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    pub fn remove(&self, p: &str) {
        std::fs::remove_file(self.path(p)).unwrap();
    }

    pub fn write_tls(&self, l: &Leaf) {
        self.write("/etc/gateway/tls/tls.crt", l.cert_pem.as_bytes());
        self.write("/etc/gateway/tls/tls.key", l.key_pem.as_bytes());
    }

    pub fn set(&mut self, k: &str, v: &str) -> &mut Self {
        self.env.insert(k.to_string(), v.to_string());
        self
    }

    pub fn unset(&mut self, k: &str) -> &mut Self {
        self.env.remove(k);
        self
    }

    pub fn loader<C: Docuconf + serde::de::DeserializeOwned>(&self) -> docuconf::Loader<C> {
        docuconf::Loader::<C>::new()
            .env(self.env.clone())
            .now(now())
            .termination_log(false)
    }

    pub fn load(&self) -> Result<Gateway, docuconf::Error> {
        self.loader::<Gateway>().load()
    }
}

pub fn root(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}
