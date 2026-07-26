use aes::Aes256;
use anyhow::{Result, anyhow, bail};
use base64::{
    Engine as _, engine::general_purpose::STANDARD as B64,
    engine::general_purpose::URL_SAFE_NO_PAD as B64_URL_SAFE,
};
use ctr::Ctr32BE;
use ctr::cipher::{KeyIvInit, StreamCipher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::models::FilemoonResponse;

pub const EPISODE_SOURCES_HASH: &str =
    "f4662f4b7510b26795dd53ef824a0bf1740fbbc5d1273fab18222ac831bca8d0";

/// Base URL for the AllAnime web app — used to scrape live keygen parameters.
const MKISSA_KEYGEN_URL: &str = "https://mkissa.to/";
/// CDN base for the app's immutable JS bundles (chunks containing the AES mask).
const KEYGEN_CDN_IMMUTABLE: &str = "https://cdn.allanime.day/all/mk/_app/immutable/";

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
        let key = hex_to_32bytes("a55ce35d83c1417fdfec0192c2b847eeae58d5bbb331a179d293aac40c035795")
            .unwrap_or([0u8; 32]);
        Self {
            epoch: 6886,
            key,
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
                        if file_data.epoch > 0 && key_bytes != [0u8; 32] {
                            let static_key_bytes = file_data
                                .static_key
                                .as_deref()
                                .and_then(|s| hex_to_32bytes(s).ok())
                                .unwrap_or_default();

                            let query_hash = if file_data.query_hash.is_empty() {
                                EPISODE_SOURCES_HASH.to_string()
                            } else {
                                file_data.query_hash
                            };

                            return Self {
                                epoch: file_data.epoch,
                                key: key_bytes,
                                query_hash,
                                static_key: static_key_bytes,
                            };
                        }
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

    /// Scrape live keygen parameters from mkissa.to — a Rust port of keygen.py.
    ///
    /// Fetches the site HTML to extract `epoch` and `partB`, locates the app
    /// entry JS, scans its chunk imports for the 64-hex AES mask, then XORs
    /// mask ^ base64(partB) to derive the key.  Also attempts to extract the
    /// GraphQL query hash from the chunk source via template-literal resolution.
    pub async fn fetch_keys_from_web(client: &Client) -> Result<Self> {
        eprintln!("[AllAnime] Fetching fresh crypto keygen from mkissa.to…");

        let html = client
            .get(MKISSA_KEYGEN_URL)
            .send()
            .await
            .map_err(|e| anyhow!("failed to fetch {MKISSA_KEYGEN_URL}: {e}"))?
            .text()
            .await?;

        // Extract window.__aaCrypto = {"epoch":…,"partB":"…",…}
        let aa_re = Regex::new(r"window\.__aaCrypto\s*=\s*(\{[^}]*\})")?;
        let aa_str = aa_re
            .captures(&html)
            .and_then(|c| c.get(1))
            .ok_or_else(|| anyhow!("__aaCrypto not found on mkissa.to"))?
            .as_str();
        let aa: serde_json::Value =
            serde_json::from_str(aa_str).map_err(|e| anyhow!("failed to parse __aaCrypto: {e}"))?;

        let epoch = aa["epoch"]
            .as_i64()
            .ok_or_else(|| anyhow!("epoch missing from __aaCrypto"))? as i32;
        let part_b_str = aa["partB"]
            .as_str()
            .ok_or_else(|| anyhow!("partB missing from __aaCrypto"))?;
        let part_b_bytes = B64
            .decode(part_b_str)
            .map_err(|e| anyhow!("partB base64 decode failed: {e}"))?;

        // Locate the app entry JS (e.g. "entry/app.DAbj2MyJ.js")
        let app_re = Regex::new(r#"_app/immutable/(entry/app\.[^"']+\.js)"#)?;
        let app_path = app_re
            .captures(&html)
            .and_then(|c| c.get(1))
            .ok_or_else(|| anyhow!("app.js entry not found on mkissa.to"))?
            .as_str();

        let app_js = client
            .get(format!("{KEYGEN_CDN_IMMUTABLE}{app_path}"))
            .send()
            .await?
            .text()
            .await?;

        // Collect chunk filenames: "../chunks/Foo.js" → "Foo.js"
        let chunks_re = Regex::new(r#"["']\.\./(chunks/[A-Za-z0-9_.%-]+\.js)["']"#)?;
        let chunks: Vec<String> = chunks_re
            .captures_iter(&app_js)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect();

        eprintln!("[AllAnime] Scanning {} chunk(s) for AES mask…", chunks.len());

        let mask_re = Regex::new("[0-9a-f]{64}")?;

        for chunk in &chunks {
            let js = match client
                .get(format!("{KEYGEN_CDN_IMMUTABLE}{chunk}"))
                .send()
                .await
            {
                Ok(r) => match r.text().await {
                    Ok(t) => t,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };

            if !js.contains("__aaCrypto") {
                continue;
            }

            let masks: Vec<&str> = mask_re.find_iter(&js).map(|m| m.as_str()).collect();
            if masks.len() != 1 {
                continue;
            }

            let mask_bytes = hex_to_32bytes(masks[0])?;
            let mut key = [0u8; 32];
            for i in 0..32 {
                key[i] = mask_bytes[i] ^ part_b_bytes.get(i).copied().unwrap_or(0);
            }

            let query_hash = source_query_hash(&js)
                .unwrap_or_else(|| EPISODE_SOURCES_HASH.to_string());

            let new_keygen = Self {
                epoch,
                key,
                query_hash,
                static_key: mask_bytes,
            };

            new_keygen.save_stored();
            eprintln!(
                "[AllAnime] Keygen updated (epoch={}, hash={})",
                new_keygen.epoch, new_keygen.query_hash
            );
            return Ok(new_keygen);
        }

        bail!("no __aaCrypto mask found in any app chunk — keygen refresh failed")
    }
}

/// Recursively resolve JS template-literal placeholders (`${name}`) within `tmpl`
/// by looking up variable/function definitions in the surrounding `chunk_js`.
fn resolve_template(tmpl: &str, chunk_js: &str, depth: usize) -> String {
    if depth > 6 {
        return tmpl.to_string();
    }

    let var_re = match Regex::new(r"\$\{([^}]+)\}") {
        Ok(r) => r,
        Err(_) => return tmpl.to_string(),
    };

    // Collect substitutions first to avoid holding borrows into `tmpl`
    let names: Vec<String> = var_re
        .captures_iter(tmpl)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();

    let mut result = tmpl.to_string();
    for name in &names {
        let repl = if name.ends_with("()") {
            // Arrow function: `helper = e => e ? \`truthy\` : \`falsy\``  — take the false branch.
            let fn_name = &name[..name.len() - 2];
            let pattern = format!(
                "{}\\s*=\\s*\\w+\\s*=>\\s*\\w+\\s*\\?\\s*`[^`]*`\\s*:\\s*`([^`]*)`",
                regex::escape(fn_name)
            );
            Regex::new(&pattern)
                .ok()
                .and_then(|re| re.captures(chunk_js))
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                .unwrap_or_default()
        } else {
            // Plain variable: `name = \`value\``
            let pattern = format!(
                "\\b{}\\s*=\\s*`([^`]*)`",
                regex::escape(name)
            );
            Regex::new(&pattern)
                .ok()
                .and_then(|re| re.captures(chunk_js))
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                .map(|v| resolve_template(&v, chunk_js, depth + 1))
                .unwrap_or_default()
        };
        result = result.replace(&format!("${{{name}}}"), &repl);
    }
    result
}

/// Locate the episode `sourceUrls` GraphQL query template literal in `chunk_js`,
/// resolve any `${…}` interpolations, then SHA-256 hash the final query string.
/// Returns `None` if the template cannot be found or fully resolved.
fn source_query_hash(chunk_js: &str) -> Option<String> {
    // Match template literals that start with `\nquery(` and contain the episode
    // sourceUrls fields — the captured group is the query body up to the closing
    // backtick delimiter.
    let re = Regex::new("(\\nquery\\([^`]*)`").ok()?;
    let template = re
        .captures_iter(chunk_js)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .find(|t| t.contains("sourceUrls") && t.contains("episode("))?;

    let query = resolve_template(&template, chunk_js, 0);
    if query.contains("${") {
        // Unresolved placeholders — hash would be wrong.
        return None;
    }

    let hash = Sha256::digest(query.as_bytes());
    Some(bytes_to_hex(&hash))
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
        Aes256Gcm, Nonce,
        aead::{Aead, KeyInit},
    };

    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow!("failed to initialize AES-GCM: {e}"))?;
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
        Aes256Gcm, Nonce,
        aead::{Aead, KeyInit},
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
