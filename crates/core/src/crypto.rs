//! Cryptographic utilities for password hashing and credential encryption.
//!
//! Uses bcrypt for password hashing and AES-256-GCM for symmetric encryption
//! of stored credentials (SVN passwords, tokens, etc.).

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use rand::RngCore;

use crate::db::Database;
use crate::errors::DatabaseError;

/// Errors from cryptographic operations.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("bcrypt error: {0}")]
    BcryptError(String),

    #[error("encryption error: {0}")]
    EncryptionError(String),

    #[error("decryption error: {0}")]
    DecryptionError(String),

    #[error("invalid key length")]
    InvalidKeyLength,

    #[error("base64 decode error: {0}")]
    Base64Error(String),

    #[error("database error: {0}")]
    DatabaseError(#[from] DatabaseError),
}

/// Encrypt a credential value using AES-256-GCM.
/// Returns `(base64_ciphertext, base64_nonce)`.
pub fn encrypt_credential(plaintext: &str, key: &[u8; 32]) -> Result<(String, String), CryptoError> {
    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| CryptoError::EncryptionError(e.to_string()))?;

    Ok((
        base64_encode(&ciphertext),
        base64_encode(&nonce_bytes),
    ))
}

/// Decrypt a credential value.
pub fn decrypt_credential(
    ciphertext_b64: &str,
    nonce_b64: &str,
    key: &[u8; 32],
) -> Result<String, CryptoError> {
    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);

    let ciphertext = base64_decode(ciphertext_b64)?;
    let nonce_bytes = base64_decode(nonce_b64)?;

    if nonce_bytes.len() != 12 {
        return Err(CryptoError::DecryptionError("invalid nonce length".into()));
    }

    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|e| CryptoError::DecryptionError(e.to_string()))?;

    String::from_utf8(plaintext)
        .map_err(|e| CryptoError::DecryptionError(format!("invalid UTF-8: {}", e)))
}

/// Hash a password using bcrypt.
pub fn hash_password(password: &str) -> Result<String, CryptoError> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST)
        .map_err(|e| CryptoError::BcryptError(e.to_string()))
}

/// Verify a password against a bcrypt hash.
pub fn verify_password(password: &str, hash: &str) -> Result<bool, CryptoError> {
    bcrypt::verify(password, hash).map_err(|e| CryptoError::BcryptError(e.to_string()))
}

/// Get or generate the server encryption key.
///
/// First checks the `REPOSYNC_ENCRYPTION_KEY` environment variable (hex-encoded 32 bytes).
/// If not set, reads from `kv_state` table. If not stored yet, generates a new random
/// key and persists it.
pub fn get_or_create_encryption_key(db: &Database) -> Result<[u8; 32], CryptoError> {
    const KV_KEY: &str = "encryption_key";

    // 1. Check env var
    if let Ok(hex_key) = std::env::var("REPOSYNC_ENCRYPTION_KEY") {
        let bytes = hex::decode(hex_key.trim())
            .map_err(|_| CryptoError::InvalidKeyLength)?;
        if bytes.len() != 32 {
            return Err(CryptoError::InvalidKeyLength);
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // 2. Check DB
    if let Some(hex_key) = db.get_state(KV_KEY)? {
        let bytes = hex::decode(&hex_key).map_err(|_| CryptoError::InvalidKeyLength)?;
        if bytes.len() != 32 {
            return Err(CryptoError::InvalidKeyLength);
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // 3. Generate and store
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    let hex_key = hex::encode(key);
    db.set_state(KV_KEY, &hex_key)?;
    tracing::info!("generated new server encryption key (stored in database)");

    Ok(key)
}

// ---------------------------------------------------------------------------
// Base64 helpers (using the `base64` crate)
// ---------------------------------------------------------------------------

fn base64_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, CryptoError> {
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| CryptoError::Base64Error(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_roundtrip() {
        let data = b"hello world";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 32];
        let plaintext = "my-secret-password";
        let (ct, nonce) = encrypt_credential(plaintext, &key).unwrap();
        let decrypted = decrypt_credential(&ct, &nonce, &key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_password_hash_verify() {
        let password = "test-password-123";
        let hash = hash_password(password).unwrap();
        assert!(verify_password(password, &hash).unwrap());
        assert!(!verify_password("wrong-password", &hash).unwrap());
    }

    #[test]
    fn test_get_or_create_encryption_key() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();

        let key1 = get_or_create_encryption_key(&db).unwrap();
        let key2 = get_or_create_encryption_key(&db).unwrap();
        assert_eq!(key1, key2, "should return same key on second call");
    }
}
