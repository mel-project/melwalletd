use std::{collections::BTreeMap, path::Path};

use acidjson::AcidJson;
use serde::{Deserialize, Serialize};
use tmelcrypt::Ed25519SK;

/// Represents a whole directory of persistent secrets, some of which may be unlocked
pub struct SecretStore {
    /// Maps wallet name to secret.
    secrets: AcidJson<BTreeMap<String, PersistentSecret>>,
}

impl SecretStore {
    /// Opens or creates a secretstore from a given filename.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        // if not exists, create
        if std::fs::read(path).is_err() {
            std::fs::write(path, "{}")?;
        }
        Ok(Self {
            secrets: AcidJson::open(path)?,
        })
    }

    /// Stores a new PersistentSecret into the SecretStore.
    pub fn store(&self, name: String, secret: PersistentSecret) {
        self.secrets.write().insert(name, secret);
    }

    /// Obtains a PersistentSecret from the SecretStore.
    pub fn load(&self, name: &str) -> Option<PersistentSecret> {
        self.secrets.read().get(name).cloned()
    }
}

/// A persistent signing secret (right now, either a plaintext secret key or a password-protected secret key)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PersistentSecret {
    Plaintext(Ed25519SK),
    PasswordEncrypted(EncryptedSK),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EncryptedSK {
    #[serde(with = "stdcode::hex")]
    argon2id_salt: Vec<u8>,
    argon2id_mem_cost: u32,
    argon2id_time_cost: u32,
    #[serde(with = "stdcode::hex")]
    cp20p1350_ciphertext: Vec<u8>,
}

impl EncryptedSK {
    /// Generates a new encrypted SK from a password and secret key.
    pub fn new(sk: Ed25519SK, pwd: &str) -> Self {
        let mut salt = [0u8; 16];
        getrandom::getrandom(&mut salt).unwrap();
        const MEM_COST: u32 = 32 * 1024;
        const TIME_COST: u32 = 10;
        let cfg = argon2::Config {
            ad: &[],
            hash_length: 32, // always enough
            lanes: 1,
            mem_cost: MEM_COST,
            secret: &[],
            thread_mode: argon2::ThreadMode::Sequential,
            time_cost: TIME_COST,
            variant: argon2::Variant::Argon2id,
            version: argon2::Version::Version13,
        };
        let encryption_key =
            argon2::hash_raw(pwd.as_bytes(), &salt, &cfg).expect("argon2id invocation failed");
        // now we use this secret key to encrypt the secret key
        let aead = crypto_api_chachapoly::ChachaPolyIetf::aead_cipher();
        let mut output_buf = vec![0u8; sk.0.len() + 16];
        aead.seal_to(&mut output_buf, &sk.0, &[], &encryption_key, &[0; 12])
            .expect("seal failed");
        Self {
            argon2id_salt: salt.to_vec(),
            argon2id_mem_cost: MEM_COST,
            argon2id_time_cost: TIME_COST,
            cp20p1350_ciphertext: output_buf,
        }
    }

    /// Decrypts to an ed25519 secret key.
    pub fn decrypt(&self, pwd: &str) -> Option<Ed25519SK> {
        let cfg = argon2::Config {
            ad: &[],
            hash_length: 32, // always enough
            lanes: 1,
            mem_cost: self.argon2id_mem_cost,
            secret: &[],
            thread_mode: argon2::ThreadMode::Sequential,
            time_cost: self.argon2id_time_cost,
            variant: argon2::Variant::Argon2id,
            version: argon2::Version::Version13,
        };
        let encryption_key = argon2::hash_raw(pwd.as_bytes(), &self.argon2id_salt, &cfg)
            .expect("argon2id invocation failed");
        let aead = crypto_api_chachapoly::ChachaPolyIetf::aead_cipher();
        let mut output = [0u8; 64];
        aead.open_to(
            &mut output,
            &self.cp20p1350_ciphertext,
            &[],
            &encryption_key,
            &[0; 12],
        )
        .ok()?;
        Some(Ed25519SK(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple() {
        let sk = Ed25519SK::generate();
        let encrypted = EncryptedSK::new(sk, "hello world");
        assert!(encrypted.decrypt("hello world").is_some());
        assert!(encrypted.decrypt("hello worldr").is_none())
    }
}
