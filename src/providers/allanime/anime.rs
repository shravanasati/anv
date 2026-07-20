use std::collections::HashMap;
use anyhow::{bail, Result};
use futures::{stream::FuturesUnordered, StreamExt};

use super::{
    crypto::{decode_provider_path, decrypt_filemoon, EPISODE_SOURCES_HASH},
    models::*,
    queries::*,
    streams::*,
    AllAnimeClient,
};
use crate::providers::AnimeProvider;
use crate::types::{EpisodeCounts, ShowInfo, StreamOption, Translation};

impl AllAnimeClient {
    pub(super) async fn fetch_show_detail(&self, show_id: &str) -> Result<ShowDetail> {
        let variables = serde_json::json!({ "showId": show_id });
        let payload: ShowDetailPayload = self
            .execute_graphql(false, variables, Some(SHOW_DETAIL_QUERY), None)
            .await?;
        Ok(payload.show)
    }

    pub(super) async fn fetch_episode_sources_internal(
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
        Ok(payload
            .episode
            .map(|e| e.source_urls)
            .unwrap_or_default())
    }

    pub(super) async fn fetch_single_provider_streams(
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
            let decrypted = decrypted
                .replace("\\u0026", "&")
                .replace("\\u003D", "=")
                .replace("\\u002F", "/")
                .replace("\\/", "/");

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
