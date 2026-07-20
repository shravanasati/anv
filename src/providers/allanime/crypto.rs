use aes::Aes256;
use anyhow::{anyhow, bail, Result};
use base64::{
    engine::general_purpose::STANDARD as B64,
    engine::general_purpose::URL_SAFE_NO_PAD as B64_URL_SAFE,
    Engine as _,
};
use ctr::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr32BE;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::models::FilemoonResponse;

pub const EPISODE_SOURCES_HASH: &str =
    "d405d0edd690624b66baba3068e0edc3ac90f1597d898a1ec8db4e5c43c00fec";

pub const KEYGEN_URL: &str =
    "https://raw.githubusercontent.com/sdaqo/anipy-cli/refs/heads/key-gen/scripts/keygen/keygen.json";

pub fn keygen_file_path() -> Option<PathBuf> {
    dirs_next::data_dir()
        .or_else(dirs_next::config_dir)
        .map(|d| d.join("anv").join("allanime_keygen.json"))
}

pub fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_to_32bytes(s: &str) -> Result<[u8; 32]> {
    let clean = s.trim();
    if clean.len() != 64 {
        bail!("hex string length is not 64 characters");
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow!("invalid hex character: {e}"))?;
    }
    Ok(bytes)
}

#[derive(Debug, Clone)]
pub struct AnimeKeygen {
    pub epoch: i32,
    pub key: [u8; 32],
    pub query_hash: String,
    pub static_key: [u8; 32],
}

impl Default for AnimeKeygen {
    fn default() -> Self {
        Self {
            epoch: 0,
            key: [0u8; 32],
            query_hash: EPISODE_SOURCES_HASH.to_string(),
            static_key: [0u8; 32],
        }
    }
}

use reqwest::Client;

impl AnimeKeygen {
    pub fn load_stored_or_default() -> Self {
        if let Some(path) = keygen_file_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(file_data) = serde_json::from_str::<StoredKeygen>(&content) {
                    if let Ok(key_bytes) = hex_to_32bytes(&file_data.key) {
                        let static_key_bytes = file_data
                            .static_key
                            .as_deref()
                            .and_then(|s| hex_to_32bytes(s).ok())
                            .unwrap_or_default();

                        return Self {
                            epoch: file_data.epoch,
                            key: key_bytes,
                            query_hash: file_data.query_hash,
                            static_key: static_key_bytes,
                        };
                    }
                }
            }
        }
        Self::default()
    }

    pub fn save_stored(&self) {
        if let Some(path) = keygen_file_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let stored = StoredKeygen {
                epoch: self.epoch,
                key: bytes_to_hex(&self.key),
                query_hash: self.query_hash.clone(),
                static_key: Some(bytes_to_hex(&self.static_key)),
            };
            if let Ok(json_str) = serde_json::to_string_pretty(&stored) {
                let _ = std::fs::write(path, json_str);
            }
        }
    }

    /// Fetch fresh keygen parameters from remote repository and update local storage.
    pub async fn refresh_from_remote(client: &Client) -> Result<Self> {
        eprintln!("[AllAnime] Refreshing crypto keygen parameters from remote repository…");
        let res = client
            .get(KEYGEN_URL)
            .send()
            .await?
            .error_for_status()?
            .json::<StoredKeygen>()
            .await?;

        let key_bytes = hex_to_32bytes(&res.key)?;
        let static_key_bytes = match res.static_key {
            Some(s) if s.len() == 64 => hex_to_32bytes(&s).unwrap_or_default(),
            _ => [0u8; 32],
        };

        let new_keygen = Self {
            epoch: res.epoch,
            key: key_bytes,
            query_hash: res.query_hash,
            static_key: static_key_bytes,
        };

        new_keygen.save_stored();

        eprintln!(
            "[AllAnime] Successfully updated keygen (epoch={}, hash={})",
            new_keygen.epoch, new_keygen.query_hash
        );

        Ok(new_keygen)
    }
}

#[derive(Deserialize, Serialize)]
pub struct StoredKeygen {
    pub epoch: i32,
    pub key: String,
    pub query_hash: String,
    pub static_key: Option<String>,
}

#[derive(Serialize)]
struct AaReqPayload<'a> {
    v: i32,
    ts: u64,
    epoch: i32,
    qh: &'a str,
}

pub fn build_aa_req(qh: &str, keygen: &AnimeKeygen) -> Result<String> {
    let key = &keygen.key;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("SystemTime before UNIX EPOCH: {e}"))?
        .as_millis();
    let ts = (now_ms / 300_000) * 300_000;
    let ts_u64 = ts as u64;

    let payload = serde_json::to_string(&AaReqPayload {
        v: 1,
        ts: ts_u64,
        epoch: keygen.epoch,
        qh,
    })?;

    let iv_input = format!("{}:{}:{}", keygen.epoch, qh, ts_u64);
    let iv_hash = Sha256::digest(iv_input.as_bytes());
    let iv_bytes = &iv_hash[..12];

    use aes_gcm::{
        aead::{Aead, KeyInit},
        Aes256Gcm, Nonce,
    };

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| anyhow!("failed to initialize AES-GCM: {e}"))?;
    let nonce = Nonce::from_slice(iv_bytes);

    let encrypted = cipher
        .encrypt(nonce, payload.as_bytes())
        .map_err(|e| anyhow!("AES-GCM encryption failed: {e}"))?;

    let mut buffer = Vec::with_capacity(1 + 12 + encrypted.len());
    buffer.push(1);
    buffer.extend_from_slice(iv_bytes);
    buffer.extend_from_slice(&encrypted);

    Ok(B64.encode(buffer))
}

pub fn decrypt_tobeparsed(blob: &str, keygen: &AnimeKeygen) -> Result<String> {
    let raw = B64
        .decode(blob)
        .map_err(|e| anyhow!("tobeparsed base64 decode failed: {e}"))?;
    if raw.len() < 1 + 12 + 16 {
        bail!("tobeparsed blob too short ({} bytes)", raw.len());
    }

    let nonce_bytes = &raw[1..13];
    let ciphertext_and_tag = &raw[13..];

    use aes_gcm::{
        aead::{Aead, KeyInit},
        Aes256Gcm, Nonce,
    };

    let nonce = Nonce::from_slice(nonce_bytes);

    let candidate_keys: &[&[u8; 32]] = &[&keygen.key, &keygen.static_key];
    for key in candidate_keys {
        let cipher = match Aes256Gcm::new_from_slice(*key) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Ok(plaintext) = cipher.decrypt(nonce, ciphertext_and_tag) {
            return String::from_utf8(plaintext)
                .map_err(|e| anyhow!("tobeparsed plaintext is not valid UTF-8: {e}"));
        }
    }

    bail!("AES-GCM decryption failed: tobeparsed could not be decrypted with any known key")
}

pub fn decrypt_filemoon(resp: &FilemoonResponse) -> Result<String> {
    let kp1 = B64_URL_SAFE
        .decode(&resp.key_parts[0])
        .map_err(|e| anyhow!("filemoon kp1 decode failed: {e}"))?;
    let kp2 = B64_URL_SAFE
        .decode(&resp.key_parts[1])
        .map_err(|e| anyhow!("filemoon kp2 decode failed: {e}"))?;
    let iv_raw = B64_URL_SAFE
        .decode(&resp.iv)
        .map_err(|e| anyhow!("filemoon iv decode failed: {e}"))?;
    let ciphertext = B64_URL_SAFE
        .decode(&resp.payload)
        .map_err(|e| anyhow!("filemoon payload decode failed: {e}"))?;

    let mut key = Vec::with_capacity(kp1.len() + kp2.len());
    key.extend_from_slice(&kp1);
    key.extend_from_slice(&kp2);

    if key.len() != 32 {
        bail!("filemoon key length is not 32 bytes (got {})", key.len());
    }

    if iv_raw.len() < 12 {
        bail!("filemoon iv length is too short (got {})", iv_raw.len());
    }

    let mut iv = [0u8; 16];
    iv[..12].copy_from_slice(&iv_raw[..12]);
    iv[15] = 0x02;

    let mut plaintext = ciphertext;
    let mut cipher = Ctr32BE::<Aes256>::new(key.as_slice().into(), &iv.into());
    cipher.apply_keystream(&mut plaintext);

    String::from_utf8(plaintext).map_err(|e| anyhow!("filemoon plaintext is not valid UTF-8: {e}"))
}

pub fn decode_provider_path(raw: &str) -> Option<String> {
    if !raw.starts_with("--") {
        return None;
    }
    let bytes = raw.trim_start_matches("--");
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut decoded = String::with_capacity(bytes.len() / 2);
    for chunk in bytes.as_bytes().chunks(2) {
        let pair = std::str::from_utf8(chunk).ok()?.to_ascii_lowercase();
        let ch = decode_pair(&pair)?;
        decoded.push(ch);
    }
    if decoded.contains("/clock") && !decoded.contains(".json") {
        decoded = decoded.replacen("/clock", "/clock.json", 1);
    }
    Some(decoded)
}

fn decode_pair(pair: &str) -> Option<char> {
    let byte = u8::from_str_radix(pair, 16).ok()?;
    let ch = (byte ^ 0x38) as char;
    ch.is_ascii_graphic().then_some(ch)
}
