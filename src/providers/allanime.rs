use aes::Aes256;
use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use ctr::Ctr32BE;
use ctr::cipher::{KeyIvInit, StreamCipher};
use reqwest::Client;
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use super::{AnimeProvider, MangaProvider, USER_AGENT};
use crate::types::{
    Chapter, ChapterCounts, EpisodeCounts, MangaInfo, Page, ShowInfo, StreamOption, Translation,
};

const ALLANIME_API_URL: &str = "https://api.allanime.day/api";
const ALLANIME_BASE_URL: &str = "https://allanime.day";
const ALLANIME_REFERER: &str = "https://allmanga.to";
const ALLANIME_IMAGE_REFERER: &str = "https://allanime.to";
const ALLANIME_ORIGIN: &str = "https://allanime.day";
// Providers known to yield direct HLS/MP4 URLs via the clock.json mechanism.
// The remaining providers (Ok, Vg, Fm-Hls, Mp4, Sw, …) are JS-obfuscated iframe
// embeds that require per-provider HTML/JS scraping to extract a playable URL —
// not currently implemented. Luf-Mp4 and Yt-mp4 cover the vast majority of shows.
const PREFERRED_PROVIDERS: &[&str] = &["Default", "S-mp4", "Luf-Mp4", "Yt-mp4"];

pub struct AllAnimeClient {
    client: Client,
}

impl AllAnimeClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder().user_agent(USER_AGENT).build()?;
        Ok(Self { client })
    }

    /// POST a GraphQL request to the AllAnime API and deserialize the `data` field.
    async fn post_graphql<T: DeserializeOwned>(&self, body: &serde_json::Value) -> Result<T> {
        let response = self
            .client
            .post(ALLANIME_API_URL)
            .header("Referer", ALLANIME_REFERER)
            .header("Origin", ALLANIME_ORIGIN)
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            bail!("AllAnime API HTTP {status}: {text}");
        }
        if std::env::var("ANV_DEBUG").is_ok() {
            eprintln!("[AllAnime] HTTP {status} — raw response:\n{text}");
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
            anyhow!(
                "failed to parse AllAnime API response: {e}\nJSON:\n{json_str}"
            )
        })?;
        Self::extract_data(envelope)
    }

    async fn fetch_show_detail(&self, show_id: &str) -> Result<ShowDetail> {
        let body = serde_json::json!({
            "query": SHOW_DETAIL_QUERY,
            "variables": { "showId": show_id }
        });
        let payload: ShowDetailPayload = self.post_graphql(&body).await?;
        Ok(payload.show)
    }

    async fn fetch_episode_sources_internal(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<SourceDescriptor>> {
        let body = serde_json::json!({
            "query": EPISODE_SOURCES_QUERY,
            "variables": {
                "showId": show_id,
                "translationType": translation.as_str(),
                "episodeString": episode
            }
        });
        let payload: EpisodePayload = self.post_graphql(&body).await?;
        Ok(payload.episode.source_urls)
    }

    async fn fetch_clock_json(&self, path: &str) -> Result<ClockResponse> {
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
            .json::<ClockResponse>()
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
        Self::new().expect("failed to build HTTP client")
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
                title: edge.name,
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

        for provider in PREFERRED_PROVIDERS {
            let source = match sources.iter().find(|s| s.source_name == *provider) {
                Some(s) => s,
                None => {
                    if debug {
                        eprintln!("[ANV_DEBUG] fetch_streams: preferred provider '{provider}' not present in source list — skipping");
                    }
                    continue;
                }
            };

            let decoded = match decode_provider_path(&source.source_url) {
                Some(d) => d,
                None => {
                    if debug {
                        eprintln!(
                            "[ANV_DEBUG] fetch_streams: provider '{provider}' — failed to decode source URL {:?}",
                            source.source_url
                        );
                    }
                    continue;
                }
            };

            if debug {
                eprintln!("[ANV_DEBUG] fetch_streams: provider '{provider}' — decoded clock URL: {decoded}");
            }

            let response = match self.fetch_clock_json(&decoded).await {
                Ok(r) => r,
                Err(err) => {
                    let err_str = err.to_string();
                    // Some providers (e.g. Yt-mp4 via fast4speed CDN) decode to an
                    // absolute external URL that serves the HLS stream directly instead
                    // of returning a clock.json JSON payload. Detect a JSON decode
                    // failure on an external URL and treat the URL itself as the stream.
                    let is_json_err = err_str.contains("error decoding response body")
                        || err_str.contains("expected value")
                        || err_str.contains("invalid type");
                    let is_external = decoded.starts_with("http")
                        && !decoded.contains("allanime.day");
                    if is_json_err && is_external {
                        if debug {
                            eprintln!(
                                "[ANV_DEBUG] fetch_streams: provider '{provider}' — clock returned non-JSON; treating decoded URL as direct HLS stream"
                            );
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
                    if debug {
                        eprintln!(
                            "[ANV_DEBUG] fetch_streams: provider '{provider}' — clock request failed: {err}"
                        );
                    }
                    continue;
                }
            };

            let mut options = Vec::new();
            for link in response.links {
                options.push(build_stream_option(&source.source_name, link));
            }

            if options.is_empty() {
                if debug {
                    eprintln!(
                        "[ANV_DEBUG] fetch_streams: provider '{provider}' — clock returned 0 links"
                    );
                }
                continue;
            }

            options.sort_by(|a, b| b.quality_rank.cmp(&a.quality_rank));
            return Ok(options);
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
/// Layout (as of PR #1667): `base64( prefix[1] || nonce[12] || ciphertext || tag[16] )`
/// - Key = SHA-256("Xot36i3lK3:v1")
/// - CTR IV = nonce[0..12] ++ 0x00_00_00_02
fn decrypt_tobeparsed(blob: &str) -> Result<String> {
    let key = Sha256::digest(b"Xot36i3lK3:v1");

    let raw = B64.decode(blob).map_err(|e| anyhow!("tobeparsed base64 decode failed: {e}"))?;
    // Layout: [1-byte prefix][12-byte nonce][ciphertext][16-byte GCM tag]
    if raw.len() < 1 + 12 + 16 {
        bail!("tobeparsed blob too short ({} bytes)", raw.len());
    }

    let nonce = &raw[1..13];
    let ciphertext = &raw[13..raw.len() - 16];

    // Build the 128-bit CTR IV: nonce (96 bits) || counter=2 (32 bits, big-endian).
    let mut iv = [0u8; 16];
    iv[..12].copy_from_slice(nonce);
    iv[15] = 0x02;

    let mut plaintext = ciphertext.to_vec();
    let key_arr: &[u8; 32] = key.as_ref();
    let mut cipher = Ctr32BE::<Aes256>::new(key_arr.into(), &iv.into());
    cipher.apply_keystream(&mut plaintext);

    String::from_utf8(plaintext)
        .map_err(|e| anyhow!("tobeparsed plaintext is not valid UTF-8: {e}"))
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
      availableEpisodes
    }
  }
}"#;

const SHOW_DETAIL_QUERY: &str = r#"query($showId: String!) {
  show(_id: $showId) {
    _id
    name
    availableEpisodesDetail
  }
}"#;

const EPISODE_SOURCES_QUERY: &str = r#"query($showId: String!, $translationType: VaildTranslationTypeEnumType!, $episodeString: String!) {
  episode(showId: $showId, translationType: $translationType, episodeString: $episodeString) {
    episodeString
        sourceUrls
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
