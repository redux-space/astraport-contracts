//! Cryptographic signing of audit log exports for verification.
//!
//! Provides `SignedExport` — a batch of audit entries wrapped with a
//! SHA-256 digest. Off-chain services sign the digest with ed25519 and
//! attach the signature. On-chain or off-chain verifiers can recompute
//! the digest from the raw entries and verify the signature against a
//! known public key.
//!
//! The signing flow (off-chain):
//!
//! 1. Call `compute_digest` with a batch of entries.
//! 2. Sign the resulting digest with ed25519 (off-chain).
//! 3. Bundle the entries, digest, and signature into `SignedExport`.
//!
//! Verification (on-chain or off-chain):
//!
//! 1. Re-derive the SHA-256 digest from `SignedExport.entries`.
//! 2. Verify the ed25519 signature against the digest and the signer's public key.
//!
//! Note: Soroban's `ed25519_verify` panics on invalid signatures, aborting
//! the transaction. This is the secure default for smart contracts. Callers
//! who need a boolean result should use `compute_digest` and verify off-chain.

use soroban_sdk::{contracttype, Bytes, BytesN, Env, Vec};

use crate::records::AuditLog;

/// A deterministically-ordered, signed export of audit log entries.
///
/// `entries` is the raw `Vec<AuditLog>` (from a `query` call).
/// `digest` is `SHA-256(serialize(entries))`.
/// `signature` is an ed25519 signature over `digest` (produced off-chain).
/// `signer` is the public key (32 bytes) that produced the signature.
#[contracttype]
#[derive(Debug, Clone)]
pub struct SignedExport {
    /// The raw audit entries.
    pub entries: Vec<AuditLog>,
    /// SHA-256 digest of the serialized entries.
    pub digest: BytesN<32>,
    /// ed25519 signature over `digest` (64 bytes, produced off-chain).
    pub signature: BytesN<64>,
    /// Ed25519 public key of the signer.
    pub signer: BytesN<32>,
}

/// Serialize a batch of audit log entries into a deterministic byte payload.
///
/// Layout per entry (all big-endian):
/// - `seq: u64` (8 bytes)
/// - `timestamp: u64` (8 bytes)
/// - `event_type as u32` (4 bytes)
/// - `permissions: u32` (4 bytes)
/// - `hash: BytesN<32>` (32 bytes)
///
/// We serialize only the immutable, compact fields so the payload is
/// compact and deterministic. The full entry (including strings, state
/// snapshots, etc.) is included in `SignedExport.entries` for off-chain
/// consumers.
pub fn serialize_entries(env: &Env, entries: &Vec<AuditLog>) -> Bytes {
    let mut buf = Bytes::new(env);
    // Length prefix (number of entries).
    let len = entries.len();
    buf.append(&Bytes::from_array(env, &len.to_be_bytes()));
    for entry in entries.iter() {
        buf.append(&Bytes::from_array(env, &entry.seq.to_be_bytes()));
        buf.append(&Bytes::from_array(env, &entry.timestamp.to_be_bytes()));
        buf.append(&Bytes::from_array(
            env,
            &(entry.event_type as u32).to_be_bytes(),
        ));
        buf.append(&Bytes::from_array(env, &entry.permissions.to_be_bytes()));
        buf.append(&Bytes::from_array(env, &entry.hash.to_array()));
    }
    buf
}

/// Compute the SHA-256 digest of a serialized entry payload.
///
/// This is the value that gets signed off-chain and verified on-chain.
pub fn compute_digest(env: &Env, entries: &Vec<AuditLog>) -> BytesN<32> {
    let payload = serialize_entries(env, entries);
    env.crypto().sha256(&payload).into()
}

/// Verify a [`SignedExport`] against a known public key.
///
/// This function verifies that:
/// 1. The public key matches `export.signer`.
/// 2. The digest matches `SHA-256(serialize(entries))`.
/// 3. The signature is valid for the digest under the public key.
///
/// # Panics
///
/// Panics (aborting the transaction) if the signature is invalid.
/// This is the secure default for Soroban contracts — callers who need
/// a boolean result should use `compute_digest` and verify off-chain.
pub fn verify_export(env: &Env, export: &SignedExport, public_key: &BytesN<32>) {
    // Check that the public key matches the one in the export.
    assert!(
        export.signer == *public_key,
        "signer public key does not match provided key"
    );
    // Recompute the digest from the entries.
    let expected_digest = compute_digest(env, &export.entries);
    assert!(
        expected_digest == export.digest,
        "digest mismatch: entries may have been tampered with"
    );
    // Verify the ed25519 signature (panics on failure).
    let digest_bytes: Bytes = export.digest.clone().into();
    let sig_bytes: BytesN<64> = export.signature.clone();
    env.crypto()
        .ed25519_verify(public_key, &digest_bytes, &sig_bytes);
}
