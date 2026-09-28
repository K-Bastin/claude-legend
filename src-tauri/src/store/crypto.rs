//! End-to-end encryption of the sync target, with a passphrase known only to
//! the user's machines.
//!
//! File contents are sealed with XChaCha20-Poly1305 under a key derived from
//! the passphrase with Argon2id. An encrypted file is `MAGIC ‖ nonce ‖
//! ciphertext ‖ tag`, a fixed [`OVERHEAD`] over the plain size. The store root
//! holds a plain `encryption.json` marker with the salt, the KDF parameters and
//! a known value sealed with the key, to tell a wrong passphrase apart.
//! File and folder names (project keys, session ids) are not encrypted.

use anyhow::{anyhow, bail, Context};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

pub const MARKER: &str = "encryption.json";
const MAGIC: &[u8; 4] = b"CLE1";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Bytes an encrypted file has over its plain content.
pub const OVERHEAD: u64 = (MAGIC.len() + NONCE_LEN + TAG_LEN) as u64;
const CHECK: &[u8] = b"claude-legend";

#[derive(Clone)]
pub struct Cipher {
    aead: XChaCha20Poly1305,
}

impl Cipher {
    fn new(key: &[u8; 32]) -> Self {
        Self {
            aead: XChaCha20Poly1305::new(key.into()),
        }
    }

    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let sealed = self
            .aead
            .encrypt(XNonce::from_slice(&nonce), plain)
            .expect("XChaCha20-Poly1305 encryption cannot fail");
        [MAGIC.as_slice(), &nonce, &sealed].concat()
    }

    pub fn open(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        if !is_sealed(data) {
            bail!("contenu non chiffré");
        }
        let (nonce, sealed) = data[MAGIC.len()..].split_at(NONCE_LEN);
        self.aead
            .decrypt(XNonce::from_slice(nonce), sealed)
            .map_err(|_| {
                anyhow!("contenu chiffré illisible (phrase de passe différente ou fichier abîmé)")
            })
    }
}

pub fn is_sealed(data: &[u8]) -> bool {
    data.len() as u64 >= OVERHEAD && data.starts_with(MAGIC)
}

/// Content of `encryption.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub version: u32,
    /// Argon2id memory in KiB, iterations and lanes.
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,
    /// [`CHECK`] sealed with the key.
    pub check: String,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn derive(passphrase: &str, marker: &Marker) -> anyhow::Result<Cipher> {
    let salt = b64()
        .decode(&marker.salt)
        .context("sel de chiffrement illisible")?;
    let params = Params::new(marker.m_cost, marker.t_cost, marker.p_cost, Some(32))
        .map_err(|e| anyhow!("paramètres de chiffrement invalides : {e}"))?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
        .map_err(|e| anyhow!("dérivation de la clé impossible : {e}"))?;
    Ok(Cipher::new(&key))
}

impl Marker {
    /// A new marker with a fresh salt, and its cipher.
    pub fn create(passphrase: &str) -> anyhow::Result<(Self, Cipher)> {
        if passphrase.chars().count() < 8 {
            bail!("la phrase de passe doit faire au moins 8 caractères");
        }
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let params = Params::default();
        let mut marker = Marker {
            version: 1,
            m_cost: params.m_cost(),
            t_cost: params.t_cost(),
            p_cost: params.p_cost(),
            salt: b64().encode(salt),
            check: String::new(),
        };
        let cipher = derive(passphrase, &marker)?;
        marker.check = b64().encode(cipher.seal(CHECK));
        Ok((marker, cipher))
    }

    /// The cipher for this marker, if `passphrase` is the right one.
    pub fn unlock(&self, passphrase: &str) -> anyhow::Result<Cipher> {
        if self.version != 1 {
            bail!("chiffrement créé par une version plus récente de Claude Legend : mets ce PC à jour");
        }
        let cipher = derive(passphrase, self)?;
        let check = b64()
            .decode(&self.check)
            .context("marqueur de chiffrement illisible")?;
        match cipher.open(&check) {
            Ok(plain) if plain == CHECK => Ok(cipher),
            _ => bail!("phrase de passe de chiffrement incorrecte"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_content_roundtrips_with_a_fixed_overhead() {
        let (marker, cipher) = Marker::create("correct horse battery").unwrap();
        for plain in [b"".as_slice(), b"{\"a\":1}", &[7u8; 5000]] {
            let sealed = cipher.seal(plain);
            assert!(is_sealed(&sealed));
            assert_eq!(sealed.len() as u64, plain.len() as u64 + OVERHEAD);
            assert_eq!(cipher.open(&sealed).unwrap(), plain);
        }
        // Same key from the marker, as on another machine.
        let other = marker.unlock("correct horse battery").unwrap();
        assert_eq!(other.open(&cipher.seal(b"x")).unwrap(), b"x");
    }

    #[test]
    fn wrong_passphrase_and_tampering_are_refused() {
        let (marker, cipher) = Marker::create("correct horse battery").unwrap();
        assert!(marker.unlock("wrong horse battery").is_err());
        let mut sealed = cipher.seal(b"secret");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(cipher.open(&sealed).is_err());
        assert!(cipher.open(b"plain text").is_err());
        assert!(Marker::create("court").is_err());
    }
}
