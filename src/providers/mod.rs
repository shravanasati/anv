use crate::types::{Provider, ShowInfo, StreamOption, Translation};
use anyhow::{Result, bail};

pub mod animehub;
pub mod anineko;

pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0 Safari/537.36";

pub const AUTO_QUALITY_LABEL: &str = "auto";
pub const AUTO_QUALITY_RANK: i32 = 0;

pub fn parse_quality_rank(label: &str) -> i32 {
    let clean = label.trim().to_lowercase();
    if clean.starts_with("1080") {
        1080
    } else if clean.starts_with("720") {
        720
    } else if clean.starts_with("480") {
        480
    } else if clean.starts_with("360") {
        360
    } else if clean.starts_with("240") {
        240
    } else {
        AUTO_QUALITY_RANK
    }
}

static RE_HLS_RES: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r#"RESOLUTION=\d+x(\d+)"#).unwrap());

/// Shared HLS master playlist parser for anime providers. Parses `#EXT-X-STREAM-INF`
/// resolution variants, constructs absolute URLs, sorts best quality first, and falls
/// back to `auto`/0 master URL if no variants are found.
pub fn parse_hls_master(
    master_url: &str,
    content: &str,
    referer: &str,
    provider_name: &str,
) -> Vec<StreamOption> {
    use std::collections::HashMap;
    use url::Url;

    let base_url = Url::parse(master_url).ok();
    let mut streams = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    for i in 0..lines.len() {
        let line = lines[i].trim();
        if line.starts_with("#EXT-X-STREAM-INF:") {
            let height = RE_HLS_RES
                .captures(line)
                .and_then(|c| c[1].parse::<i32>().ok())
                .unwrap_or(0);

            if let Some(next_line) = lines.get(i + 1) {
                let variant = next_line.trim();
                if !variant.is_empty() && !variant.starts_with('#') {
                    let absolute_url = if let Some(ref base) = base_url {
                        base.join(variant)
                            .map(|u| u.to_string())
                            .unwrap_or_else(|_| variant.to_string())
                    } else {
                        variant.to_string()
                    };

                    let label = if height > 0 {
                        format!("{height}p")
                    } else {
                        AUTO_QUALITY_LABEL.to_string()
                    };

                    let mut headers = HashMap::new();
                    headers.insert("Referer".to_string(), referer.to_string());

                    streams.push(StreamOption {
                        provider: provider_name.to_string(),
                        url: absolute_url,
                        quality_label: label,
                        quality_rank: height,
                        is_hls: true,
                        headers,
                        subtitle: None,
                    });
                }
            }
        }
    }

    if streams.is_empty() {
        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), referer.to_string());

        streams.push(StreamOption {
            provider: provider_name.to_string(),
            url: master_url.to_string(),
            quality_label: AUTO_QUALITY_LABEL.to_string(),
            quality_rank: AUTO_QUALITY_RANK,
            is_hls: true,
            headers,
            subtitle: None,
        });
    } else {
        streams.sort_by_key(|b| std::cmp::Reverse(b.quality_rank));
    }

    streams
}

/// Generic HTTP request executor with exponential backoff for retrying transient errors.
#[allow(dead_code)]
pub async fn fetch_with_retry<F>(
    builder_fn: F,
    attempts: u32,
    initial_backoff: std::time::Duration,
) -> Result<reqwest::Response>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    use anyhow::Context;

    let mut backoff = initial_backoff;
    for i in 1..=attempts {
        let req = builder_fn();
        match req.send().await {
            Ok(res) => {
                let status = res.status();
                if status.is_success() {
                    return Ok(res);
                } else if (status.is_server_error()
                    || status == reqwest::StatusCode::TOO_MANY_REQUESTS)
                    && i < attempts
                {
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                    continue;
                } else {
                    return res.error_for_status().context("HTTP request failed");
                }
            }
            Err(_err) if i < attempts => {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            Err(err) => return Err(err.into()),
        }
    }
    bail!("HTTP request failed after {attempts} attempts")
}

pub trait AnimeProvider {
    async fn search_shows(&self, query: &str, translation: Translation) -> Result<Vec<ShowInfo>>;
    async fn fetch_episodes(&self, show_id: &str, translation: Translation) -> Result<Vec<String>>;
    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>>;
    async fn fetch_mal_id(&self, _show_id: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

macro_rules! delegate_anime {
    ($self:expr, $fn:ident ($($arg:expr),* $(,)?)) => {
        match $self {
            Self::Animehub(c) => c.$fn($($arg),*).await,
            Self::Anineko(c) => c.$fn($($arg),*).await,
        }
    };
}

#[derive(Debug, Clone)]
pub enum AnyAnimeClient {
    Animehub(animehub::AnimehubClient),
    Anineko(anineko::AninekoClient),
}

impl AnimeProvider for AnyAnimeClient {
    async fn search_shows(&self, query: &str, translation: Translation) -> Result<Vec<ShowInfo>> {
        delegate_anime!(self, search_shows(query, translation))
    }

    async fn fetch_episodes(&self, show_id: &str, translation: Translation) -> Result<Vec<String>> {
        delegate_anime!(self, fetch_episodes(show_id, translation))
    }

    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>> {
        delegate_anime!(self, fetch_streams(show_id, translation, episode))
    }

    async fn fetch_mal_id(&self, show_id: &str) -> Result<Option<String>> {
        delegate_anime!(self, fetch_mal_id(show_id))
    }
}

impl Provider {
    pub fn anime_client(&self) -> Result<AnyAnimeClient> {
        match self {
            Provider::Animehub => Ok(AnyAnimeClient::Animehub(animehub::AnimehubClient::new()?)),
            Provider::Anineko => Ok(AnyAnimeClient::Anineko(anineko::AninekoClient::new()?)),
            _ => bail!(
                "Provider '{}' does not support anime streaming.",
                self.display_name()
            ),
        }
    }

    pub fn all_anime() -> &'static [Provider] {
        &[
            Provider::Animehub,
            Provider::Anineko,
        ]
    }

    pub fn valid_anime_providers() -> String {
        let mut names = vec!["all"];
        names.extend(Provider::all_anime().iter().map(|p| p.cli_name()));
        names.join(", ")
    }
}
