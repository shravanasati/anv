use aes::Aes256;
use anyhow::{Result, anyhow, bail};
use base64::{
    Engine as _, engine::general_purpose::STANDARD as B64,
    engine::general_purpose::URL_SAFE_NO_PAD as B64_URL_SAFE,
};
use ctr::Ctr32BE;
use ctr::cipher::{KeyIvInit, StreamCipher};
use reqwest::Client;
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{StreamExt, stream::FuturesUnordered};

use super::{AnimeProvider, MangaProvider, USER_AGENT};
use crate::types::{
    Chapter, ChapterCounts, EpisodeCounts, MangaInfo, Page, ShowInfo, StreamOption, Translation,
};

const ALLANIME_API_URL: &str = "https://api.allanime.day/api";
const ALLANIME_BASE_URL: &str = "https://allanime.day";
const ALLANIME_REFERER: &str = "https://allmanga.to";
const ALLANIME_IMAGE_REFERER: &str = "https://allanime.to";
const ALLANIME_ORIGIN: &str = "https://allanime.day";
const EPISODE_SOURCES_HASH: &str =
    "d405d0edd690624b66baba3068e0edc3ac90f1597d898a1ec8db4e5c43c00fec";

// AES-256-GCM key used for both aaReq token generation and tobeparsed decryption.
// This is the XOR-derived key from the AllAnime CDN JS bundle (mask ^ base64(partB)).
// Source: https://cdn.allanime.day/all/mk/_app/immutable/chunks/DNe1e6Xy.js
// Update when AllAnime rotates their key (check for AA_CRYPTO_STALE / AA_CRYPTO_MISSING errors).
const ALLANIME_CRYPTO_KEY: [u8; 32] = [
    0x22, 0x19, 0x6f, 0xa6, 0xaf, 0xca, 0x95, 0x30,
    0x9f, 0xda, 0xbe, 0x9a, 0x35, 0x34, 0xb8, 0x7c,
    0xd2, 0x45, 0x4e, 0x50, 0xef, 0xea, 0xbf, 0xcb,
    0xdb, 0xdf, 0xd3, 0xde, 0x67, 0x8b, 0x39, 0x82,
];

// AllAnime sometimes encrypts the tobeparsed response with this static legacy key
// (sha256("Xot36i3lK3:v1")) instead of the aaReq key, depending on the rotation.
// Both are tried in decrypt_tobeparsed — see anipy-cli PR #335.
const ALLANIME_RESPONSE_STATIC_KEY: [u8; 32] = {
    // sha256(b"Xot36i3lK3:v1") precomputed at compile time.
    // Computed with:
    // python3 -c "import hashlib; d=hashlib.sha256(b'Xot36i3lK3:v1').digest(); print([hex(b) for b in d])"
    // ['0xa2', '0x54', '0xaa', '0x27', '0xc4', '0x10', '0xf2', '0x97', '0xbd', '0x4',
    //  '0xba', '0x33', '0xa0', '0xc0', '0xdf', '0x7f', '0xf4', '0xe7', '0x6', '0xbf',
    //  '0x3a', '0xe2', '0x72', '0x71', '0xc6', '0x70', '0x3f', '0x84', '0xe7', '0x50',
    //  '0xf5', '0x52']
    [
        0xa2, 0x54, 0xaa, 0x27, 0xc4, 0x10, 0xf2, 0x97,
        0xbd, 0x04, 0xba, 0x33, 0xa0, 0xc0, 0xdf, 0x7f,
        0xf4, 0xe7, 0x06, 0xbf, 0x3a, 0xe2, 0x72, 0x71,
        0xc6, 0x70, 0x3f, 0x84, 0xe7, 0x50, 0xf5, 0x52,
    ]
};

// Providers known to yield direct HLS/MP4 URLs via the clock.json mechanism.
// The remaining providers (Ok, Vg, Fm-Hls, Mp4, Sw, …) are JS-obfuscated iframe
// embeds that require per-provider HTML/JS scraping to extract a playable URL —
// not currently implemented. Luf-Mp4 and Yt-mp4 cover the vast majority of shows.
const PREFERRED_PROVIDERS: &[&str] = &[
    "Default", "S-mp4", "Luf-Mp4", "Yt-mp4", "Fm-mp4", "Fm-Hls", "Mp4",
];

pub struct AllAnimeClient {
    client: Client,
    prefer_english_titles: bool,
    /// Base URL for GraphQL API calls.  Defaults to `ALLANIME_API_URL` but can
    /// be overridden with a relay/proxy to bypass Cloudflare geo-blocking.
    api_url: String,
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

        Ok(Self { client, prefer_english_titles, api_url })
    }

    /// Execute a GraphQL request (either GET or POST) and deserialize the `data` field.
    async fn execute_graphql<T: DeserializeOwned>(
        &self,
        use_get: bool,
        variables: serde_json::Value,
        query: Option<&str>,
        hash: Option<&str>,
    ) -> Result<T> {
        let request = if use_get {
            let mut extensions = serde_json::json!({});
            if let Some(h) = hash {
                if h == EPISODE_SOURCES_HASH {
                    let aa_req = build_aa_req(h)?;
                    extensions = serde_json::json!({
                        "persistedQuery": {
                            "version": 1,
                            "sha256Hash": h
                        },
                        "aaReq": aa_req
                    });
                } else {
                    extensions = serde_json::json!({
                        "persistedQuery": {
                            "version": 1,
                            "sha256Hash": h
                        }
                    });
                }
            }

            self.client
                .get(&self.api_url)
                .query(&[
                    ("variables", serde_json::to_string(&variables)?),
                    ("extensions", serde_json::to_string(&extensions)?),
                ])
                .header("Referer", "https://youtu-chan.com")
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

        // AllAnime now AES-256-CTR-encrypts responses; detect and unwrap.
        let json_str: std::borrow::Cow<str> = if text.contains("\"tobeparsed\"") {
            let enc: EncryptedEnvelope = serde_json::from_str(&text).map_err(|e| {
                anyhow!("failed to parse encrypted AllAnime envelope: {e}\nRaw:\n{text}")
            })?;
            let plaintext = decrypt_tobeparsed(&enc.data.tobeparsed)?;

            if std::env::var("ANV_DEBUG").is_ok() {
                eprintln!("[AllAnime] decrypted tobeparsed plaintext:\n{plaintext}");
            }

            // The plaintext is the inner data object; wrap it so it matches
            // GraphQlEnvelope<T> which expects {"data": {...}}.
            std::borrow::Cow::Owned(format!(r#"{{"data":{plaintext}}}"#))
        } else {
            std::borrow::Cow::Borrowed(&text)
        };

        let envelope: GraphQlEnvelope<T> = serde_json::from_str(&json_str).map_err(|e| {
            anyhow!("failed to parse AllAnime API response: {e}\nJSON:\n{json_str}")
        })?;

        Self::extract_data(envelope)
    }

    /// POST a GraphQL request to the AllAnime API and deserialize the `data` field.
    async fn post_graphql<T: DeserializeOwned>(&self, body: &serde_json::Value) -> Result<T> {
        let variables = body
            .get("variables")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let query = body.get("query").and_then(|v| v.as_str());
        self.execute_graphql(false, variables, query, None).await
    }

    async fn fetch_show_detail(&self, show_id: &str) -> Result<ShowDetail> {
        let variables = serde_json::json!({ "showId": show_id });
        let payload: ShowDetailPayload = self
            .execute_graphql(false, variables, Some(SHOW_DETAIL_QUERY), None)
            .await?;
        Ok(payload.show)
    }

    async fn fetch_episode_sources_internal(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<SourceDescriptor>> {
        let variables = serde_json::json!({
            "showId": show_id,
            "translationType": translation.as_str(),
            "episodeString": episode
        });

        let payload: EpisodePayload = self
            .execute_graphql(
                true,
                variables,
                None,
                Some(EPISODE_SOURCES_HASH),
            )
            .await?;
        Ok(payload.episode.source_urls)
    }

    async fn fetch_provider_json(&self, path: &str) -> Result<serde_json::Value> {
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

    async fn fetch_manga_detail(&self, manga_id: &str) -> Result<MangaDetail> {
        let body = serde_json::json!({
            "query": MANGA_DETAIL_QUERY,
            "variables": { "mangaId": manga_id }
        });
        let payload: MangaDetailPayload = self.post_graphql(&body).await?;
        Ok(payload.manga)
    }

    async fn fetch_single_provider_streams(
        &self,
        provider: &str,
        source_url: &str,
        debug: bool,
    ) -> Result<Vec<StreamOption>> {
        let decoded = if source_url.starts_with("http") || source_url.starts_with("/") {
            source_url.to_string()
        } else {
            match decode_provider_path(source_url) {
                Some(d) => d,
                None => {
                    if debug {
                        eprintln!(
                            "[ANV_DEBUG] fetch_streams: provider '{provider}' — failed to decode source URL {source_url:?}"
                        );
                    }
                    bail!("failed to decode source URL");
                }
            }
        };

        if debug {
            eprintln!("[ANV_DEBUG] fetch_streams: provider '{provider}' — working URL: {decoded}");
        }

        // Some providers (e.g. Yt-mp4 via fast4speed CDN) decode to an
        // absolute external URL that serves the HLS stream directly instead
        // of returning a clock.json JSON payload.
        let is_external = decoded.starts_with("http") && !decoded.contains("allanime.day");
        if is_external {
            if debug {
                eprintln!(
                    "[ANV_DEBUG] fetch_streams: provider '{provider}' — external URL detected; handling based on host"
                );
            }

            if decoded.contains("mp4upload.com") {
                if debug {
                    eprintln!(
                        "[ANV_DEBUG] fetch_streams: provider '{provider}' — Mp4Upload detected; scraping embed page"
                    );
                }
                let response = self
                    .client
                    .get(&decoded)
                    .header("Referer", ALLANIME_REFERER)
                    .send()
                    .await?
                    .text()
                    .await?;

                let re_mp4 = regex::Regex::new(r#"(?:src|file):\s*"([^"]+\.mp4[^"]*)""#).unwrap();
                if let Some(cap) = re_mp4.captures(&response) {
                    let mp4_url = cap[1].replace("\\u0026", "&").replace("\\", "");
                    if debug {
                        eprintln!(
                            "[ANV_DEBUG] fetch_streams: provider '{provider}' — scraped Mp4Upload URL: {mp4_url}"
                        );
                    }
                    return Ok(vec![StreamOption {
                        provider: provider.to_string(),
                        url: mp4_url,
                        quality_label: "auto".to_string(),
                        quality_rank: 0,
                        is_hls: false,
                        headers: {
                            let mut h = HashMap::new();
                            h.insert(
                                "Referer".to_string(),
                                "https://www.mp4upload.com/".to_string(),
                            );
                            h
                        },
                        subtitle: None,
                    }]);
                }
            }

            let mut headers = HashMap::new();
            headers.insert("Referer".to_string(), ALLANIME_REFERER.to_string());
            let option = StreamOption {
                provider: provider.to_string(),
                url: decoded,
                quality_label: "auto".to_string(),
                quality_rank: quality_rank("auto"),
                is_hls: true,
                headers,
                subtitle: None,
            };
            return Ok(vec![option]);
        }

        let json = match self.fetch_provider_json(&decoded).await {
            Ok(j) => j,
            Err(err) => {
                if debug {
                    eprintln!(
                        "[ANV_DEBUG] fetch_streams: provider '{provider}' — request failed: {err}"
                    );
                }
                bail!(err);
            }
        };

        let mut options: Vec<StreamOption> = if json.get("links").is_some() {
            let response: ClockResponse = serde_json::from_value(json)?;
            response
                .links
                .into_iter()
                .map(|link| build_stream_option(provider, link))
                .collect()
        } else if json.get("payload").is_some() {
            let response: FilemoonResponse = serde_json::from_value(json)?;
            let decrypted = decrypt_filemoon(&response)?;
            if debug {
                eprintln!("[AllAnime] decrypted Filemoon payload:\n{decrypted}");
            }
            // replace escaped characters as per ani-cli
            let decrypted = decrypted
                .replace("\\u0026", "&")
                .replace("\\u003D", "=")
                .replace("\\u002F", "/")
                .replace("\\/", "/");

            // Use regex to extract url and height from Filemoon payload
            // Since we need to pair them, and they might come in any order,
            // we'll find all "url" and all "height" and hope they match 1:1.
            // Actually, ani-cli's sed is better at pairing.
            let re_url = regex::Regex::new(r#""url"\s*:\s*"([^"]+)""#).unwrap();
            let re_height = regex::Regex::new(r#""height"\s*:\s*"?(\d+)"?"#).unwrap();

            let urls: Vec<_> = re_url
                .captures_iter(&decrypted)
                .map(|c| c[1].to_string())
                .collect();
            let heights: Vec<_> = re_height
                .captures_iter(&decrypted)
                .map(|c| c[1].parse::<i32>().unwrap_or(0))
                .collect();

            urls.into_iter()
                .zip(heights)
                .map(|(url, height)| StreamOption {
                    provider: provider.to_string(),
                    url,
                    quality_label: format!("{}p", height),
                    quality_rank: height,
                    is_hls: true,
                    headers: {
                        let mut h = HashMap::new();
                        h.insert("Referer".to_string(), ALLANIME_REFERER.to_string());
                        h
                    },
                    subtitle: None,
                })
                .collect()
        } else {
            if debug {
                eprintln!(
                    "[ANV_DEBUG] fetch_streams: provider '{provider}' — unknown JSON format: {json}"
                );
            }
            bail!("unknown provider response format");
        };

        if options.is_empty() {
            if debug {
                eprintln!("[ANV_DEBUG] fetch_streams: provider '{provider}' — returned 0 links");
            }
            bail!("returned 0 links");
        }

        options.sort_by(|a, b| b.quality_rank.cmp(&a.quality_rank));
        Ok(options)
    }

    fn extract_data<T>(envelope: GraphQlEnvelope<T>) -> Result<T> {
        if let Some(errors) = envelope.errors {
            let joined = errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            bail!("AllAnime API error: {joined}");
        }
        envelope
            .data
            .ok_or_else(|| anyhow!("AllAnime API returned empty response"))
    }
}

impl Default for AllAnimeClient {
    fn default() -> Self {
        Self::new(false, None).expect("failed to build HTTP client")
    }
}

impl AnimeProvider for AllAnimeClient {
    async fn search_shows(&self, query: &str, translation: Translation) -> Result<Vec<ShowInfo>> {
        let body = serde_json::json!({
            "query": SEARCH_SHOWS_QUERY,
            "variables": {
                "search": {
                    "allowAdult": false,
                    "allowUnknown": false,
                    "query": query,
                },
                "limit": 25,
                "page": 1,
                "translationType": translation.as_str(),
                "countryOrigin": "ALL"
            }
        });
        let payload: SearchPayload = self.post_graphql(&body).await?;
        Ok(payload
            .shows
            .edges
            .into_iter()
            .map(|edge| ShowInfo {
                id: edge.id,
                title: if self.prefer_english_titles {
                    edge.english_name
                        .filter(|s| !s.is_empty())
                        .unwrap_or(edge.name)
                } else {
                    edge.name
                },
                mal_id: edge.mal_id,
                available_eps: EpisodeCounts {
                    sub: edge.available_episodes.sub,
                    dub: edge.available_episodes.dub,
                },
            })
            .collect())
    }

    async fn fetch_episodes(&self, show_id: &str, translation: Translation) -> Result<Vec<String>> {
        let detail = self.fetch_show_detail(show_id).await?;
        let episodes = match translation {
            Translation::Sub => detail.available_episodes_detail.sub,
            Translation::Dub => detail.available_episodes_detail.dub,
            Translation::Raw => bail!("Raw translation is not supported for anime"),
        };
        Ok(episodes)
    }

    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>> {
        let sources = self
            .fetch_episode_sources_internal(show_id, translation, episode)
            .await?;

        let debug = std::env::var("ANV_DEBUG").is_ok();

        if debug {
            let names: Vec<&str> = sources.iter().map(|s| s.source_name.as_str()).collect();
            eprintln!(
                "[ANV_DEBUG] fetch_streams: show={show_id} ep={episode} translation={} — {} source(s) from API: {:?}",
                translation.as_str(),
                sources.len(),
                names
            );
        }

        let mut futures = FuturesUnordered::new();
        for provider_name in PREFERRED_PROVIDERS {
            if let Some(source) = sources.iter().find(|s| s.source_name == *provider_name) {
                futures.push(self.fetch_single_provider_streams(
                    provider_name,
                    &source.source_url,
                    debug,
                ));
            } else if debug {
                eprintln!(
                    "[ANV_DEBUG] fetch_streams: preferred provider '{provider_name}' not present in source list — skipping"
                );
            }
        }

        while let Some(res) = futures.next().await {
            if let Ok(options) = res {
                return Ok(options);
            }
        }

        if debug {
            eprintln!(
                "[ANV_DEBUG] fetch_streams: all preferred providers exhausted — returning empty stream list"
            );
            // Show which iframe-only providers were available but not attempted
            // (they require JS scraping that isn't implemented).
            let skipped: Vec<&str> = sources
                .iter()
                .filter(|s| !PREFERRED_PROVIDERS.contains(&s.source_name.as_str()))
                .map(|s| s.source_name.as_str())
                .collect();
            if !skipped.is_empty() {
                eprintln!(
                    "[ANV_DEBUG] fetch_streams: iframe-only providers present but not supported: {:?}",
                    skipped
                );
            }
        }

        Ok(Vec::new())
    }

    async fn fetch_mal_id(&self, show_id: &str) -> Result<Option<String>> {
        let detail = self.fetch_show_detail(show_id).await?;
        Ok(detail.mal_id)
    }
}

impl MangaProvider for AllAnimeClient {
    async fn search_mangas(&self, query: &str, translation: Translation) -> Result<Vec<MangaInfo>> {
        let body = serde_json::json!({
            "query": SEARCH_MANGAS_QUERY,
            "variables": {
                "search": {
                    "allowAdult": false,
                    "allowUnknown": false,
                    "query": query,
                },
                "limit": 25,
                "page": 1,
                "translationType": translation.as_str(),
                "countryOrigin": "ALL"
            }
        });
        let payload: SearchMangaPayload = self.post_graphql(&body).await?;
        Ok(payload
            .mangas
            .edges
            .into_iter()
            .map(|edge| MangaInfo {
                id: edge.id,
                title: edge.name,
                available_chapters: ChapterCounts {
                    sub: edge.available_chapters.sub,
                    raw: edge.available_chapters.raw,
                },
            })
            .collect())
    }

    async fn fetch_chapters(
        &self,
        manga_id: &str,
        translation: Translation,
    ) -> Result<Vec<Chapter>> {
        let detail = self.fetch_manga_detail(manga_id).await?;
        let raw_chapters = match translation {
            Translation::Sub => detail.available_chapters_detail.sub,
            Translation::Raw => detail.available_chapters_detail.raw,
            Translation::Dub => bail!("Dub translation is not supported for manga"),
        };
        Ok(raw_chapters
            .into_iter()
            .map(|ch| Chapter {
                id: ch.clone(),
                label: ch,
            })
            .collect())
    }

    async fn fetch_pages(
        &self,
        manga_id: &str,
        translation: Translation,
        chapter_id: &str,
    ) -> Result<Vec<Page>> {
        let body = serde_json::json!({
            "query": CHAPTER_PAGES_QUERY,
            "variables": {
                "mangaId": manga_id,
                "translationType": translation.as_str(),
                "chapterString": chapter_id
            }
        });
        let payload: ChapterPagesPayload = self.post_graphql(&body).await?;
        Ok(if let Some(edge) = payload.chapter_pages.edges.first() {
            let head = &edge.picture_url_head;
            edge.picture_urls
                .iter()
                .map(|p| {
                    let url = if p.url.starts_with("http") {
                        p.url.clone()
                    } else {
                        format!("{}{}", head, p.url)
                    };
                    let mut headers = HashMap::new();
                    headers.insert("Referer".to_string(), ALLANIME_IMAGE_REFERER.to_string());
                    headers.insert("Origin".to_string(), ALLANIME_IMAGE_REFERER.to_string());
                    Page { url, headers }
                })
                .collect()
        } else {
            Vec::new()
        })
    }
}

fn build_stream_option(provider: &str, link: ClockLink) -> StreamOption {
    let quality_label = link
        .resolution
        .clone()
        .unwrap_or_else(|| String::from("auto"));
    let quality_rank = quality_rank(&quality_label);
    let subtitle = link
        .subtitles
        .iter()
        .find(|sub| sub.lang.as_deref() == Some("en") || sub.label.as_deref() == Some("English"))
        .map(|sub| sub.src.clone());

    let mut headers = link.headers;
    if !headers.keys().any(|k| k.eq_ignore_ascii_case("referer")) {
        headers.insert("Referer".to_string(), ALLANIME_REFERER.to_string());
    }

    StreamOption {
        provider: provider.to_string(),
        url: link.link,
        quality_label,
        quality_rank,
        is_hls: link.hls,
        headers,
        subtitle,
    }
}

fn quality_rank(label: &str) -> i32 {
    if label.eq_ignore_ascii_case("auto") {
        return 10_000;
    }
    label.trim_end_matches('p').parse::<i32>().unwrap_or(0)
}

fn decode_provider_path(raw: &str) -> Option<String> {
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

/// Decodes a two-hex-digit string to its corresponding URL character.
///
/// The encoding is a simple XOR cipher: `byte ^ 0x38`, where `byte` is the
/// hex-decoded value of the two-character pair.
fn decode_pair(pair: &str) -> Option<char> {
    let byte = u8::from_str_radix(pair, 16).ok()?;
    let ch = (byte ^ 0x38) as char;
    // Only emit printable (graphic) ASCII — control characters have no place in URLs.
    ch.is_ascii_graphic().then_some(ch)
}

/// Decrypts the `tobeparsed` blob returned by the AllAnime API.
///
/// Layout: `base64( prefix[1] || nonce[12] || ciphertext || tag[16] )`
///
/// AllAnime encrypts the response with either the aaReq key (`ALLANIME_CRYPTO_KEY`)
/// or a static legacy key (`ALLANIME_RESPONSE_STATIC_KEY`), depending on the rotation.
/// Both are tried; the first that authenticates successfully is used.
/// See: anipy-cli PR #335.
fn decrypt_tobeparsed(blob: &str) -> Result<String> {
    let raw = B64
        .decode(blob)
        .map_err(|e| anyhow!("tobeparsed base64 decode failed: {e}"))?;
    // Layout: [1-byte prefix][12-byte nonce][ciphertext][16-byte GCM tag]
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

    // Try both the aaReq key and the static legacy key. AllAnime rotates between them.
    let candidate_keys: &[&[u8; 32]] = &[&ALLANIME_CRYPTO_KEY, &ALLANIME_RESPONSE_STATIC_KEY];
    for key in candidate_keys {
        let cipher = Aes256Gcm::new_from_slice(*key)
            .map_err(|e| anyhow!("failed to initialize AES-GCM: {e}"))?;
        if let Ok(plaintext) = cipher.decrypt(nonce, ciphertext_and_tag) {
            return String::from_utf8(plaintext)
                .map_err(|e| anyhow!("tobeparsed plaintext is not valid UTF-8: {e}"));
        }
    }

    bail!("AES-GCM decryption failed: tobeparsed could not be decrypted with any known key")
}

#[derive(serde::Serialize)]
struct AaReqPayload<'a> {
    v: i32,
    ts: u64,
    epoch: i32,
    #[serde(rename = "buildId")]
    build_id: &'a str,
    qh: &'a str,
}

fn build_aa_req(qh: &str) -> Result<String> {
    let key = &ALLANIME_CRYPTO_KEY;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("SystemTime before UNIX EPOCH: {e}"))?
        .as_millis();
    let ts = (now_ms / 300_000) * 300_000;
    let ts_u64 = ts as u64;

    let payload = serde_json::to_string(&AaReqPayload {
        v: 1,
        ts: ts_u64,
        epoch: 4128,
        build_id: "9",
        qh,
    })?;

    let iv_input = format!("4128:9:{}:{}", qh, ts_u64);
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

/// Decrypts the Filemoon payload using AES-256-CTR.
fn decrypt_filemoon(resp: &FilemoonResponse) -> Result<String> {
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

    // Build the 128-bit CTR IV: nonce (96 bits) || counter=2 (32 bits, big-endian).
    let mut iv = [0u8; 16];
    iv[..12].copy_from_slice(&iv_raw[..12]);
    iv[15] = 0x02;

    let mut plaintext = ciphertext;
    let mut cipher = Ctr32BE::<Aes256>::new(key.as_slice().into(), &iv.into());
    cipher.apply_keystream(&mut plaintext);

    String::from_utf8(plaintext).map_err(|e| anyhow!("filemoon plaintext is not valid UTF-8: {e}"))
}

/// Wrapper for the encrypted envelope: `{"data": {"_m": "...", "tobeparsed": "<base64>"}}`
#[derive(Debug, Deserialize)]
struct EncryptedEnvelope {
    data: EncryptedData,
}

#[derive(Debug, Deserialize)]
struct EncryptedData {
    tobeparsed: String,
}

#[derive(Debug, Deserialize)]
struct FilemoonResponse {
    iv: String,
    payload: String,
    #[serde(rename = "key_parts")]
    key_parts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GraphQlEnvelope<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct SearchPayload {
    shows: SearchShows,
}

#[derive(Debug, Deserialize)]
struct SearchShows {
    edges: Vec<SearchEdge>,
}

#[derive(Debug, Deserialize, Clone)]
struct SearchEdge {
    #[serde(rename = "_id")]
    id: String,
    name: String,
    #[serde(rename = "englishName")]
    english_name: Option<String>,
    #[serde(rename = "malId")]
    mal_id: Option<String>,
    #[serde(rename = "availableEpisodes")]
    #[serde(default)]
    available_episodes: AvailabilitySnapshot,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct AvailabilitySnapshot {
    #[serde(default)]
    sub: usize,
    #[serde(default)]
    dub: usize,
}

#[derive(Debug, Deserialize)]
struct SearchMangaPayload {
    mangas: SearchMangas,
}

#[derive(Debug, Deserialize)]
struct SearchMangas {
    edges: Vec<SearchMangaEdge>,
}

#[derive(Debug, Deserialize, Clone)]
struct SearchMangaEdge {
    #[serde(rename = "_id")]
    id: String,
    name: String,
    #[serde(rename = "availableChapters")]
    #[serde(default)]
    available_chapters: ChapterAvailabilitySnapshot,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct ChapterAvailabilitySnapshot {
    #[serde(default)]
    sub: usize,
    #[serde(default)]
    raw: usize,
}

#[derive(Debug, Deserialize)]
struct MangaDetailPayload {
    manga: MangaDetail,
}

#[derive(Debug, Deserialize)]
struct MangaDetail {
    #[serde(rename = "availableChaptersDetail")]
    #[serde(default)]
    available_chapters_detail: ChapterDetail,
}

#[derive(Debug, Deserialize, Default)]
struct ChapterDetail {
    #[serde(default)]
    sub: Vec<String>,
    #[serde(default)]
    raw: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ChapterPagesPayload {
    #[serde(rename = "chapterPages")]
    chapter_pages: ChapterPagesConnection,
}

#[derive(Debug, Deserialize)]
struct ChapterPagesConnection {
    edges: Vec<ChapterPageEdge>,
}

#[derive(Debug, Deserialize)]
struct ChapterPageEdge {
    #[serde(rename = "pictureUrlHead")]
    picture_url_head: String,
    #[serde(rename = "pictureUrls")]
    picture_urls: Vec<PictureUrl>,
}

#[derive(Debug, Deserialize)]
struct PictureUrl {
    url: String,
}

#[derive(Debug, Deserialize)]
struct ShowDetailPayload {
    show: ShowDetail,
}

#[derive(Debug, Deserialize)]
struct ShowDetail {
    #[serde(rename = "malId")]
    mal_id: Option<String>,
    #[serde(rename = "availableEpisodesDetail")]
    #[serde(default)]
    available_episodes_detail: EpisodeDetail,
}

#[derive(Debug, Deserialize, Default)]
struct EpisodeDetail {
    #[serde(default)]
    sub: Vec<String>,
    #[serde(default)]
    dub: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EpisodePayload {
    episode: EpisodeSources,
}

#[derive(Debug, Deserialize)]
struct EpisodeSources {
    #[serde(rename = "sourceUrls")]
    source_urls: Vec<SourceDescriptor>,
}

#[derive(Debug, Deserialize)]
struct SourceDescriptor {
    #[serde(rename = "sourceUrl")]
    source_url: String,
    #[serde(rename = "sourceName")]
    source_name: String,
}

#[derive(Debug, Deserialize)]
struct ClockResponse {
    links: Vec<ClockLink>,
}

#[derive(Debug, Deserialize)]
struct ClockLink {
    link: String,
    #[serde(rename = "resolutionStr")]
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    hls: bool,
    #[serde(default)]
    subtitles: Vec<ClockSubtitle>,
    #[serde(default)]
    headers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ClockSubtitle {
    src: String,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

const SEARCH_SHOWS_QUERY: &str = r#"query($search: SearchInput, $limit: Int, $page: Int, $translationType: VaildTranslationTypeEnumType, $countryOrigin: VaildCountryOriginEnumType) {
  shows(search: $search, limit: $limit, page: $page, translationType: $translationType, countryOrigin: $countryOrigin) {
    edges {
      _id
      name
      englishName
      malId
      availableEpisodes
    }
  }
}"#;

const SHOW_DETAIL_QUERY: &str = r#"query($showId: String!) {
  show(_id: $showId) {
    _id
    name
    malId
    availableEpisodesDetail
  }
}"#;


const SEARCH_MANGAS_QUERY: &str = r#"query($search: SearchInput, $limit: Int, $page: Int, $translationType: VaildTranslationTypeMangaEnumType, $countryOrigin: VaildCountryOriginEnumType) {
  mangas(search: $search, limit: $limit, page: $page, translationType: $translationType, countryOrigin: $countryOrigin) {
    edges {
      _id
      name
      availableChapters
    }
  }
}"#;

const MANGA_DETAIL_QUERY: &str = r#"query($mangaId: String!) {
  manga(_id: $mangaId) {
    availableChaptersDetail
  }
}"#;

const CHAPTER_PAGES_QUERY: &str = r#"query($mangaId: String!, $translationType: VaildTranslationTypeMangaEnumType!, $chapterString: String!) {
  chapterPages(mangaId: $mangaId, translationType: $translationType, chapterString: $chapterString) {
    edges {
      pictureUrlHead
      pictureUrls
    }
  }
}"#;
