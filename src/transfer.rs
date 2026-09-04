use crate::crypto::{self, NONCE_LEN, SALT_LEN};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"INRX";
const FORMAT_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedCommand {
    pub id: String,
    pub template: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
    /// Variable name -> default env's *name* (not id - ids are only
    /// meaningful within the machine that generated them; names are the
    /// natural key envs are matched by everywhere else). Sparse: only
    /// variables with a default appear. `serde(default)` so a transfer
    /// file written before this field existed still decodes cleanly.
    #[serde(default)]
    pub variable_defaults: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedEnv {
    pub id: String,
    pub name: String,
    /// `EnvKind::as_str()` - "secret"/"text"/"number_float"/"number_int".
    pub kind: String,
    /// Always plaintext - secrets are decrypted before export and
    /// re-encrypted under the destination's master key on import. Safe
    /// because the whole payload is itself encrypted under the transfer
    /// passphrase before it ever touches disk.
    pub value: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferPayload {
    pub exported_at: String,
    pub commands: Vec<ExportedCommand>,
    pub envs: Vec<ExportedEnv>,
}

/// Encrypts `payload` under `passphrase` into a self-contained transfer
/// file: magic bytes + format version + a fresh KDF salt + AEAD nonce +
/// ciphertext. The passphrase is unrelated to either machine's master
/// password - it's whatever secret the sender and receiver agree on to
/// protect the file itself in transit.
pub fn encode(payload: &TransferPayload, passphrase: &str) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(payload)?;
    let salt = crypto::random_salt();
    let key = crypto::derive_key(passphrase, &salt)?;
    let (nonce, ciphertext) = crypto::encrypt(&key, &json)?;

    let mut out = Vec::with_capacity(MAGIC.len() + 1 + SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypts and parses a transfer file produced by `encode`. Fails with a
/// clear error on a wrong passphrase, a truncated/corrupted file, or a file
/// that isn't an inr transfer file at all.
pub fn decode(bytes: &[u8], passphrase: &str) -> Result<TransferPayload> {
    let header_len = MAGIC.len() + 1 + SALT_LEN + NONCE_LEN;
    if bytes.len() < header_len {
        return Err(anyhow!("not a valid inr transfer file (too short)"));
    }

    let (magic, rest) = bytes.split_at(MAGIC.len());
    if magic != MAGIC {
        return Err(anyhow!("not an inr transfer file"));
    }

    let (version, rest) = rest.split_at(1);
    if version[0] != FORMAT_VERSION {
        return Err(anyhow!(
            "unsupported transfer file version {} (this inr supports version {})",
            version[0],
            FORMAT_VERSION
        ));
    }

    let (salt, rest) = rest.split_at(SALT_LEN);
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let salt: [u8; SALT_LEN] = salt.try_into().expect("split_at guarantees length");

    let key = crypto::derive_key(passphrase, &salt)?;
    let plaintext = crypto::decrypt(&key, nonce, ciphertext)
        .map_err(|_| anyhow!("wrong transfer passphrase or corrupted file"))?;
    let payload: TransferPayload = serde_json::from_slice(&plaintext)
        .map_err(|e| anyhow!("transfer file content is corrupted: {e}"))?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> TransferPayload {
        TransferPayload {
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            commands: vec![ExportedCommand {
                id: "cmd1".to_string(),
                template: "ssh %s:host".to_string(),
                description: "connect".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                variable_defaults: BTreeMap::from([("host".to_string(), "prod-host".to_string())]),
            }],
            envs: vec![ExportedEnv {
                id: "env1".to_string(),
                name: "apikey".to_string(),
                kind: "secret".to_string(),
                value: "sk-super-secret".to_string(),
                description: "api key".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            }],
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let payload = sample_payload();
        let bytes = encode(&payload, "correct horse battery staple").unwrap();
        let decoded = decode(&bytes, "correct horse battery staple").unwrap();
        assert_eq!(decoded.commands[0].template, payload.commands[0].template);
        assert_eq!(decoded.envs[0].value, payload.envs[0].value);
    }

    #[test]
    fn encode_decode_roundtrips_variable_defaults() {
        let payload = sample_payload();
        let bytes = encode(&payload, "correct horse battery staple").unwrap();
        let decoded = decode(&bytes, "correct horse battery staple").unwrap();
        assert_eq!(
            decoded.commands[0].variable_defaults.get("host"),
            Some(&"prod-host".to_string())
        );
    }

    #[test]
    fn decode_defaults_variable_defaults_to_empty_when_field_is_absent() {
        // Simulates a transfer file written before this field existed:
        // the same payload shape, minus `variable_defaults` in the JSON.
        #[derive(serde::Serialize)]
        struct OldExportedCommand {
            id: String,
            template: String,
            description: String,
            created_at: String,
            updated_at: String,
        }
        #[derive(serde::Serialize)]
        struct OldPayload {
            exported_at: String,
            commands: Vec<OldExportedCommand>,
            envs: Vec<ExportedEnv>,
        }
        let old = OldPayload {
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            commands: vec![OldExportedCommand {
                id: "cmd1".to_string(),
                template: "echo hi".to_string(),
                description: "greet".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            }],
            envs: vec![],
        };
        let json = serde_json::to_vec(&old).unwrap();
        let salt = crypto::random_salt();
        let key = crypto::derive_key("pw", &salt).unwrap();
        let (nonce, ciphertext) = crypto::encrypt(&key, &json).unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(FORMAT_VERSION);
        bytes.extend_from_slice(&salt);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&ciphertext);

        let decoded = decode(&bytes, "pw").unwrap();
        assert!(decoded.commands[0].variable_defaults.is_empty());
    }

    #[test]
    fn decode_rejects_wrong_passphrase() {
        let bytes = encode(&sample_payload(), "correct passphrase").unwrap();
        let err = decode(&bytes, "wrong passphrase").unwrap_err();
        assert!(err.to_string().contains("wrong transfer passphrase"));
    }

    #[test]
    fn decode_rejects_non_inr_file() {
        let bytes = vec![0u8; 64];
        let err = decode(&bytes, "whatever").unwrap_err();
        assert!(err.to_string().contains("not an inr transfer file"));
    }

    #[test]
    fn decode_rejects_truncated_file() {
        let err = decode(b"short", "whatever").unwrap_err();
        assert!(err.to_string().contains("too short"));
    }
}
