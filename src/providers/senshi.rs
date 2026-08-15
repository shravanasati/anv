use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use url::Url;

use crate::dbg_log;
use crate::providers::{AUTO_QUALITY_LABEL, AUTO_QUALITY_RANK, AnimeProvider};
use crate::types::{EpisodeCounts, Provider, ShowInfo, StreamOption, Translation};

const BASE_URL: &str = "https://senshi.live";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct SenshiClient {
    client: Client,
}

impl SenshiClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent(USER_AGENT)
            .build()?;

        Ok(Self { client })
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(
        &self,
        method: reqwest::Method,
        url: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<T> {
        dbg_log!("senshi", "request: {method} {url}");

        let mut req = self
            .client
            .request(method, url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", format!("{}/", BASE_URL));

        if let Some(body) = payload {
            req = req.json(body);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("Senshi request to {url} failed"))?;
        let status = resp.status();
        if !status.is_success() {
            dbg_log!(
                "senshi",
                "request to {url} failed with HTTP status {status}"
            );
            bail!("Senshi request to {url} failed with status {status}");
        }

        let raw_text = resp
            .text()
            .await
            .with_context(|| format!("failed to read Senshi response from {url}"))?;
        dbg_log!(
            "senshi",
            "response from {url} (len={}): {}",
            raw_text.len(),
            if raw_text.len() > 500 {
                &raw_text[..500]
            } else {
                &raw_text
            }
        );

        let data = serde_json::from_str::<T>(&raw_text)
            .with_context(|| format!("failed to parse Senshi JSON from {url}"))?;
        Ok(data)
    }

    async fn resolve_senshi_subtitle(&self, item: &EmbedItem) -> Option<String> {
        let manifest_url = senshi_subtitle_manifest_url(item)?;
        dbg_log!("senshi", "subtitle manifest_url: {manifest_url}");
        let tracks: Vec<SenshiSubtitleTrack> = self
            .fetch_json(reqwest::Method::GET, &manifest_url, None)
            .await
            .ok()?;

        dbg_log!("senshi", "subtitle tracks count: {}", tracks.len());
        let selected = pick_senshi_subtitle_track(&tracks)?;
        dbg_log!("senshi", "selected subtitle track: {selected}");
        Some(self.prepare_senshi_subtitle(&selected).await)
    }

    async fn prepare_senshi_subtitle(&self, subtitle_url: &str) -> String {
        let subtitle_url = subtitle_url.trim();
        if subtitle_url.is_empty() {
            return String::new();
        }

        if let Some(styled) = self.validated_senshi_ass_url(subtitle_url).await {
            return styled;
        }

        subtitle_url.to_string()
    }

    async fn validated_senshi_ass_url(&self, subtitle_url: &str) -> Option<String> {
        let parsed = Url::parse(subtitle_url).ok()?;
        if !parsed.path().ends_with(".vtt") {
            return None;
        }

        let ass_path = parsed.path().trim_end_matches(".vtt").to_string() + ".ass";
        let mut ass_url = parsed.clone();
        ass_url.set_path(&ass_path);
        let candidate = ass_url.to_string();

        let resp = self
            .client
            .get(&candidate)
            .header("User-Agent", USER_AGENT)
            .header("Referer", format!("{}/", BASE_URL))
            .header("Range", "bytes=0-8191")
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        let bytes = resp.bytes().await.ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim_start_matches('\u{feff}').trim();

        if text.contains("[Script Info]") && text.contains("[V4+ Styles]") {
            Some(candidate)
        } else {
            None
        }
    }
}

impl Default for SenshiClient {
    fn default() -> Self {
        Self::new().expect("failed to create default SenshiClient")
    }
}

#[derive(Deserialize)]
struct FilterResponse {
    #[serde(default)]
    data: Vec<AnimeItem>,
    #[serde(default)]
    _total: usize,
}

#[derive(Deserialize)]
struct AnimeItem {
    id: usize,
    #[serde(default)]
    title: String,
    #[serde(default)]
    title_english: String,
    #[serde(default)]
    _type: String,
    #[serde(default)]
    ani_episodes: String,
}

#[derive(Deserialize)]
struct EpisodeItem {
    id: usize,
    #[serde(default)]
    ep_id: usize,
}

#[derive(Deserialize)]
struct EmbedItem {
    url: String,
    #[serde(rename = "serverFM")]
    server_fm: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    masked_base_url: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
struct SenshiSubtitleTrack {
    #[serde(default)]
    src: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    default: bool,
}

impl AnimeProvider for SenshiClient {
    async fn search_shows(&self, query: &str, _translation: Translation) -> Result<Vec<ShowInfo>> {
        let query = query.trim();
        if query.is_empty() {
            bail!("empty search query");
        }

        let payload = serde_json::json!({
            "searchTerm": query,
            "page": 1,
            "limit": 25,
        });

        let url = format!("{}/anime/filter", BASE_URL);
        let resp: FilterResponse = self
            .fetch_json(reqwest::Method::POST, &url, Some(&payload))
            .await?;

        dbg_log!("senshi", "search_shows results count: {}", resp.data.len());

        if resp.data.is_empty() {
            bail!("no results for '{query}'");
        }

        let mut shows = Vec::new();
        for item in resp.data {
            if item.id == 0 {
                continue;
            }

            let mut title = item.title_english.trim().to_string();
            if title.is_empty() {
                title = item.title.trim().to_string();
            }
            if title.is_empty() {
                continue;
            }

            let ep_count = parse_episode_count(&item.ani_episodes).unwrap_or(0);

            dbg_log!(
                "senshi",
                "  show id={} title='{}' ep_count={ep_count}",
                item.id,
                title
            );

            shows.push(ShowInfo {
                id: item.id.to_string(),
                title,
                mal_id: Some(item.id.to_string()),
                available_eps: EpisodeCounts {
                    sub: ep_count,
                    dub: ep_count,
                },
            });
        }

        if shows.is_empty() {
            bail!("no results for '{query}'");
        }

        Ok(shows)
    }

    async fn fetch_episodes(
        &self,
        show_id: &str,
        _translation: Translation,
    ) -> Result<Vec<String>> {
        let mal_id = parse_mal_id(show_id)?;
        let url = format!("{}/episodes/{}", BASE_URL, mal_id);
        let episodes: Vec<EpisodeItem> = self.fetch_json(reqwest::Method::GET, &url, None).await?;

        dbg_log!(
            "senshi",
            "fetch_episodes mal_id={mal_id} count={}",
            episodes.len()
        );

        if episodes.is_empty() {
            bail!("no episodes found for mal id {mal_id}");
        }

        let mut seen = std::collections::BTreeSet::new();
        for ep in episodes {
            let ep_no = if ep.ep_id > 0 { ep.ep_id } else { ep.id };
            if ep_no > 0 {
                seen.insert(ep_no);
            }
        }

        if seen.is_empty() {
            bail!("no valid episodes found for mal id {mal_id}");
        }

        let result: Vec<String> = seen.into_iter().map(|n| n.to_string()).collect();
        dbg_log!("senshi", "parsed episodes: {:?}", result);

        Ok(result)
    }

    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>> {
        let mal_id = parse_mal_id(show_id)?;
        let ep_no: usize = episode
            .parse()
            .with_context(|| format!("invalid episode number '{episode}'"))?;

        let url = format!("{}/episode-embeds/{}/{}", BASE_URL, mal_id, ep_no);
        let embeds: Vec<EmbedItem> = self.fetch_json(reqwest::Method::GET, &url, None).await?;

        dbg_log!(
            "senshi",
            "fetch_streams mal_id={mal_id} ep_no={ep_no} translation={} embeds_count={}",
            translation.as_str(),
            embeds.len()
        );
        for (i, item) in embeds.iter().enumerate() {
            dbg_log!(
                "senshi",
                "  embed[{i}] status='{}' url='{}' serverFM={:?} masked_base_url='{}'",
                item.status,
                item.url,
                item.server_fm,
                item.masked_base_url
            );
        }

        if embeds.is_empty() {
            bail!("no streams found for episode {ep_no}");
        }

        let mut streams = Vec::new();
        for item in embeds {
            let matches_status = match translation {
                Translation::Dub => item.status.trim().eq_ignore_ascii_case("dub"),
                _ => is_sub_embed_status(&item.status),
            };

            if !matches_status {
                dbg_log!(
                    "senshi",
                    "  skipping embed status='{}' (wanted translation={})",
                    item.status,
                    translation.as_str()
                );
                continue;
            }

            let stream_url = item.url.trim().to_string();
            if stream_url.is_empty() {
                dbg_log!("senshi", "  skipping embed with empty URL");
                continue;
            }

            let subtitle = if translation == Translation::Sub {
                self.resolve_senshi_subtitle(&item).await
            } else {
                None
            };

            let mut headers = HashMap::new();
            headers.insert("Referer".to_string(), format!("{}/", BASE_URL));
            headers.insert("User-Agent".to_string(), USER_AGENT.to_string());

            streams.push(StreamOption {
                provider: Provider::Senshi.display_name().to_string(),
                url: stream_url,
                quality_label: AUTO_QUALITY_LABEL.to_string(),
                quality_rank: AUTO_QUALITY_RANK,
                is_hls: true,
                headers,
                subtitle,
            });
        }

        if streams.is_empty() {
            dbg_log!(
                "senshi",
                "fetch_streams: 0 streams matched status filter for translation {}",
                translation.as_str()
            );
            bail!(
                "no {} streams found for episode {}",
                translation.as_str(),
                ep_no
            );
        }

        dbg_log!("senshi", "returning {} stream option(s)", streams.len());

        Ok(streams)
    }

    async fn fetch_mal_id(&self, show_id: &str) -> Result<Option<String>> {
        let mal_id = parse_mal_id(show_id)?;
        Ok(Some(mal_id.to_string()))
    }
}

fn is_sub_embed_status(status: &str) -> bool {
    let s = status.trim().to_lowercase();
    matches!(
        s.as_str(),
        "hardsub" | "softsub" | "sub" | "hard_sub" | "soft_sub" | "subtitled"
    )
}

fn parse_mal_id(show_id: &str) -> Result<usize> {
    let show_id = show_id.trim();
    if show_id.is_empty() {
        bail!("empty show id");
    }
    show_id
        .parse::<usize>()
        .with_context(|| format!("invalid senshi mal id '{show_id}'"))
}

fn parse_episode_count(raw: &str) -> Option<usize> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "?" {
        return None;
    }
    raw.parse::<usize>().ok().filter(|&n| n > 0)
}

fn senshi_subtitle_manifest_url(item: &EmbedItem) -> Option<String> {
    if let Some(server_fm) = &item.server_fm {
        if let Some(manifest) = subtitle_info_from_url(server_fm.trim()) {
            return Some(manifest);
        }
    }

    let base = item.masked_base_url.trim();
    if base.is_empty() {
        None
    } else {
        Some(format!("{}/sub_filemoon.json", base.trim_end_matches('/')))
    }
}

fn subtitle_info_from_url(raw_url: &str) -> Option<String> {
    if raw_url.is_empty() {
        return None;
    }
    let parsed = Url::parse(raw_url).ok()?;
    let val = parsed
        .query_pairs()
        .find(|(k, _)| k == "sub.info")
        .map(|(_, v)| v.trim().to_string())?;

    if val.is_empty() { None } else { Some(val) }
}

fn pick_senshi_subtitle_track(tracks: &[SenshiSubtitleTrack]) -> Option<String> {
    for track in tracks {
        let file = track.src.trim();
        if !file.is_empty() && track.default && !is_senshi_forced_subtitle_label(&track.label) {
            return Some(file.to_string());
        }
    }

    for track in tracks {
        let file = track.src.trim();
        let label = track.label.trim().to_lowercase();
        if !file.is_empty() && label.contains("eng") && !is_senshi_forced_subtitle_label(&label) {
            return Some(file.to_string());
        }
    }

    for track in tracks {
        let file = track.src.trim();
        if !file.is_empty() && track.default {
            return Some(file.to_string());
        }
    }

    for track in tracks {
        let file = track.src.trim();
        let label = track.label.trim().to_lowercase();
        if !file.is_empty() && label.contains("eng") {
            return Some(file.to_string());
        }
    }

    for track in tracks {
        let file = track.src.trim();
        if !file.is_empty() {
            return Some(file.to_string());
        }
    }

    None
}

fn is_senshi_forced_subtitle_label(label: &str) -> bool {
    let label = label.trim().to_lowercase();
    label.contains("forced") || label.contains("sign") || label.contains("song")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mal_id() {
        assert_eq!(parse_mal_id("52991").unwrap(), 52991);
        assert!(parse_mal_id("abc").is_err());
        assert!(parse_mal_id("").is_err());
    }

    #[test]
    fn test_parse_episode_count() {
        assert_eq!(parse_episode_count("28"), Some(28));
        assert_eq!(parse_episode_count("?"), None);
        assert_eq!(parse_episode_count("0"), None);
    }

    #[test]
    fn test_pick_senshi_subtitle_track() {
        let tracks = vec![
            SenshiSubtitleTrack {
                src: "http://example.com/signs.vtt".to_string(),
                label: "English (Signs & Songs)".to_string(),
                default: true,
            },
            SenshiSubtitleTrack {
                src: "http://example.com/full.vtt".to_string(),
                label: "English".to_string(),
                default: false,
            },
        ];

        assert_eq!(
            pick_senshi_subtitle_track(&tracks),
            Some("http://example.com/full.vtt".to_string())
        );
    }

    #[test]
    fn test_subtitle_info_from_url() {
        let url =
            "https://filemoon.sx/e/12345?sub.info=https%3A%2F%2Fsenshi.live%2Fsubs%2F123.json";
        assert_eq!(
            subtitle_info_from_url(url),
            Some("https://senshi.live/subs/123.json".to_string())
        );
    }
}
