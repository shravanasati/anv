use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use url::Url;

use crate::dbg_log;
use crate::providers::{AnimeProvider, USER_AGENT};
use crate::types::{EpisodeCounts, ShowInfo, StreamOption, Translation};

pub const ANIDB_BASE_URL: &str = "https://anidb.app";

fn decode_html_entities(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[derive(Debug, Clone)]
pub struct AnidbClient {
    client: Client,
}

#[derive(Deserialize)]
struct AnidbEpisodeItem {
    id: usize,
    number: usize,
}

/// Wrapper for the episodes API response: {"episodes": [...]}
#[derive(Deserialize)]
struct AnidbEpisodesResponse {
    episodes: Vec<AnidbEpisodeItem>,
}

#[derive(Deserialize)]
struct AnidbLanguageItem {
    code: Option<String>,
    embed_url: Option<String>,
}

/// Wrapper for the languages API response: {"languages": [...]}
#[derive(Deserialize)]
struct AnidbLanguagesResponse {
    languages: Vec<AnidbLanguageItem>,
}

impl AnidbClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()?;

        Ok(Self { client })
    }

    fn extract_numeric_id(show_id: &str) -> Result<&str> {
        show_id
            .rsplit('-')
            .next()
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .ok_or_else(|| anyhow!("invalid show_id format for AniDB: {show_id}"))
    }
}

impl AnimeProvider for AnidbClient {
    async fn search_shows(&self, query: &str, _translation: Translation) -> Result<Vec<ShowInfo>> {
        // Primary: use the search suggestions endpoint (stable HTML structure with alt attribute)
        let suggestions_url = format!("{ANIDB_BASE_URL}/search/suggestions");
        dbg_log!("anidb", "search_shows: GET {suggestions_url}?q={query}");
        let suggestions_response = self
            .client
            .get(&suggestions_url)
            .query(&[("q", query)])
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        dbg_log!(
            "anidb",
            "search_shows: suggestions response ({} bytes)",
            suggestions_response.len()
        );

        // Matches <a href="https://anidb.app/anime/SLUG"> ... alt="TITLE" across newlines
        let re_suggestions = regex::Regex::new(
            r#"href="https://anidb\.app/anime/([a-z0-9-]+-[0-9]+)"[\s\S]*?alt="([^"]+)""#,
        )
        .unwrap();

        let mut shows = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        for cap in re_suggestions.captures_iter(&suggestions_response) {
            let id = cap[1].to_string();
            let title = decode_html_entities(&cap[2]);
            if seen_ids.insert(id.clone()) {
                shows.push(ShowInfo {
                    id,
                    title,
                    mal_id: None,
                    available_eps: EpisodeCounts::default(),
                });
            }
        }

        dbg_log!(
            "anidb",
            "search_shows: found {} results from suggestions endpoint",
            shows.len()
        );

        // Fallback: scrape the /browse page with updated patterns
        // (anidb.app now uses full absolute URLs + title= attribute instead of relative + alt=)
        if shows.is_empty() {
            let browse_url = format!("{ANIDB_BASE_URL}/browse");
            dbg_log!(
                "anidb",
                "search_shows: suggestions empty, falling back to browse: GET {browse_url}?q={query}"
            );
            let browse_response = self
                .client
                .get(&browse_url)
                .query(&[("q", query)])
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            dbg_log!(
                "anidb",
                "search_shows: browse response ({} bytes)",
                browse_response.len()
            );

            // Full URL + title attribute (current browse page format)
            let re_browse_title = regex::Regex::new(
                r#"href="https://anidb\.app/anime/([a-z0-9-]+-[0-9]+)"[^>]*title="([^"]+)""#,
            )
            .unwrap();
            // Legacy: relative URL + alt attribute (old format, kept for resilience)
            let re_browse_alt =
                regex::Regex::new(r#"href="/anime/([a-z0-9-]+-[0-9]+)"[^>]*alt="([^"]+)""#)
                    .unwrap();

            for cap in re_browse_title.captures_iter(&browse_response) {
                let id = cap[1].to_string();
                let title = decode_html_entities(&cap[2]);
                if seen_ids.insert(id.clone()) {
                    shows.push(ShowInfo {
                        id,
                        title,
                        mal_id: None,
                        available_eps: EpisodeCounts::default(),
                    });
                }
            }

            if shows.is_empty() {
                for cap in re_browse_alt.captures_iter(&browse_response) {
                    let id = cap[1].to_string();
                    let title = decode_html_entities(&cap[2]);
                    if seen_ids.insert(id.clone()) {
                        shows.push(ShowInfo {
                            id,
                            title,
                            mal_id: None,
                            available_eps: EpisodeCounts::default(),
                        });
                    }
                }
            }
        }

        Ok(shows)
    }

    async fn fetch_episodes(
        &self,
        show_id: &str,
        _translation: Translation,
    ) -> Result<Vec<String>> {
        let num_id = Self::extract_numeric_id(show_id)?;
        let url = format!("{ANIDB_BASE_URL}/api/frontend/anime/{num_id}/episodes");
        dbg_log!("anidb", "fetch_episodes: GET {url}");

        let resp: AnidbEpisodesResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let eps = resp.episodes;
        dbg_log!("anidb", "fetch_episodes: got {} episode items", eps.len());

        let mut ep_numbers: Vec<usize> = eps.into_iter().map(|e| e.number).collect();
        ep_numbers.sort_unstable();
        ep_numbers.dedup();

        Ok(ep_numbers.into_iter().map(|n| n.to_string()).collect())
    }

    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>> {
        let num_id = Self::extract_numeric_id(show_id)?;
        let ep_list_url = format!("{ANIDB_BASE_URL}/api/frontend/anime/{num_id}/episodes");
        dbg_log!("anidb", "fetch_streams: GET episodes {ep_list_url}");

        let resp: AnidbEpisodesResponse = self
            .client
            .get(&ep_list_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let eps = resp.episodes;
        dbg_log!(
            "anidb",
            "fetch_streams: {} episode items, looking for ep={episode}",
            eps.len()
        );

        let ep_num = episode
            .parse::<usize>()
            .map_err(|_| anyhow!("invalid episode number: {episode}"))?;

        let target_ep = eps
            .into_iter()
            .find(|e| e.number == ep_num)
            .ok_or_else(|| anyhow!("episode {episode} not found for show {show_id}"))?;
        dbg_log!(
            "anidb",
            "fetch_streams: matched ep id={} number={}",
            target_ep.id,
            target_ep.number
        );

        let lang_url = format!(
            "{ANIDB_BASE_URL}/api/frontend/episode/{}/languages",
            target_ep.id
        );
        dbg_log!("anidb", "fetch_streams: GET languages {lang_url}");
        let lang_resp: AnidbLanguagesResponse = self
            .client
            .get(&lang_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let langs = lang_resp.languages;
        dbg_log!("anidb", "fetch_streams: {} language items available", langs.len());
        dbg_log!(
            "anidb",
            "fetch_streams: language codes = {:?}",
            langs
                .iter()
                .map(|l| l.code.as_deref().unwrap_or("?"))
                .collect::<Vec<_>>()
        );

        let target_lang = match translation {
            Translation::Dub => "eng",
            _ => "jpn",
        };
        dbg_log!("anidb", "fetch_streams: target lang={target_lang}");

        let embed_url = langs
            .iter()
            .find(|l| l.code.as_deref() == Some(target_lang))
            .or_else(|| langs.first())
            .and_then(|l| l.embed_url.as_deref())
            .ok_or_else(|| anyhow!("no embed URL found for episode {episode} ({target_lang})"))?;
        dbg_log!("anidb", "fetch_streams: embed_url={embed_url}");

        let embed_page = self
            .client
            .get(embed_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        dbg_log!("anidb", "fetch_streams: embed page ({} bytes)", embed_page.len());

        let re_file = regex::Regex::new(r#"file:\s*['"]([^'"]+\.m3u8[^'"]*)['"]"#).unwrap();
        let re_file_generic = regex::Regex::new(r#"(https?://[^\s'"]+\.m3u8[^\s'"]*)"#).unwrap();

        let master_m3u8 = re_file
            .captures(&embed_page)
            .map(|c| c[1].to_string())
            .or_else(|| {
                re_file_generic
                    .captures(&embed_page)
                    .map(|c| c[1].to_string())
            })
            .ok_or_else(|| {
                dbg_log!(
                    "anidb",
                    "fetch_streams: failed to find m3u8 in embed page. embed page snippet:\n{}",
                    &embed_page[..embed_page.len().min(2000)]
                );
                anyhow!("failed to extract m3u8 stream URL from embed page")
            })?;
        dbg_log!("anidb", "fetch_streams: master m3u8={master_m3u8}");

        let m3u8_content = self
            .client
            .get(&master_m3u8)
            .send()
            .await
            .map_err(|e| anyhow!("failed to fetch master m3u8: {e}"))?
            .text()
            .await
            .unwrap_or_default();
        dbg_log!("anidb", "fetch_streams: m3u8 content ({} bytes)", m3u8_content.len());

        let base_url = Url::parse(&master_m3u8).ok();
        let mut streams = Vec::new();
        let lines: Vec<&str> = m3u8_content.lines().collect();

        let re_res = regex::Regex::new(r#"RESOLUTION=\d+x(\d+)"#).unwrap();

        for i in 0..lines.len() {
            let line = lines[i].trim();
            if line.starts_with("#EXT-X-STREAM-INF:") {
                let height = re_res
                    .captures(line)
                    .and_then(|c| c[1].parse::<i32>().ok())
                    .unwrap_or(0);

                if let Some(next_line) = lines.get(i + 1) {
                    let stream_rel = next_line.trim();
                    if !stream_rel.is_empty() && !stream_rel.starts_with('#') {
                        let full_url = match &base_url {
                            Some(b) => b
                                .join(stream_rel)
                                .map(|u| u.to_string())
                                .unwrap_or_else(|_| stream_rel.to_string()),
                            None => stream_rel.to_string(),
                        };

                        let quality_label = if height > 0 {
                            format!("{height}p")
                        } else {
                            "auto".to_string()
                        };

                        let mut headers = HashMap::new();
                        headers.insert("Referer".to_string(), ANIDB_BASE_URL.to_string());

                        streams.push(StreamOption {
                            provider: "AniDB".to_string(),
                            url: full_url,
                            quality_label,
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
            dbg_log!("anidb", "fetch_streams: no quality variants parsed, using master m3u8 directly");
            let mut headers = HashMap::new();
            headers.insert("Referer".to_string(), ANIDB_BASE_URL.to_string());

            streams.push(StreamOption {
                provider: "AniDB".to_string(),
                url: master_m3u8,
                quality_label: "auto".to_string(),
                quality_rank: 0,
                is_hls: true,
                headers,
                subtitle: None,
            });
        } else {
            streams.sort_by(|a, b| b.quality_rank.cmp(&a.quality_rank));
        }
        dbg_log!("anidb", "fetch_streams: returning {} stream options", streams.len());

        Ok(streams)
    }

    async fn fetch_mal_id(&self, show_id: &str) -> Result<Option<String>> {
        let detail_url = format!("{ANIDB_BASE_URL}/anime/{show_id}");
        let text = self
            .client
            .get(&detail_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        let re_mal = regex::Regex::new(r#"myanimelist\.net/anime/([0-9]+)"#).unwrap();
        Ok(re_mal.captures(&text).map(|cap| cap[1].to_string()))
    }
}
