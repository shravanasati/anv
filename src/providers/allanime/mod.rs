use anyhow::{anyhow, bail, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use super::USER_AGENT;

pub mod anime;
pub mod crypto;
pub mod manga;
pub mod models;
pub mod queries;
pub mod streams;

pub use crypto::AnimeKeygen;
use crypto::{build_aa_req, decrypt_tobeparsed, EPISODE_SOURCES_HASH};
use models::*;
use streams::*;

pub const ALLANIME_API_URL: &str = "https://api.allanime.day/api";
pub const ALLANIME_BASE_URL: &str = "https://allanime.day";
pub const ALLANIME_IMAGE_REFERER: &str = "https://allanime.to";
pub const ALLANIME_ORIGIN: &str = "https://allanime.day";

pub struct AllAnimeClient {
    pub(super) client: Client,
    pub(super) prefer_english_titles: bool,
    /// Base URL for GraphQL API calls. Defaults to `ALLANIME_API_URL` but can
    /// be overridden with a relay/proxy to bypass Cloudflare geo-blocking.
    pub(super) api_url: String,
    pub(super) keygen: Arc<RwLock<AnimeKeygen>>,
}

impl AllAnimeClient {
    pub fn new(prefer_english_titles: bool, api_proxy: Option<&str>) -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()?;

        let api_url = match api_proxy {
            Some(url) if !url.is_empty() => {
                let base = url.trim_end_matches('/');
                let url = if base.ends_with("/api") {
                    base.to_string()
                } else {
                    format!("{base}/api")
                };
                eprintln!("Using API proxy: {url}");
                url
            }
            _ => ALLANIME_API_URL.to_string(),
        };

        Ok(Self {
            client,
            prefer_english_titles,
            api_url,
            keygen: Arc::new(RwLock::new(AnimeKeygen::load_stored_or_default())),
        })
    }

    /// Fetch fresh keygen parameters from remote github repository and update disk storage.
    pub async fn refresh_keygen(&self) -> Result<AnimeKeygen> {
        let new_keygen = AnimeKeygen::refresh_from_remote(&self.client).await?;
        let mut guard = self.keygen.write().await;
        *guard = new_keygen.clone();
        Ok(new_keygen)
    }

    /// Execute a GraphQL request (either GET or POST) and deserialize the `data` field.
    ///
    /// Automatically retries when rate limited or when AA_CRYPTO_STALE is returned.
    pub(super) async fn execute_graphql<T: DeserializeOwned>(
        &self,
        use_get: bool,
        variables: serde_json::Value,
        query: Option<&str>,
        hash: Option<&str>,
    ) -> Result<T> {
        const MAX_RETRIES: u32 = 3;

        for attempt in 0..=MAX_RETRIES {
            let current_keygen = self.keygen.read().await.clone();

            let request = if use_get {
                let mut extensions = serde_json::json!({});
                if let Some(h) = hash {
                    let active_hash = if h == EPISODE_SOURCES_HASH {
                        &current_keygen.query_hash
                    } else {
                        h
                    };

                    let aa_req = build_aa_req(active_hash, &current_keygen)?;
                    extensions = serde_json::json!({
                        "persistedQuery": {
                            "version": 1,
                            "sha256Hash": active_hash
                        },
                        "aaReq": aa_req
                    });
                }

                self.client
                    .get(&self.api_url)
                    .query(&[
                        ("variables", serde_json::to_string(&variables)?),
                        ("extensions", serde_json::to_string(&extensions)?),
                    ])
                    .header("Referer", "https://youtu-chan.com/")
                    .header("Origin", "https://mkissa.to")
            } else {
                let mut body = serde_json::json!({ "variables": variables });
                if let Some(q) = query {
                    body["query"] = serde_json::json!(q);
                }
                self.client
                    .post(&self.api_url)
                    .header("Referer", ALLANIME_REFERER)
                    .header("Origin", ALLANIME_ORIGIN)
                    .json(&body)
            };

            let response = request.header("Accept", "application/json").send().await?;
            let status = response.status();
            let text = response.text().await?;

            if !status.is_success() {
                bail!("AllAnime API HTTP {status}: {text}");
            }

            if std::env::var("ANV_DEBUG").is_ok() {
                eprintln!(
                    "[AllAnime] {} HTTP {status} — raw response:\n{text}",
                    if use_get { "GET" } else { "POST" }
                );
            }

            // AllAnime now AES-256-GCM-encrypts responses; detect and unwrap.
            let json_str: std::borrow::Cow<str> = if text.contains("\"tobeparsed\"") {
                let enc: EncryptedEnvelope = match serde_json::from_str(&text) {
                    Ok(e) => e,
                    Err(e) => bail!("failed to parse encrypted AllAnime envelope: {e}\nRaw:\n{text}"),
                };

                let plaintext = match decrypt_tobeparsed(&enc.data.tobeparsed, &current_keygen) {
                    Ok(pt) => pt,
                    Err(err) => {
                        if attempt < MAX_RETRIES {
                            eprintln!("[AllAnime] tobeparsed decryption failed ({err}). Refreshing keygen…");
                            let _ = self.refresh_keygen().await;
                            continue;
                        } else {
                            bail!(err);
                        }
                    }
                };

                if std::env::var("ANV_DEBUG").is_ok() {
                    eprintln!("[AllAnime] decrypted tobeparsed plaintext:\n{plaintext}");
                }

                std::borrow::Cow::Owned(format!(r#"{{"data":{plaintext}}}"#))
            } else {
                std::borrow::Cow::Borrowed(&text)
            };

            let raw_envelope: GraphQlRawEnvelope = serde_json::from_str(&json_str).map_err(|e| {
                anyhow!("failed to parse AllAnime API raw envelope: {e}\nJSON:\n{json_str}")
            })?;

            if let Some(ref errors) = raw_envelope.errors {
                if let Some(first) = errors.first() {
                    if first.message.contains("AA_CRYPTO_STALE") || first.message.contains("AA_CRYPTO_MISSING") {
                        if attempt < MAX_RETRIES {
                            eprintln!("[AllAnime] Received {}, refreshing keygen and retrying (attempt {}/{MAX_RETRIES})…", first.message, attempt + 1);
                            let _ = self.refresh_keygen().await;
                            continue;
                        }
                    }

                    let wait_secs = first
                        .message
                        .split_whitespace()
                        .rev()
                        .nth(1)
                        .and_then(|s| s.parse::<u64>().ok());

                    if let Some(secs) = wait_secs {
                        if attempt < MAX_RETRIES {
                            eprintln!(
                                "Rate limited by AllAnime — retrying in {secs}s (attempt {}/{MAX_RETRIES})…",
                                attempt + 1
                            );
                            tokio::time::sleep(Duration::from_secs(secs)).await;
                            continue;
                        }
                    }

                    let joined = errors
                        .iter()
                        .map(|e| e.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ");
                    bail!("AllAnime API error: {joined}");
                }
            }

            let data_value = raw_envelope
                .data
                .ok_or_else(|| anyhow!("AllAnime API returned empty response"))?;

            let data: T = serde_json::from_value(data_value).map_err(|e| {
                anyhow!("failed to parse AllAnime API data structure: {e}\nJSON:\n{json_str}")
            })?;

            return Ok(data);
        }

        bail!("AllAnime API: exceeded maximum retries");
    }

    /// POST a GraphQL request to the AllAnime API and deserialize the `data` field.
    pub(super) async fn post_graphql<T: DeserializeOwned>(&self, body: &serde_json::Value) -> Result<T> {
        let variables = body
            .get("variables")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let query = body.get("query").and_then(|v| v.as_str());
        self.execute_graphql(false, variables, query, None).await
    }

    pub(super) async fn fetch_provider_json(&self, path: &str) -> Result<serde_json::Value> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{ALLANIME_BASE_URL}{path}")
        };
        let response = self
            .client
            .get(&url)
            .header("Referer", ALLANIME_REFERER)
            .header("Origin", ALLANIME_ORIGIN)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        Ok(response)
    }
}

impl Default for AllAnimeClient {
    fn default() -> Self {
        Self::new(false, None).expect("failed to build HTTP client")
    }
}
