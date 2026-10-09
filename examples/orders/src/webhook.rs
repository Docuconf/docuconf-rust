//! Checks the signature on incoming payment webhooks against the key set
//! in WEBHOOK_KEYS.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Reports whether `signature`, the hex-encoded HMAC-SHA256 of `body`, was
/// made with any of `keys`. Accepting every key in the set is what lets a
/// key be rotated: during the overlap the old and the new key both work.
pub fn verify(keys: &[String], body: &[u8], signature: &str) -> bool {
    let Ok(got) = hex::decode(signature) else {
        return false;
    };
    let mut ok = false;
    for key in keys {
        let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("any key length");
        mac.update(body);
        // A constant-time comparison, and every key is checked, so the time
        // taken does not say which one matched.
        ok |= mac.verify_slice(&got).is_ok();
    }
    ok
}

/// The hex-encoded HMAC-SHA256 of `body` under `key`, as a sender makes it.
#[cfg(test)]
pub fn sign(key: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}
