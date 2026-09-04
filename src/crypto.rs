use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::Zeroizing;

pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
const CANARY_PLAINTEXT: &[u8] = b"INR-CANARY-v1";

pub type DerivedKey = Zeroizing<[u8; 32]>;

/// Derives a 32-byte key from a master password using Argon2id.
/// Params match OWASP's current minimum recommendation for Argon2id
/// (19 MiB memory, 2 iterations, 1 lane) - deliberately not tunable,
/// since this is a single-user local CLI, not a multi-tenant service.
pub fn derive_key(password: &str, salt: &[u8; SALT_LEN]) -> Result<DerivedKey> {
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default());
    let mut out = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(password.as_bytes(), salt, out.as_mut())
        .map_err(|e| anyhow!("key derivation failed: {e}"))?;
    Ok(out)
}

pub fn random_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).expect("OS RNG unavailable");
    salt
}

fn random_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).expect("OS RNG unavailable");
    nonce
}

/// Encrypts `plaintext` under `key`, returning (nonce, ciphertext).
pub fn encrypt(key: &DerivedKey, plaintext: &[u8]) -> Result<([u8; NONCE_LEN], Vec<u8>)> {
    let key_array =
        Key::<Aes256Gcm>::try_from(key.as_slice()).map_err(|_| anyhow!("invalid key length"))?;
    let cipher = Aes256Gcm::new(&key_array);
    let nonce_bytes = random_nonce();
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| anyhow!("encryption failed"))?;
    Ok((nonce_bytes, ciphertext))
}

/// Decrypts `ciphertext` under `key` and `nonce`. Fails (auth tag mismatch)
/// if the key is wrong or the data was tampered with.
pub fn decrypt(key: &DerivedKey, nonce: &[u8], ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if nonce.len() != NONCE_LEN {
        return Err(anyhow!("invalid nonce length"));
    }
    let key_array =
        Key::<Aes256Gcm>::try_from(key.as_slice()).map_err(|_| anyhow!("invalid key length"))?;
    let cipher = Aes256Gcm::new(&key_array);
    let nonce =
        Nonce::try_from(nonce).map_err(|_| anyhow!("invalid nonce length"))?;
    let plaintext = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| anyhow!("wrong password or corrupted data"))?;
    Ok(Zeroizing::new(plaintext))
}

/// Creates a canary: a known plaintext encrypted under the freshly-derived
/// key, stored alongside the salt so a later password attempt can be
/// verified by trying to decrypt it (AES-GCM's auth tag makes a wrong-key
/// decrypt fail cleanly instead of producing garbage).
pub fn create_canary(key: &DerivedKey) -> Result<([u8; NONCE_LEN], Vec<u8>)> {
    encrypt(key, CANARY_PLAINTEXT)
}

pub fn verify_canary(key: &DerivedKey, nonce: &[u8], ciphertext: &[u8]) -> bool {
    match decrypt(key, nonce, ciphertext) {
        Ok(plaintext) => plaintext.as_slice() == CANARY_PLAINTEXT,
        Err(_) => false,
    }
}

/// Reads and validates a master password against the stored canary,
/// returning the derived key on success.
pub fn unlock(password: &str, salt: &[u8], canary_nonce: &[u8], canary_cipher: &[u8]) -> Result<DerivedKey> {
    let salt: [u8; SALT_LEN] = salt
        .try_into()
        .context("stored salt has unexpected length")?;
    let key = derive_key(password, &salt)?;
    if verify_canary(&key, canary_nonce, canary_cipher) {
        Ok(key)
    } else {
        Err(anyhow!("wrong password"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canary_roundtrip_correct_password() {
        let salt = random_salt();
        let key = derive_key("correct horse battery staple", &salt).unwrap();
        let (nonce, cipher) = create_canary(&key).unwrap();
        assert!(verify_canary(&key, &nonce, &cipher));
    }

    #[test]
    fn canary_rejects_wrong_password() {
        let salt = random_salt();
        let key = derive_key("correct horse battery staple", &salt).unwrap();
        let (nonce, cipher) = create_canary(&key).unwrap();

        let wrong_key = derive_key("incorrect horse", &salt).unwrap();
        assert!(!verify_canary(&wrong_key, &nonce, &cipher));
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let salt = random_salt();
        let key = derive_key("pw", &salt).unwrap();
        let (nonce, cipher) = encrypt(&key, b"top secret value").unwrap();
        let plain = decrypt(&key, &nonce, &cipher).unwrap();
        assert_eq!(plain.as_slice(), b"top secret value");
    }

    #[test]
    fn decrypt_wrong_key_fails() {
        let salt = random_salt();
        let key = derive_key("pw", &salt).unwrap();
        let (nonce, cipher) = encrypt(&key, b"top secret value").unwrap();

        let other_key = derive_key("other", &salt).unwrap();
        assert!(decrypt(&other_key, &nonce, &cipher).is_err());
    }

    #[test]
    fn unlock_full_flow() {
        let salt = random_salt();
        let key = derive_key("hunter2", &salt).unwrap();
        let (canary_nonce, canary_cipher) = create_canary(&key).unwrap();

        assert!(unlock("hunter2", &salt, &canary_nonce, &canary_cipher).is_ok());
        assert!(unlock("wrong", &salt, &canary_nonce, &canary_cipher).is_err());
    }
}
