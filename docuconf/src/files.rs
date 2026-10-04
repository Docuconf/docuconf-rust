//! Boot-time checks of file inputs (SPEC §11.2 item 7).

use std::any::Any;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use figment::providers::{Format, Json, Toml, Yaml};
use figment::Figment;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};

use crate::decl::{go, FileDecl, FileKind};
use crate::error::{Code, Violation};
use crate::types::{BinaryFile, CaBundle, ConfigDoc, Keystore, TextFile, TlsKeyPair};
use crate::value::Typed;

pub(crate) struct FileCx<'a> {
    pub root: Option<PathBuf>,
    pub env: &'a HashMap<String, String>,
    pub now: SystemTime,
    /// Resolved variable values by name, for keystore passwords.
    pub values: &'a HashMap<String, Typed>,
}

pub(crate) enum Outcome {
    Absent,
    Loaded(Box<dyn Any + Send>),
    Failed(Vec<Violation>),
}

struct Ctx<'a> {
    decl: &'a FileDecl,
    path: PathBuf,
    cx: &'a FileCx<'a>,
}

impl Ctx<'_> {
    fn v(&self, code: Code, message: impl Into<String>) -> Violation {
        Violation {
            input: self.decl.name.clone(),
            code,
            message: message.into(),
        }
    }

    /// Reads a file. `Ok(None)` when it is absent and not required.
    fn read(&self, path: &Path, required: bool) -> Result<Option<Vec<u8>>, Violation> {
        let shown = path.display();
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return if required {
                    Err(self.v(Code::FileMissing, format!("{shown} does not exist")))
                } else {
                    Ok(None)
                };
            }
            Err(e) => return Err(self.unreadable(path, &e)),
        };
        if meta.is_dir() {
            return Err(self.v(
                Code::FileUnreadable,
                format!("{shown} is a directory, not a file"),
            ));
        }
        if let Some(max) = self.decl.max_size {
            if meta.len() > max {
                return Err(self.v(
                    Code::FileTooLarge,
                    format!("{shown} is {} bytes, above maxSize {max}", meta.len()),
                ));
            }
        }
        std::fs::read(path)
            .map(Some)
            .map_err(|e| self.unreadable(path, &e))
    }

    fn unreadable(&self, path: &Path, e: &std::io::Error) -> Violation {
        let hint = if e.kind() == ErrorKind::PermissionDenied {
            " (secret volumes are owned by root: a non-root container needs the pod's fsGroup)"
        } else {
            ""
        };
        self.v(
            Code::FileUnreadable,
            format!("cannot read {}: {e}{hint}", path.display()),
        )
    }
}

/// Where a file input is read from: the `pathEnv` variable when set, else
/// the declared path, under `DOCUCONF_FILE_ROOT` when that is set.
pub(crate) fn resolve_path(decl: &FileDecl, cx: &FileCx) -> PathBuf {
    let p = decl
        .path_env
        .as_ref()
        .and_then(|e| cx.env.get(e))
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| decl.path.clone());
    match &cx.root {
        Some(root) if p.starts_with('/') => root.join(p.trim_start_matches('/')),
        _ => PathBuf::from(p),
    }
}

pub(crate) fn check(decl: &FileDecl, cx: &FileCx) -> Outcome {
    let c = Ctx {
        decl,
        path: resolve_path(decl, cx),
        cx,
    };
    let r = match &decl.kind {
        FileKind::Tls => tls(&c),
        FileKind::CaBundle => ca_bundle(&c),
        FileKind::Keystore => keystore(&c),
        FileKind::Text => text(&c),
        FileKind::Binary => binary(&c),
        FileKind::Config { schema, bind } => config(&c, schema, *bind),
    };
    match r {
        Ok(Some(b)) => Outcome::Loaded(b),
        Ok(None) => Outcome::Absent,
        Err(v) => Outcome::Failed(v),
    }
}

type R = Result<Option<Box<dyn Any + Send>>, Vec<Violation>>;

fn binary(c: &Ctx) -> R {
    let Some(bytes) = c.read(&c.path, c.decl.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    Ok(Some(Box::new(BinaryFile {
        path: c.path.clone(),
        bytes,
    })))
}

fn text(c: &Ctx) -> R {
    let d = c.decl;
    let Some(bytes) = c.read(&c.path, d.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Err(vec![c.v(Code::FileMalformed, "is not valid UTF-8 text")]);
    };
    let mut viols = Vec::new();
    let n = text.chars().count() as u64;
    if let Some(lo) = d.min_length {
        if n < lo {
            viols.push(c.v(
                Code::OutOfRange,
                format!("is {n} characters, below minLength {lo}"),
            ));
        }
    }
    if let Some(hi) = d.max_length {
        if n > hi {
            viols.push(c.v(
                Code::OutOfRange,
                format!("is {n} characters, above maxLength {hi}"),
            ));
        }
    }
    if let Some((p, re)) = &d.pattern {
        if !re.is_match(&text) {
            viols.push(c.v(Code::PatternMismatch, format!("does not match pattern {p}")));
        }
    }
    if !viols.is_empty() {
        return Err(viols);
    }
    Ok(Some(Box::new(TextFile {
        path: c.path.clone(),
        text,
    })))
}

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

fn config(c: &Ctx, schema: &serde_json::Value, bind: crate::decl::BindFn) -> R {
    let d = c.decl;
    let Some(bytes) = c.read(&c.path, d.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    let bytes = bytes.strip_prefix(BOM).unwrap_or(&bytes);
    let format = d.format.as_deref().unwrap_or("json");
    let reason = |e: &dyn std::fmt::Display| {
        if d.secret {
            String::new()
        } else {
            format!(": {e}")
        }
    };
    let Ok(src) = std::str::from_utf8(bytes) else {
        return Err(vec![c.v(Code::FileMalformed, "is not valid UTF-8")]);
    };
    // Parse with figment's own providers, the host's parsers.
    let fig = match format {
        "yaml" => Figment::from(Yaml::string(src)),
        "toml" => Figment::from(Toml::string(src)),
        _ => Figment::from(Json::string(src)),
    };
    let doc: serde_json::Value = match fig.extract() {
        Ok(v) => v,
        Err(e) => {
            let what = format.to_uppercase();
            return Err(vec![c.v(
                Code::FileMalformed,
                format!("is not a valid {what} document{}", reason(&e)),
            )]);
        }
    };
    let viols: Vec<Violation> = crate::schema::validate(schema, &doc, d.secret)
        .into_iter()
        .map(|m| c.v(Code::SchemaMismatch, m))
        .collect();
    if !viols.is_empty() {
        return Err(viols);
    }
    if let Err(e) = bind(&doc) {
        return Err(vec![c.v(
            Code::SchemaMismatch,
            format!("does not bind to the app's type{}", reason(&e)),
        )]);
    }
    Ok(Some(Box::new(ConfigDoc {
        path: c.path.clone(),
        doc,
    })))
}

fn parse_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String> {
    let mut out = Vec::new();
    for (i, item) in CertificateDer::pem_slice_iter(pem).enumerate() {
        let der = item.map_err(|e| format!("certificate {i} is not valid PEM: {e}"))?;
        if let Err(e) = x509_parser::parse_x509_certificate(&der) {
            return Err(format!("certificate {i} does not parse: {e}"));
        }
        out.push(der);
    }
    Ok(out)
}

fn rfc3339(t: SystemTime) -> String {
    humantime::format_rfc3339_seconds(t).to_string()
}

fn asn1_to_system(t: &x509_parser::time::ASN1Time) -> SystemTime {
    let secs = t.timestamp();
    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
    }
}

fn key_algorithm(cert: &x509_parser::certificate::X509Certificate) -> String {
    let oid = cert.public_key().algorithm.algorithm.to_id_string();
    match oid.as_str() {
        "1.2.840.113549.1.1.1" => "RSA".into(),
        "1.2.840.10045.2.1" => "ECDSA".into(),
        "1.3.101.112" => "Ed25519".into(),
        other => other.to_string(),
    }
}

fn covers(
    ee: &webpki::EndEntityCert,
    cert: &x509_parser::certificate::X509Certificate,
    name: &str,
) -> bool {
    if let Ok(sn) = ServerName::try_from(name) {
        return ee.verify_is_valid_for_subject_name(&sn).is_ok();
    }
    // A name the contract writes as a wildcard must appear as is.
    cert.subject_alternative_name()
        .ok()
        .flatten()
        .map(|san| {
            san.value.general_names.iter().any(|g| {
                matches!(g, x509_parser::extensions::GeneralName::DNSName(d) if d.eq_ignore_ascii_case(name))
            })
        })
        .unwrap_or(false)
}

fn tls(c: &Ctx) -> R {
    let d = c.decl;
    let Some(crt) = c
        .read(&c.path.join("tls.crt"), d.required)
        .map_err(|v| vec![v])?
    else {
        return Ok(None);
    };
    let key_pem = c
        .read(&c.path.join("tls.key"), true)
        .map_err(|v| vec![v])?
        .unwrap_or_default();

    let chain = parse_certs(&crt)
        .map_err(|e| vec![c.v(Code::CertificateInvalid, format!("tls.crt: {e}"))])?;
    if chain.is_empty() {
        return Err(vec![
            c.v(Code::CertificateInvalid, "tls.crt holds no PEM certificate")
        ]);
    }
    let Ok(key) = PrivateKeyDer::from_pem_slice(&key_pem) else {
        return Err(vec![c.v(
            Code::FileMalformed,
            "tls.key holds no parseable PEM private key",
        )]);
    };
    let Ok(signing) = rustls::crypto::ring::sign::any_supported_type(&key) else {
        return Err(vec![c.v(
            Code::FileMalformed,
            "tls.key holds a private key of an unsupported type",
        )]);
    };
    let certified = rustls::sign::CertifiedKey::new(chain.clone(), signing);
    if let Err(rustls::Error::InconsistentKeys(rustls::InconsistentKeys::KeyMismatch)) =
        certified.keys_match()
    {
        return Err(vec![c.v(
            Code::KeyMismatch,
            "tls.key does not match the certificate in tls.crt",
        )]);
    }

    let (_, leaf) = x509_parser::parse_x509_certificate(&chain[0])
        .map_err(|e| vec![c.v(Code::CertificateInvalid, format!("tls.crt: {e}"))])?;
    let mut viols = Vec::new();
    let now = c.cx.now;
    let not_before = asn1_to_system(&leaf.validity().not_before);
    let not_after = asn1_to_system(&leaf.validity().not_after);
    if now < not_before {
        viols.push(c.v(
            Code::CertificateInvalid,
            format!("certificate is not valid until {}", rfc3339(not_before)),
        ));
    } else if now >= not_after {
        viols.push(c.v(
            Code::CertificateInvalid,
            format!("certificate expired at {}", rfc3339(not_after)),
        ));
    } else if let Some(min) = d.min_remaining {
        let left = not_after.duration_since(now).unwrap_or_default();
        if left < min {
            let left = Duration::from_secs(left.as_secs() / 60 * 60);
            viols.push(c.v(
                Code::CertificateExpiring,
                format!(
                    "certificate expires at {}, in {}, less than minRemaining {}",
                    rfc3339(not_after),
                    go(left),
                    go(min)
                ),
            ));
        }
    }

    let ee = webpki::EndEntityCert::try_from(&chain[0]);
    match &ee {
        Ok(ee) => {
            for name in &d.dns_names {
                if !covers(ee, &leaf, name) {
                    viols.push(c.v(
                        Code::CertificateNameMismatch,
                        format!("certificate does not cover {name}"),
                    ));
                }
            }
        }
        Err(e) => viols.push(c.v(
            Code::CertificateInvalid,
            format!("tls.crt: leaf certificate is not usable: {e:?}"),
        )),
    }
    if !d.key_algorithms.is_empty() {
        let alg = key_algorithm(&leaf);
        if !d.key_algorithms.contains(&alg) {
            viols.push(c.v(
                Code::CertificateInvalid,
                format!(
                    "certificate key algorithm {alg} is not one of {}",
                    d.key_algorithms.join(", ")
                ),
            ));
        }
    }

    let mut ca = Vec::new();
    match c.read(&c.path.join("ca.crt"), d.require_ca) {
        Err(v) => viols.push(v),
        Ok(None) => {}
        Ok(Some(pem)) => match parse_certs(&pem) {
            Err(e) => viols.push(c.v(Code::CertificateInvalid, format!("ca.crt: {e}"))),
            Ok(certs) if certs.is_empty() => {
                viols.push(c.v(Code::CertificateInvalid, "ca.crt holds no PEM certificate"))
            }
            Ok(certs) => ca = certs,
        },
    }
    if d.require_ca && !ca.is_empty() {
        if let Ok(ee) = &ee {
            let anchors: Vec<_> = ca
                .iter()
                .filter_map(|der| webpki::anchor_from_trusted_cert(der).ok())
                .map(|a| a.to_owned())
                .collect();
            let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
            if let Err(e) = ee.verify_for_usage(
                webpki::ALL_VERIFICATION_ALGS,
                &anchors,
                &chain[1..],
                UnixTime::since_unix_epoch(since),
                webpki::KeyUsage::server_auth(),
                None,
                None,
            ) {
                viols.push(c.v(
                    Code::CertificateInvalid,
                    format!("certificate does not chain to ca.crt: {e:?}"),
                ));
            }
        }
    }
    if !viols.is_empty() {
        return Err(viols);
    }
    Ok(Some(Box::new(TlsKeyPair {
        dir: c.path.clone(),
        chain,
        key,
        ca,
        not_after,
    })))
}

fn ca_bundle(c: &Ctx) -> R {
    let d = c.decl;
    let Some(pem) = c.read(&c.path, d.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    let certs = parse_certs(&pem).map_err(|e| vec![c.v(Code::CertificateInvalid, e)])?;
    if certs.is_empty() {
        return Err(vec![c.v(Code::FileMalformed, "holds no PEM certificates")]);
    }
    if (certs.len() as u64) < d.min_certificates {
        return Err(vec![c.v(
            Code::FileMalformed,
            format!(
                "holds {} certificate{}, need at least {}",
                certs.len(),
                if certs.len() == 1 { "" } else { "s" },
                d.min_certificates
            ),
        )]);
    }
    Ok(Some(Box::new(CaBundle {
        path: c.path.clone(),
        certs,
    })))
}

fn keystore(c: &Ctx) -> R {
    let d = c.decl;
    let Some(data) = c.read(&c.path, d.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    let password = match &d.password_var {
        None => String::new(),
        Some(var) => match c.cx.values.get(var) {
            Some(Typed::Str(p)) => p.clone(),
            _ => {
                return Err(vec![c.v(
                    Code::KeystoreUnreadable,
                    format!("cannot open: its password variable {var} is not set"),
                )])
            }
        },
    };
    let ks = p12_keystore::KeyStore::from_pkcs12(
        &data,
        &password,
        p12_keystore::Pkcs12ImportPolicy::Relaxed,
    )
    .map_err(|e| {
        vec![c.v(
            Code::KeystoreUnreadable,
            format!(
                "cannot open with the password from {}: {e}",
                d.password_var
                    .as_deref()
                    .unwrap_or("(no password variable)")
            ),
        )]
    })?;
    let mut key = None;
    let mut chain = Vec::new();
    if let Some((_, kc)) = ks.private_key_chain() {
        key = Some(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            kc.key().as_der().to_vec(),
        )));
        chain = kc
            .certs()
            .iter()
            .map(|c| CertificateDer::from(c.as_der().to_vec()))
            .collect();
    }
    let trusted: Vec<CertificateDer<'static>> = ks
        .entries()
        .filter_map(|(_, e)| match e {
            p12_keystore::KeyStoreEntry::Certificate(c) => {
                Some(CertificateDer::from(c.as_der().to_vec()))
            }
            _ => None,
        })
        .collect();
    if key.is_none() && trusted.is_empty() {
        return Err(vec![c.v(
            Code::KeystoreUnreadable,
            "holds no key entry and no trusted certificate",
        )]);
    }
    if let Some(leaf) = chain.first() {
        if let Ok((_, cert)) = x509_parser::parse_x509_certificate(leaf) {
            let not_after = asn1_to_system(&cert.validity().not_after);
            if c.cx.now >= not_after {
                return Err(vec![c.v(
                    Code::CertificateInvalid,
                    format!("keystore certificate expired at {}", rfc3339(not_after)),
                )]);
            }
        }
    }
    Ok(Some(Box::new(Keystore {
        path: c.path.clone(),
        key,
        chain,
        trusted,
    })))
}
