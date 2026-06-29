//! Shared Ed25519 *message construction* for the Linux appcast manifest.
//!
//! Used by BOTH the in-app verifier (`ui::updater_linux`) and the offline signer
//! (`bin/cc-sign-manifest`, which pulls this file in via `#[path]`) so the two can never drift.
//!
//! ## Domain separation
//! The Ed25519 key that signs the Linux manifest is the *same key* that Sparkle (macOS) and
//! WinSparkle (Windows) use to sign their update enclosures (see the release pipeline). Reusing
//! one key across feeds means a signature minted in one context is, in principle, a valid
//! Ed25519 signature in another. To make a Linux-manifest signature valid *only* as a
//! Linux-manifest signature, we don't sign the raw manifest bytes — we sign
//! `APPCAST_DOMAIN_PREFIX || manifest_bytes`. A signature over a Sparkle `.zip`/`.exe` (or any
//! other context) therefore can never validate here, and vice-versa.
//!
//! This is a pure, platform-neutral helper (no I/O, no platform deps) so the signer binary can
//! compile it on any host.

/// Domain-separation tag prepended to the manifest bytes before signing/verifying. Bump the
/// trailing version if the signed-message construction ever changes (it is part of the contract
/// between signer and verifier).
pub const APPCAST_DOMAIN_PREFIX: &[u8] = b"crowd-cast/linux-appcast/v1\n";

/// The exact byte string that gets Ed25519-signed and verified: the domain tag followed by the
/// raw manifest bytes. Both the signer and `updater_linux` MUST feed this to sign/verify.
pub fn signing_message(manifest_bytes: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(APPCAST_DOMAIN_PREFIX.len() + manifest_bytes.len());
    msg.extend_from_slice(APPCAST_DOMAIN_PREFIX);
    msg.extend_from_slice(manifest_bytes);
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_message_is_prefix_then_body() {
        let body = br#"{"version":"1.0.4"}"#;
        let msg = signing_message(body);
        assert!(msg.starts_with(APPCAST_DOMAIN_PREFIX));
        assert_eq!(&msg[APPCAST_DOMAIN_PREFIX.len()..], body);
        // Empty body still carries the domain tag (so an empty manifest can't masquerade as raw).
        assert_eq!(signing_message(b""), APPCAST_DOMAIN_PREFIX);
    }
}
