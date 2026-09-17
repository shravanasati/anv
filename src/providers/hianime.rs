use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};

use crate::dbg_log;
use crate::providers::{AnimeProvider, USER_AGENT};
use crate::types::{EpisodeCounts, Provider, ShowInfo, StreamOption, Translation};

pub const HIANIME_BASE_URL: &str = "https://hianime.at";
pub const HIANIME_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const DEOBFUSCATION_KEY: &[u8] = b"otaku-embed-v1";

static RE_EP_PAIR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-number="([^"]+)"[^>]*data-id="([0-9]+)""#).unwrap());
static RE_BLOB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"window\.__P="([^"]*)""#).unwrap());
static RE_MAL_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"/mal/([0-9]+)/"#).unwrap());
static RE_NUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(\d+)"#).unwrap());
static RE_M3U8_SRC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""src"\s*:\s*"([^"]*\.m3u8[^"]*)""#).unwrap());

#[derive(Debug, Clone)]
pub struct HianimeClient {
    client: Client,
}

impl HianimeClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(HIANIME_USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { client })
    }

    async fn fetch_string(&self, url: &str, referer: Option<&str>) -> Result<String> {
        dbg_log!("hianime", "GET {url}");
        let mut req = self
            .client
            .get(url)
            .header("User-Agent", HIANIME_USER_AGENT);
        if let Some(r) = referer {
            req = req.header("Referer", r);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            bail!("hianime request to {url} failed with status {status}");
        }
        let text = resp.text().await?;
        if text.contains("<title>Just a moment") {
            bail!("blocked by Cloudflare. Try again later");
        }
        Ok(text)
    }

    /// Fetch `(episode_number, episode_id)` pairs for a show.
    async fn fetch_episode_pairs(&self, show_id: &str) -> Result<Vec<(String, String)>> {
        let numeric = numeric_id(show_id);
        if numeric.is_empty() {
            bail!("invalid hianime show id '{show_id}'");
        }
        let url = format!("{HIANIME_BASE_URL}/api/theme/episode/list/{numeric}");
        let body = self
            .fetch_string(&url, Some(&format!("{HIANIME_BASE_URL}/{show_id}")))
            .await?;
        let pairs = parse_episodes_payload(&body);
        if pairs.is_empty() {
            bail!("no episodes found for \"{show_id}\"");
        }
        Ok(pairs)
    }

    /// Resolve the embed URL + referer + MAL id for one episode/mode.
    async fn resolve_embed(&self, show_id: &str, episode: &str, mode: &str) -> Result<EmbedInfo> {
        let pairs = self.fetch_episode_pairs(show_id).await?;
        let ep_id = pairs
            .iter()
            .find(|(num, _)| num == episode)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| anyhow!("episode {episode} not released"))?;
        fetch_embed_info(&self.client, &ep_id, mode).await
    }
}

impl Default for HianimeClient {
    fn default() -> Self {
        Self::new().expect("failed to build HiAnime HTTP client")
    }
}

struct EmbedInfo {
    referer: String,
    mal_id: Option<String>,
    master_url: String,
    subtitle: Option<String>,
}

async fn fetch_embed_info(client: &Client, ep_id: &str, mode: &str) -> Result<EmbedInfo> {
    let servers_url = format!("{HIANIME_BASE_URL}/api/theme/episode/servers?episodeId={ep_id}");
    dbg_log!("hianime", "GET {servers_url}");
    let body = client
        .get(&servers_url)
        .header("User-Agent", HIANIME_USER_AGENT)
        .header("Referer", HIANIME_BASE_URL)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let servers_html = extract_html_field(&body);
    let hash = extract_server_hash(&servers_html, mode)
        .ok_or_else(|| anyhow!("no {mode} sources found (ZokoAnime server missing)"))?;
    let embed_url = decode_b64(&hash)?;
    if embed_url.is_empty() {
        bail!("empty embed url for episode {ep_id}");
    }
    let referer = referer_for(&embed_url);
    let mal_id = extract_mal_id(&embed_url);

    dbg_log!("hianime", "GET embed {embed_url}");
    let embed_html = client
        .get(&embed_url)
        .header("User-Agent", HIANIME_USER_AGENT)
        .header("Referer", HIANIME_BASE_URL)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let blob = RE_BLOB
        .captures(&embed_html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .ok_or_else(|| anyhow!("embed config not found"))?;
    let json_str = deobfuscate_blob(blob)?;
    let (master_url, subtitle) = parse_embed_json(&json_str)?;
    Ok(EmbedInfo {
        referer,
        mal_id,
        master_url,
        subtitle,
    })
}

impl AnimeProvider for HianimeClient {
    async fn search_shows(&self, query: &str, _translation: Translation) -> Result<Vec<ShowInfo>> {
        let query_clean = query.trim();
        if query_clean.is_empty() {
            bail!("empty search query");
        }
        let url = format!("{HIANIME_BASE_URL}/search");
        dbg_log!("hianime", "search '{query_clean}'");
        let html = self
            .client
            .get(&url)
            .query(&[("keyword", query_clean)])
            .header("User-Agent", HIANIME_USER_AGENT)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        if html.contains("<title>Just a moment") {
            bail!("blocked by Cloudflare. Try again later");
        }
        Ok(parse_search_results(&html))
    }

    async fn fetch_episodes(
        &self,
        show_id: &str,
        _translation: Translation,
    ) -> Result<Vec<String>> {
        let pairs = self.fetch_episode_pairs(show_id).await?;
        dbg_log!("hianime", "found {} episodes for {show_id}", pairs.len());
        Ok(pairs.into_iter().map(|(num, _)| num).collect())
    }

    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>> {
        let mode = match translation {
            Translation::Dub => "dub",
            Translation::Sub | Translation::Raw => "sub",
        };
        // Fall back to the other track when the requested one has no server,
        // mirroring the ani-cli behaviour of erroring only when both are empty.
        let info = match self.resolve_embed(show_id, episode, mode).await {
            Ok(info) => info,
            Err(err) => {
                let fallback = if mode == "dub" { "sub" } else { "dub" };
                dbg_log!(
                    "hianime",
                    "{mode} embed failed for ep {episode}: {err:#}; trying {fallback}"
                );
                self.resolve_embed(show_id, episode, fallback).await?
            }
        };

        dbg_log!("hianime", "GET master {}", info.master_url);
        let master_text = self
            .client
            .get(&info.master_url)
            .header("User-Agent", HIANIME_USER_AGENT)
            .header("Referer", &info.referer)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        if master_text.trim().is_empty() {
            bail!("empty master playlist for episode {episode}");
        }

        let mut streams = crate::providers::parse_hls_master(
            &info.master_url,
            &master_text,
            &info.referer,
            Provider::Hianime.display_name(),
        );
        for stream in &mut streams {
            stream
                .headers
                .insert("User-Agent".to_string(), USER_AGENT.to_string());
            // Raw means "no subtitles": keep video but drop the VTT track.
            stream.subtitle = if translation == Translation::Raw {
                None
            } else {
                info.subtitle.clone()
            };
        }
        if streams.is_empty() {
            bail!("no streams found for episode {episode}");
        }
        Ok(streams)
    }

    async fn fetch_mal_id(&self, show_id: &str) -> Result<Option<String>> {
        // The MAL id is embedded in the ZokoAnime stream URL
        // (…/stream/mal/<id>/…), so resolve it via the first episode.
        let pairs = match self.fetch_episode_pairs(show_id).await {
            Ok(p) => p,
            Err(err) => {
                dbg_log!(
                    "hianime",
                    "fetch_mal_id: no episodes for {show_id}: {err:#}"
                );
                return Ok(None);
            }
        };
        let Some((first_num, _)) = pairs.first() else {
            return Ok(None);
        };
        for mode in ["sub", "dub"] {
            match self.resolve_embed(show_id, first_num, mode).await {
                Ok(info) => return Ok(info.mal_id),
                Err(err) => {
                    dbg_log!("hianime", "fetch_mal_id {mode} failed: {err:#}");
                }
            }
        }
        Ok(None)
    }
}

/// Numeric suffix after the last `-` (e.g. `naruto-1335` → `1335`).
/// This is the ID the episode-list API expects.
pub fn numeric_id(show_id: &str) -> &str {
    show_id.rsplit('-').next().unwrap_or(show_id)
}

pub fn decode_b64(input: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input.trim())
        .context("failed to base64-decode embed hash")?;
    String::from_utf8(bytes).context("embed hash is not valid UTF-8")
}

/// The embed page ships its config as base64(json XOR "otaku-embed-v1").
pub fn deobfuscate_blob(blob_b64: &str) -> Result<String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(blob_b64.trim())
        .context("failed to base64-decode embed blob")?;
    let out: Vec<u8> = raw
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ DEOBFUSCATION_KEY[i % DEOBFUSCATION_KEY.len()])
        .collect();
    String::from_utf8(out).context("deobfuscated embed config is not valid UTF-8")
}

pub fn extract_mal_id(embed_url: &str) -> Option<String> {
    RE_MAL_ID.captures(embed_url).map(|c| c[1].to_string())
}

pub fn referer_for(embed_url: &str) -> String {
    url::Url::parse(embed_url)
        .map(|u| format!("{}://{}/", u.scheme(), u.host_str().unwrap_or_default()))
        .unwrap_or_else(|_| format!("{HIANIME_BASE_URL}/"))
}

/// Servers/episodes endpoints return `{"html": "..."}`; fall back to the raw
/// body when it is already an HTML fragment.
pub fn extract_html_field(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(html) = v.get("html").and_then(|h| h.as_str()) {
            return html.to_string();
        }
    }
    body.to_string()
}

/// Parse the episode-list payload into `(episode_number, episode_id)` pairs,
/// preserving document order and dropping duplicates.
pub fn parse_episodes_payload(body: &str) -> Vec<(String, String)> {
    let html = extract_html_field(body);
    let mut pairs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for cap in RE_EP_PAIR.captures_iter(&html) {
        let num = cap[1].trim().to_string();
        let id = cap[2].trim().to_string();
        if num.is_empty() || id.is_empty() {
            continue;
        }
        if seen.insert(num.clone()) {
            pairs.push((num, id));
        }
    }
    pairs
}

/// Find the ZokoAnime server hash for the requested `sub`/`dub` track.
pub fn extract_server_hash(servers_html: &str, mode: &str) -> Option<String> {
    // Prefer the DOM: attribute order and whitespace vary between responses.
    if let Ok(sel) = Selector::parse("div.server-item") {
        let doc = Html::parse_fragment(servers_html);
        for el in doc.select(&sel) {
            let v = el.value();
            if v.attr("data-type") == Some(mode) && v.attr("data-server-name") == Some("ZokoAnime")
            {
                if let Some(hash) = v.attr("data-hash") {
                    if !hash.trim().is_empty() {
                        return Some(hash.trim().to_string());
                    }
                }
            }
        }
    }
    // Regex fallback for malformed fragments the parser drops.
    let pattern = format!(r#"data-type="{mode}"[^>]*data-hash="([^"]+)""#);
    Regex::new(&pattern)
        .ok()
        .and_then(|re| re.captures(servers_html))
        .map(|c| c[1].to_string())
}

/// Extract `(master_playlist_url, default_subtitle_url)` from the
/// deobfuscated embed JSON.
pub fn parse_embed_json(json_str: &str) -> Result<(String, Option<String>)> {
    let v: serde_json::Value =
        serde_json::from_str(json_str).context("failed to parse embed config JSON")?;

    let mut master: Option<String> = None;
    if let Some(s) = v.get("src").and_then(|s| s.as_str()) {
        if s.contains(".m3u8") {
            master = Some(s.to_string());
        }
    }
    if master.is_none() {
        if let Some(arr) = v.get("sources").and_then(|s| s.as_array()) {
            for entry in arr {
                if let Some(s) = entry.get("src").and_then(|s| s.as_str()) {
                    if s.contains(".m3u8") {
                        master = Some(s.to_string());
                        break;
                    }
                }
            }
        }
    }
    if master.is_none() {
        if let Some(s) = v.get("sources").and_then(|s| s.as_str()) {
            if s.contains(".m3u8") {
                master = Some(s.to_string());
            }
        }
    }
    // Last resort: first quoted src ending in .m3u8 anywhere in the blob.
    if master.is_none() {
        master = RE_M3U8_SRC.captures(json_str).map(|c| c[1].to_string());
    }
    let master = master.ok_or_else(|| anyhow!("master playlist URL not found in embed config"))?;

    let mut subtitle: Option<String> = None;
    if let Some(subs) = v.get("subtitles").and_then(|s| s.as_array()) {
        let pick = subs
            .iter()
            .find(|s| s.get("default").and_then(|d| d.as_bool()) == Some(true))
            .or_else(|| subs.first());
        if let Some(src) = pick.and_then(|s| s.get("src")).and_then(|s| s.as_str()) {
            if !src.trim().is_empty() {
                subtitle = Some(src.trim().to_string());
            }
        }
    }
    Ok((master, subtitle))
}

fn decode_entities(s: &str) -> String {
    s.replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// Parse search HTML into `ShowInfo` list. Ticks (`tick-sub`/`tick-dub`)
/// provide sub/dub episode counts when present.
pub fn parse_search_results(html: &str) -> Vec<ShowInfo> {
    // The top-10 sidebar repeats result markup; drop it like ani-cli does.
    let usable = match html.find("id=\"main-sidebar\"") {
        Some(idx) => &html[..idx],
        None => html,
    };
    let doc = Html::parse_document(usable);
    let Ok(item_sel) = Selector::parse("div.flw-item") else {
        return Vec::new();
    };
    let Ok(name_sel) = Selector::parse("h3.film-name a") else {
        return Vec::new();
    };
    let sub_sel = Selector::parse("div.tick-item.tick-sub").ok();
    let dub_sel = Selector::parse("div.tick-item.tick-dub").ok();

    let mut shows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in doc.select(&item_sel) {
        let Some(name_el) = item.select(&name_sel).next() else {
            continue;
        };
        let href = name_el.value().attr("href").unwrap_or_default();
        let slug = href
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        let title_attr = name_el.value().attr("title").unwrap_or_default().trim();
        let title_text = name_el.text().collect::<String>();
        let raw_title = if title_attr.is_empty() {
            title_text.trim().to_string()
        } else {
            title_attr.to_string()
        };
        let title = decode_entities(&raw_title);
        if title.is_empty() {
            continue;
        }

        let mut sub = 0usize;
        let mut dub = 0usize;
        if let Some(ref sel) = sub_sel {
            if let Some(el) = item.select(sel).next() {
                let text = el.text().collect::<String>();
                sub = RE_NUM
                    .captures(&text)
                    .and_then(|c| c[1].parse().ok())
                    .unwrap_or(0);
            }
        }
        if let Some(ref sel) = dub_sel {
            if let Some(el) = item.select(sel).next() {
                let text = el.text().collect::<String>();
                dub = RE_NUM
                    .captures(&text)
                    .and_then(|c| c[1].parse().ok())
                    .unwrap_or(0);
            }
        }

        shows.push(ShowInfo {
            id: slug,
            title,
            mal_id: None,
            available_eps: EpisodeCounts { sub, dub },
        });
    }
    shows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_numeric_id() {
        assert_eq!(numeric_id("naruto-1335"), "1335");
        assert_eq!(numeric_id("boruto-naruto-next-generations-650"), "650");
    }

    #[test]
    fn test_decode_b64_hash() {
        let out =
            decode_b64("aHR0cHM6Ly96b2tvYW5pbWUudmlkZW8vc3RyZWFtL21hbC8yMC8xL3N1Yg==").unwrap();
        assert_eq!(out, "https://zokoanime.video/stream/mal/20/1/sub");
    }

    #[test]
    fn test_extract_mal_id() {
        assert_eq!(
            extract_mal_id("https://zokoanime.video/stream/mal/20/1/sub"),
            Some("20".to_string())
        );
        assert_eq!(extract_mal_id("https://example.com/x"), None);
    }

    #[test]
    fn test_referer_for() {
        assert_eq!(
            referer_for("https://zokoanime.video/stream/mal/20/1/sub"),
            "https://zokoanime.video/"
        );
    }

    #[test]
    fn test_deobfuscate_blob() {
        // `{"src":"https://example.com/master.m3u8"}` XORed with the key.
        let key = b"otaku-embed-v1";
        let plain = br#"{"src":"https://example.com/m.m3u8","subtitles":[{"default":true,"src":"https://example.com/s.vtt"}]}"#;
        let enc: Vec<u8> = plain
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ key[i % key.len()])
            .collect();
        let blob = base64::engine::general_purpose::STANDARD.encode(&enc);
        let out = deobfuscate_blob(&blob).unwrap();
        assert_eq!(out, String::from_utf8_lossy(plain));
        let (master, sub) = parse_embed_json(&out).unwrap();
        assert_eq!(master, "https://example.com/m.m3u8");
        assert_eq!(sub, Some("https://example.com/s.vtt".to_string()));
    }

    #[test]
    fn test_parse_embed_json_sources_array() {
        let json = r#"{"sources":[{"src":"https://cdn.example/master.m3u8"}],"subtitles":[]}"#;
        let (master, sub) = parse_embed_json(json).unwrap();
        assert_eq!(master, "https://cdn.example/master.m3u8");
        assert_eq!(sub, None);
    }

    #[test]
    fn test_parse_episodes_payload() {
        let body = r#"{"status":true,"html":"<a class=\"ssl-item ep-item\" data-number=\"1\" data-id=\"22676\" href=\"https://hianime.at/watch/naruto-1335?ep=22676\">x</a><a class=\"ssl-item ep-item\" data-number=\"2\" data-id=\"22677\" href=\"https://hianime.at/watch/naruto-1335?ep=22677\">x</a>"}"#;
        let pairs = parse_episodes_payload(body);
        assert_eq!(
            pairs,
            vec![
                ("1".to_string(), "22676".to_string()),
                ("2".to_string(), "22677".to_string())
            ]
        );
    }

    #[test]
    fn test_extract_server_hash() {
        let html = r#"<div class="item server-item" data-type="sub" data-server-name="ZokoAnime" data-hash="abc123"><a>ZokoAnime</a></div><div class="item server-item" data-type="dub" data-server-name="ZokoAnime" data-hash="def456"><a>ZokoAnime</a></div>"#;
        assert_eq!(extract_server_hash(html, "sub"), Some("abc123".to_string()));
        assert_eq!(extract_server_hash(html, "dub"), Some("def456".to_string()));
        assert_eq!(extract_server_hash(html, "raw"), None);
    }

    #[test]
    fn test_parse_search_results() {
        let html = r#"
        <div class="flw-item">
            <div class="film-poster"><div class="tick"><div class="tick-item tick-sub">220</div><div class="tick-item tick-dub">220</div></div></div>
            <div class="film-detail"><h3 class="film-name"><a href="https://hianime.at/naruto-1335" title="Naruto">Naruto</a></h3></div>
        </div>
        <div class="flw-item">
            <div class="film-poster"></div>
            <div class="film-detail"><h3 class="film-name"><a href="https://hianime.at/one-piece-100" title="One Piece">One Piece</a></h3></div>
        </div>
        <div id="main-sidebar"><div class="flw-item"><div class="film-detail"><h3 class="film-name"><a href="https://hianime.at/sidebar-1" title="Sidebar">Sidebar</a></h3></div></div></div>
        "#;
        let shows = parse_search_results(html);
        assert_eq!(shows.len(), 2);
        assert_eq!(shows[0].id, "naruto-1335");
        assert_eq!(shows[0].title, "Naruto");
        assert_eq!(shows[0].available_eps.sub, 220);
        assert_eq!(shows[0].available_eps.dub, 220);
        assert_eq!(shows[1].id, "one-piece-100");
    }

    #[test]
    fn test_decode_entities() {
        assert_eq!(decode_entities("Naruto&#039;s"), "Naruto's");
    }
}
