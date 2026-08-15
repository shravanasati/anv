use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex, OnceLock},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use url::Url;

use crate::providers::{AUTO_QUALITY_LABEL, AUTO_QUALITY_RANK, AnimeProvider, parse_quality_rank};
use crate::types::{EpisodeCounts, Provider, ShowInfo, StreamOption, Translation};

const BASE_URL: &str = "https://anineko.to";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const PNG_IEND_MARKER: &[u8] = &[0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82];

static RE_EP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"/ep-(\d+)"#).unwrap());
static RE_WATCH_SLUG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"/watch/([^/?#]+)"#).unwrap());
static RE_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-id="(hsub|sub|dub)""#).unwrap());
static RE_VIDEO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"data-video="([^"]+)""#).unwrap());
static RE_SUB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"const subtitle = "([^"]+)""#).unwrap());
static RE_TRACK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"file:\s*"([^"]+\.(?:vtt|ass|srt))""#).unwrap());
static RE_MASTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"const src = "(https?://[^"]+/master\.m3u8)""#).unwrap());
static RE_VARIANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^#EXT-X-STREAM-INF:.*NAME="([^"]+)".*\r?\n([^\r\n#]+)"#).unwrap()
});
static RE_PUBLIC_MASTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"const src = "(https?://[^"]+/public/stream/[^"]+/master\.m3u8)""#).unwrap()
});

#[derive(Debug, Clone)]
pub struct AninekoClient {
    client: Client,
}

impl AninekoClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent(USER_AGENT)
            .build()?;

        Ok(Self { client })
    }

    async fn fetch_string(&self, url: &str, referer: &str) -> Result<String> {
        let mut req = self.client.get(url).header("User-Agent", USER_AGENT);
        if !referer.is_empty() {
            req = req.header("Referer", referer);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            bail!(
                "AniNeko request to {} failed with status {}",
                url,
                resp.status()
            );
        }
        Ok(resp.text().await?)
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    success: bool,
    #[serde(default)]
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    title: String,
    url: String,
    #[allow(dead_code)]
    image: String,
    #[serde(default)]
    meta: String,
}

impl AnimeProvider for AninekoClient {
    async fn search_shows(&self, query: &str, _translation: Translation) -> Result<Vec<ShowInfo>> {
        let query = query.trim();
        if query.is_empty() {
            bail!("empty search query");
        }

        let encoded_query: String =
            url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let raw_url = format!("{}/ajax/search?q={}", BASE_URL, encoded_query);
        let referer = format!("{}/", BASE_URL);
        let body = self.fetch_string(&raw_url, &referer).await?;

        let payload: SearchResponse = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse AniNeko search response for '{query}'"))?;

        if !payload.success {
            bail!("AniNeko search failed for query '{query}'");
        }

        let mut shows = Vec::new();
        for res in payload.results {
            let slug = match slug_from_watch_url(&res.url) {
                Some(s) if !s.is_empty() => s,
                _ => continue,
            };

            let title = if res.meta.trim().is_empty() {
                res.title.trim().to_string()
            } else {
                format!("{} — {}", res.title.trim(), res.meta.trim())
            };

            shows.push(ShowInfo {
                id: slug,
                title,
                mal_id: None,
                available_eps: EpisodeCounts::default(),
            });
        }

        if shows.is_empty() {
            bail!("No results for \"{}\"", query);
        }

        Ok(shows)
    }

    async fn fetch_episodes(
        &self,
        show_id: &str,
        _translation: Translation,
    ) -> Result<Vec<String>> {
        let slug = show_id.trim();
        if slug.is_empty() {
            bail!("empty show id");
        }

        let watch_url = format!("{}/watch/{}", BASE_URL, slug);
        let referer = format!("{}/", BASE_URL);
        let html = self.fetch_string(&watch_url, &referer).await?;

        let mut seen = std::collections::BTreeSet::new();

        for cap in RE_EP.captures_iter(&html) {
            if let Ok(ep_num) = cap[1].parse::<usize>() {
                if ep_num > 0 {
                    seen.insert(ep_num);
                }
            }
        }

        if seen.is_empty() {
            bail!("no episodes found for \"{}\"", slug);
        }

        Ok(seen.into_iter().map(|n| n.to_string()).collect())
    }

    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>> {
        let slug = show_id.trim();
        if slug.is_empty() {
            bail!("empty show id");
        }
        let ep_num: usize = episode
            .parse()
            .with_context(|| format!("invalid episode number '{episode}'"))?;

        let watch_url = format!("{}/watch/{}/ep-{}", BASE_URL, slug, ep_num);
        let referer = format!("{}/", BASE_URL);
        let html = self.fetch_string(&watch_url, &referer).await?;

        let groups = extract_lang_embed_urls(&html);

        let embed_urls = match translation {
            Translation::Dub => groups.get("dub").cloned().unwrap_or_default(),
            Translation::Raw => {
                let soft = groups.get("sub").cloned().unwrap_or_default();
                if !soft.is_empty() {
                    soft
                } else {
                    groups.get("hsub").cloned().unwrap_or_default()
                }
            }
            Translation::Sub => {
                let hard = groups.get("hsub").cloned().unwrap_or_default();
                if !hard.is_empty() {
                    hard
                } else {
                    groups.get("sub").cloned().unwrap_or_default()
                }
            }
        };

        if embed_urls.is_empty() {
            bail!(
                "no {} streams found for episode {}",
                translation.label(),
                episode
            );
        }

        for embed_url in embed_urls {
            let host_type = resolve_embed_host(&embed_url);
            match host_type.as_str() {
                "bibiemb" => {
                    if let Ok(mut streams) = resolve_bibiemb(&self.client, &embed_url).await {
                        if !streams.is_empty() {
                            if translation == Translation::Raw {
                                for s in &mut streams {
                                    s.subtitle = None;
                                }
                            }
                            return Ok(streams);
                        }
                    }
                }
                "vibeplayer" => {
                    if let Ok(mut streams) = resolve_vibeplayer(&self.client, &embed_url).await {
                        if !streams.is_empty() {
                            if translation == Translation::Raw {
                                for s in &mut streams {
                                    s.subtitle = None;
                                }
                            }
                            return Ok(streams);
                        }
                    }
                }
                _ => continue,
            }
        }

        bail!("no playable streams resolved for episode {}", episode)
    }
}

pub fn slug_from_watch_url(watch_path: &str) -> Option<String> {
    RE_WATCH_SLUG
        .captures(watch_path)
        .map(|c| c[1].trim().to_string())
}

pub fn extract_lang_embed_urls(html: &str) -> HashMap<String, Vec<String>> {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    groups.insert("hsub".to_string(), Vec::new());
    groups.insert("sub".to_string(), Vec::new());
    groups.insert("dub".to_string(), Vec::new());

    let matches: Vec<_> = RE_MARKER.find_iter(html).collect();
    for (i, m) in matches.iter().enumerate() {
        let lang_match = RE_MARKER.captures(m.as_str()).unwrap();
        let lang = lang_match[1].to_string();
        let start = m.end();
        let end = if i + 1 < matches.len() {
            matches[i + 1].start()
        } else {
            html.len()
        };
        let block = &html[start..end];

        let mut seen = std::collections::HashSet::new();
        for vcap in RE_VIDEO.captures_iter(block) {
            let embed_url = vcap[1].trim().to_string();
            if !embed_url.is_empty() && seen.insert(embed_url.clone()) {
                groups.entry(lang.clone()).or_default().push(embed_url);
            }
        }
    }

    groups
}

pub fn resolve_embed_host(embed_url: &str) -> String {
    let host = match Url::parse(embed_url) {
        Ok(u) => u.host_str().unwrap_or("").to_lowercase(),
        Err(_) => return String::new(),
    };

    if host.contains("bibiemb.") {
        "bibiemb".to_string()
    } else if is_vibeplayer_host(&host) {
        "vibeplayer".to_string()
    } else {
        String::new()
    }
}

fn is_vibeplayer_host(host: &str) -> bool {
    let host = host.trim().to_lowercase();
    if host.is_empty() {
        return false;
    }
    if host.contains("vibeplayer.") || host.contains("vivibebe.") {
        return true;
    }
    host.starts_with("vibe") && host.contains("be.")
}

pub fn subtitle_from_embed_url(embed_url: &str) -> Option<String> {
    let parsed = Url::parse(embed_url).ok()?;
    for (key, val) in parsed.query_pairs() {
        let k = key.to_lowercase();
        if (k == "sub" || k == "caption_1" || k == "c1_file") && !val.trim().is_empty() {
            return Some(val.trim().to_string());
        }
    }
    None
}

pub fn subtitle_from_embed_html(html: &str) -> Option<String> {
    if let Some(cap) = RE_SUB.captures(html) {
        let s = cap[1].trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(cap) = RE_TRACK.captures(html) {
        let s = cap[1].trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    None
}

pub fn resolve_subtitle(embed_url: &str, html: &str) -> Option<String> {
    if let Some(sub) = subtitle_from_embed_url(embed_url) {
        return Some(sub);
    }
    subtitle_from_embed_html(html)
}

async fn resolve_bibiemb(client: &Client, embed_url: &str) -> Result<Vec<StreamOption>> {
    let html = client
        .get(embed_url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", format!("{}/", BASE_URL))
        .send()
        .await?
        .text()
        .await?;

    let cap = RE_MASTER
        .captures(&html)
        .ok_or_else(|| anyhow!("bibiemb master m3u8 not found"))?;
    let master_url = cap[1].to_string();

    let subtitle = resolve_subtitle(embed_url, &html);

    let playlist = client
        .get(&master_url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", embed_url)
        .send()
        .await?
        .text()
        .await?;

    let mut streams = Vec::new();

    let base = Url::parse(&master_url)?;

    for vcap in RE_VARIANT.captures_iter(&playlist) {
        let name = vcap[1].trim().to_string();
        let rel_url = vcap[2].trim();
        let abs_url = base.join(rel_url)?.to_string();

        let rank = parse_quality_rank(&name);

        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), embed_url.to_string());

        streams.push(StreamOption {
            provider: Provider::Anineko.display_name().to_string(),
            url: abs_url,
            quality_label: name,
            quality_rank: rank,
            is_hls: true,
            headers,
            subtitle: subtitle.clone(),
        });
    }

    if streams.is_empty() {
        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), embed_url.to_string());

        streams.push(StreamOption {
            provider: Provider::Anineko.display_name().to_string(),
            url: master_url,
            quality_label: AUTO_QUALITY_LABEL.to_string(),
            quality_rank: AUTO_QUALITY_RANK,
            is_hls: true,
            headers,
            subtitle,
        });
    }

    streams.sort_by_key(|b| std::cmp::Reverse(b.quality_rank));
    Ok(streams)
}

async fn resolve_vibeplayer(client: &Client, embed_url: &str) -> Result<Vec<StreamOption>> {
    let html = client
        .get(embed_url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", format!("{}/", BASE_URL))
        .send()
        .await?
        .text()
        .await?;

    let cap = RE_PUBLIC_MASTER
        .captures(&html)
        .ok_or_else(|| anyhow!("vibeplayer master m3u8 not found"))?;
    let master_url = cap[1].to_string();

    let subtitle = resolve_subtitle(embed_url, &html);

    let proxy = get_vibe_proxy().await?;
    let proxy_url = proxy.register(master_url, embed_url.to_string()).await;

    let mut headers = HashMap::new();
    headers.insert("Referer".to_string(), embed_url.to_string());

    Ok(vec![StreamOption {
        provider: Provider::Anineko.display_name().to_string(),
        url: proxy_url,
        quality_label: AUTO_QUALITY_LABEL.to_string(),
        quality_rank: AUTO_QUALITY_RANK,
        is_hls: true,
        headers,
        subtitle,
    }])
}

// ---------------------------------------------------------------------------
// VibeProxy: Local HLS Proxy to strip PNG headers from video segments
// ---------------------------------------------------------------------------

struct VibeSession {
    master_url: String,
    referer: String,
    created_at: std::time::Instant,
    variants: Mutex<HashMap<String, Vec<String>>>,
}

pub struct VibeProxy {
    base_url: String,
    sessions: Mutex<HashMap<String, Arc<VibeSession>>>,
    client: Client,
}

static VIBE_PROXY: OnceLock<Arc<VibeProxy>> = OnceLock::new();

async fn get_vibe_proxy() -> Result<Arc<VibeProxy>> {
    if let Some(proxy) = VIBE_PROXY.get() {
        return Ok(Arc::clone(proxy));
    }

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind VibeProxy listener")?;
    let local_addr = listener.local_addr()?;
    let base_url = format!("http://127.0.0.1:{}", local_addr.port());

    let proxy = Arc::new(VibeProxy {
        base_url,
        sessions: Mutex::new(HashMap::new()),
        client: Client::builder().user_agent(USER_AGENT).build()?,
    });

    let proxy_ref = Arc::clone(&proxy);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let p = Arc::clone(&proxy_ref);
                    tokio::spawn(async move {
                        let _ = p.handle_connection(stream).await;
                    });
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
    });

    let _ = VIBE_PROXY.set(Arc::clone(&proxy));
    Ok(proxy)
}

impl VibeProxy {
    pub async fn register(&self, master_url: String, referer: String) -> String {
        let session_id = format!("{:016x}", rand::random::<u64>());
        let session = Arc::new(VibeSession {
            master_url,
            referer,
            created_at: std::time::Instant::now(),
            variants: Mutex::new(HashMap::new()),
        });
        let ttl = std::time::Duration::from_secs(7200); // 2 hours
        {
            let mut lock = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            lock.retain(|_, s| s.created_at.elapsed() < ttl);
            lock.insert(session_id.clone(), session);
        }
        format!("{}/stream/{}/master.m3u8", self.base_url, session_id)
    }

    async fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
        let mut reader = BufReader::new(&mut stream);
        let mut req_line = String::new();
        if reader.read_line(&mut req_line).await? == 0 {
            return Ok(());
        }

        let parts: Vec<&str> = req_line.split_whitespace().collect();
        if parts.len() < 2 {
            return Ok(());
        }

        let path = parts[1].trim_start_matches("/stream/");
        let path_parts: Vec<&str> = path.split('/').collect();

        if path_parts.len() < 2 {
            return write_http_response(&mut stream, 404, "text/plain", b"Not Found").await;
        }

        let session_id = path_parts[0];
        let session = {
            let guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            guard.get(session_id).cloned()
        };
        let Some(session) = session else {
            return write_http_response(&mut stream, 404, "text/plain", b"Session Not Found").await;
        };

        if path_parts.len() == 2 && path_parts[1] == "master.m3u8" {
            self.serve_master(&mut stream, session_id, &session).await
        } else if path_parts.len() == 2 && path_parts[1].ends_with(".m3u8") {
            self.serve_variant(&mut stream, session_id, &session, path_parts[1])
                .await
        } else if path_parts.len() == 4 && path_parts[2] == "seg" {
            self.serve_segment(&mut stream, &session, path_parts[1], path_parts[3])
                .await
        } else {
            write_http_response(&mut stream, 404, "text/plain", b"Not Found").await
        }
    }

    async fn serve_master(
        &self,
        stream: &mut TcpStream,
        session_id: &str,
        session: &VibeSession,
    ) -> Result<()> {
        let body = match self
            .client
            .get(&session.master_url)
            .header("Referer", &session.referer)
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => res.text().await?,
            _ => {
                return write_http_response(stream, 502, "text/plain", b"Master Fetch Error").await;
            }
        };

        let mut rewritten = String::new();
        for line in body.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                rewritten.push_str(line);
            } else {
                rewritten.push_str(&format!("{}/stream/{}/{}", self.base_url, session_id, l));
            }
            rewritten.push('\n');
        }

        write_http_response(
            stream,
            200,
            "application/vnd.apple.mpegurl",
            rewritten.as_bytes(),
        )
        .await
    }

    async fn serve_variant(
        &self,
        stream: &mut TcpStream,
        session_id: &str,
        session: &VibeSession,
        variant_name: &str,
    ) -> Result<()> {
        let base = Url::parse(&session.master_url)?;
        let variant_url = base.join(variant_name)?.to_string();

        let body = match self
            .client
            .get(&variant_url)
            .header("Referer", &session.referer)
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => res.text().await?,
            _ => {
                return write_http_response(stream, 502, "text/plain", b"Variant Fetch Error")
                    .await;
            }
        };

        let mut segments = Vec::new();
        let mut rewritten = String::new();
        let var_base = Url::parse(&variant_url)?;

        for line in body.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                rewritten.push_str(line);
            } else {
                let seg_url = var_base.join(l)?.to_string();
                let idx = segments.len();
                segments.push(seg_url);
                rewritten.push_str(&format!(
                    "{}/stream/{}/{}/seg/{}",
                    self.base_url, session_id, variant_name, idx
                ));
            }
            rewritten.push('\n');
        }

        session
            .variants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(variant_name.to_string(), segments);
        write_http_response(
            stream,
            200,
            "application/vnd.apple.mpegurl",
            rewritten.as_bytes(),
        )
        .await
    }

    async fn serve_segment(
        &self,
        stream: &mut TcpStream,
        session: &VibeSession,
        variant_name: &str,
        idx_str: &str,
    ) -> Result<()> {
        let idx: usize = match idx_str.parse() {
            Ok(i) => i,
            Err(_) => return write_http_response(stream, 400, "text/plain", b"Bad Index").await,
        };

        let seg_url = {
            let guard = session
                .variants
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard
                .get(variant_name)
                .and_then(|segs| segs.get(idx))
                .cloned()
        };
        let Some(seg_url) = seg_url else {
            return write_http_response(stream, 404, "text/plain", b"Segment Not Found").await;
        };

        let bytes = match self
            .client
            .get(&seg_url)
            .header("Referer", &session.referer)
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => res.bytes().await?,
            _ => {
                return write_http_response(stream, 502, "text/plain", b"Segment Fetch Error")
                    .await;
            }
        };

        let stripped = strip_png_wrapper(&bytes);
        write_http_response(stream, 200, "video/mp2t", stripped).await
    }
}

pub fn strip_png_wrapper(data: &[u8]) -> &[u8] {
    if let Some(pos) = data
        .windows(PNG_IEND_MARKER.len())
        .position(|w| w == PNG_IEND_MARKER)
    {
        &data[pos + PNG_IEND_MARKER.len()..]
    } else {
        data
    }
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("Unknown");
    let response_headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        content_type
    );
    stream.write_all(response_headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug_from_watch_url() {
        assert_eq!(
            slug_from_watch_url("/watch/one-piece"),
            Some("one-piece".to_string())
        );
        assert_eq!(
            slug_from_watch_url("https://anineko.to/watch/frieren-2nd-season/ep-1"),
            Some("frieren-2nd-season".to_string())
        );
    }

    #[test]
    fn test_extract_lang_embed_urls() {
        let html = r#"
        <div class="server-items" data-id="sub">
          <div data-video="https://bibiemb.xyz/aaa"></div>
          <div data-video="https://vibeplayer.site/bbb?sub=https://cdn.example/sub.vtt"></div>
        </div>
        <div class="server-items" data-id="dub">
          <div data-video="https://bibiemb.xyz/ccc"></div>
        </div>"#;

        let groups = extract_lang_embed_urls(html);
        assert_eq!(groups.get("sub").unwrap().len(), 2);
        assert_eq!(groups.get("dub").unwrap().len(), 1);
        assert_eq!(
            subtitle_from_embed_url(&groups.get("sub").unwrap()[1]),
            Some("https://cdn.example/sub.vtt".to_string())
        );
    }

    #[test]
    fn test_resolve_embed_host() {
        assert_eq!(resolve_embed_host("https://bibiemb.xyz/e123"), "bibiemb");
        assert_eq!(
            resolve_embed_host("https://vivibebe.site/e6693c8de8202fbe"),
            "vibeplayer"
        );
        assert_eq!(
            resolve_embed_host("https://vibeplayer.site/watch"),
            "vibeplayer"
        );
    }

    #[test]
    fn test_strip_png_wrapper() {
        let mut data = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // PNG magic
        data.extend_from_slice(PNG_IEND_MARKER);
        let payload = vec![0x47, 0x40, 0x00, 0x10]; // MPEG-TS start
        data.extend_from_slice(&payload);

        let result = strip_png_wrapper(&data);
        assert_eq!(result, payload.as_slice());
    }
}
