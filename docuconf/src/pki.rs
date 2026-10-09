//! Checks of the PKI file inputs: TLS key pairs and CA bundles (cargo
//! feature `tls`) and PKCS#12 keystores (feature `keystore`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls_pki_types::pem::PemObject;
#[cfg(feature = "keystore")]
use rustls_pki_types::PrivatePkcs8KeyDer;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

use crate::decl::go;
use crate::error::Code;
use crate::files::{Ctx, R};
#[cfg(feature = "keystore")]
use crate::types::Keystore;
use crate::types::{CaBundle, TlsKeyPair};
#[cfg(feature = "keystore")]
use crate::value::Typed;

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

pub(crate) fn tls(c: &Ctx) -> R {
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
            c.v(Code::FileMalformed, "tls.crt holds no PEM certificate")
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

pub(crate) fn ca_bundle(c: &Ctx) -> R {
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

#[cfg(feature = "keystore")]
pub(crate) fn keystore(c: &Ctx) -> R {
    let d = c.decl;
    let Some(data) = c.read(&c.path, d.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    let password = match &d.password_var {
        None => String::new(),
        // An unset password variable is an empty password (SPEC §11.2
        // item 7): a keystore may be written without one.
        Some(var) => match c.cx.values.get(var) {
            Some(Typed::Str(p)) => p.clone(),
            _ => String::new(),
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
