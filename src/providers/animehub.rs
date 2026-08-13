use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use url::Url;

use crate::providers::{AnimeProvider, USER_AGENT};
use crate::types::{EpisodeCounts, ShowInfo, StreamOption, Translation};

pub const ANIMEHUB_BASE_URL: &str = "https://123animehub.cc";

macro_rules! dbg_log {
    ($($arg:tt)*) => {
        if std::env::var("ANV_DEBUG").is_ok() {
            eprintln!("[animehub] {}", format!($($arg)*));
        }
    };
}

#[derive(Debug, Clone)]
pub struct AnimehubClient {
    client: Client,
}

impl AnimehubClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()?;

        Ok(Self { client })
    }

    async fn fetch_string_with_retry(
        &self,
        req_builder: reqwest::RequestBuilder,
    ) -> Result<String> {
        let mut last_err = None;
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let req = req_builder
                .try_clone()
                .ok_or_else(|| anyhow!("cannot clone request builder"))?;
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_server_error() {
                        dbg_log!("fetch attempt {attempt} failed with status {status}");
                        last_err = Some(anyhow!("server error status {status}"));
                        continue;
                    }
                    if !status.is_success() {
                        bail!("request failed with status {status}");
                    }
                    return resp.text().await.context("failed to read response text");
                }
                Err(e) => {
                    dbg_log!("fetch attempt {attempt} error: {e}");
                    last_err = Some(anyhow!(e));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("request failed after 5 retries")))
    }
}

impl Default for AnimehubClient {
    fn default() -> Self {
        Self::new().expect("failed to build Animehub HTTP client")
    }
}

impl AnimeProvider for AnimehubClient {
    async fn search_shows(&self, query: &str, _translation: Translation) -> Result<Vec<ShowInfo>> {
        let query_clean = query.trim().replace(':', "");
        if query_clean.is_empty() {
            bail!("empty search query");
        }

        let search_url = format!("{ANIMEHUB_BASE_URL}/search");
        let first_req = self
            .client
            .get(&search_url)
            .query(&[("keyword", &query_clean)]);
        let html_text = self.fetch_string_with_retry(first_req).await?;

        let doc = Html::parse_document(&html_text);

        let total_sel =
            Selector::parse("span.total").map_err(|e| anyhow!("invalid total selector: {e}"))?;
        let pages: usize = match doc.select(&total_sel).next() {
            Some(el) => el
                .text()
                .collect::<String>()
                .trim()
                .parse::<usize>()
                .unwrap_or(0)
                .min(2),
            None => 0,
        };

        if pages == 0 {
            return Ok(Vec::new());
        }

        let film_item_sel = Selector::parse("div.film-list div.item")
            .map_err(|e| anyhow!("invalid film item selector: {e}"))?;
        let name_sel =
            Selector::parse("a.name").map_err(|e| anyhow!("invalid name selector: {e}"))?;
        let lang_sel = Selector::parse("span.sub, span.dub")
            .map_err(|e| anyhow!("invalid lang selector: {e}"))?;
        let ep_sel = Selector::parse("span.eps, span.ep, span.tick-eps, div.status, span.tick-item, span.total-ep, div.episodes").ok();
        let re_num = regex::Regex::new(r#"(\d+)"#).map_err(|e| anyhow!("invalid regex: {e}"))?;

        let mut results_order = Vec::new();
        let mut results_map: HashMap<String, (String, bool, bool, usize)> = HashMap::new();

        for p in 1..=pages {
            let page_doc = if p == 1 {
                doc.clone()
            } else {
                let req = self
                    .client
                    .get(&search_url)
                    .query(&[("keyword", &query_clean), ("page", &p.to_string())]);
                let page_text = self.fetch_string_with_retry(req).await?;
                Html::parse_document(&page_text)
            };

            for item in page_doc.select(&film_item_sel) {
                let Some(name_el) = item.select(&name_sel).next() else {
                    continue;
                };

                let mut link = name_el.value().attr("href").unwrap_or_default().to_string();
                if link.is_empty() {
                    continue;
                }

                if link.ends_with("-dub") {
                    link.truncate(link.len() - 4);
                }

                let mut raw_name = name_el.text().collect::<String>().trim().to_string();
                if raw_name.ends_with(" (Dub)") {
                    raw_name.truncate(raw_name.len() - 6);
                }
                if raw_name.ends_with(" Dub") {
                    raw_name.truncate(raw_name.len() - 4);
                }
                let name = raw_name.trim().to_string();

                let mut is_dub = false;
                if let Some(lang_el) = item.select(&lang_sel).next() {
                    let lang_text = lang_el.text().collect::<String>().trim().to_uppercase();
                    let lang_class = lang_el.value().attr("class").unwrap_or_default();
                    if lang_text == "DUB" || lang_class.contains("dub") {
                        is_dub = true;
                    }
                }

                let mut ep_count = 0;
                if let Some(ref sel) = ep_sel {
                    for el in item.select(sel) {
                        let text = el.text().collect::<String>();
                        for cap in re_num.captures_iter(&text) {
                            if let Ok(n) = cap[1].parse::<usize>() {
                                if n > ep_count {
                                    ep_count = n;
                                }
                            }
                        }
                    }
                }

                if let Some(entry) = results_map.get_mut(&link) {
                    if is_dub {
                        entry.2 = true;
                    } else {
                        entry.1 = true;
                    }
                    if ep_count > entry.3 {
                        entry.3 = ep_count;
                    }
                } else {
                    results_order.push(link.clone());
                    results_map.insert(link, (name, !is_dub, is_dub, ep_count));
                }
            }
        }

        let mut shows = Vec::new();
        for link in results_order {
            if let Some((name, has_sub, has_dub, ep_count)) = results_map.remove(&link) {
                let count = if ep_count > 0 { ep_count } else { 1 };
                shows.push(ShowInfo {
                    id: link,
                    title: name,
                    mal_id: None,
                    available_eps: EpisodeCounts {
                        sub: if has_sub { count } else { 0 },
                        dub: if has_dub { count } else { 0 },
                    },
                });
            }
        }

        Ok(shows)
    }

    async fn fetch_episodes(
        &self,
        show_id: &str,
        translation: Translation,
    ) -> Result<Vec<String>> {
        let mut identifier = show_id.to_string();
        if translation == Translation::Dub && !identifier.ends_with("-dub") {
            identifier.push_str("-dub");
        }

        let anime_url = if identifier.starts_with('/') {
            format!("{ANIMEHUB_BASE_URL}{identifier}")
        } else {
            format!("{ANIMEHUB_BASE_URL}/{identifier}")
        };

        let slug = identifier
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&identifier);

        let ep_api_url = format!("{ANIMEHUB_BASE_URL}/ajax/film/sv?id={slug}");
        let req = self
            .client
            .get(&ep_api_url)
            .header("Referer", &anime_url);

        let body = self.fetch_string_with_retry(req).await?;

        #[derive(Deserialize)]
        struct EpApiResponse {
            html: String,
        }

        let json_resp: EpApiResponse = serde_json::from_str(&body)
            .context("failed to parse episode list JSON from AnimeHub")?;

        let doc = Html::parse_fragment(&json_resp.html);
        let ep_sel = Selector::parse("ul.episodes li a[data-id]")
            .or_else(|_| Selector::parse("ul.episodes li a"))
            .map_err(|e| anyhow!("invalid episode selector: {e}"))?;

        let mut episodes = Vec::new();
        for el in doc.select(&ep_sel) {
            let data_id = el.value().attr("data-id").unwrap_or_default();
            if data_id.is_empty() {
                continue;
            }
            let ep_num_str = data_id.rsplit('/').next().unwrap_or(data_id);
            if let Ok(num) = ep_num_str.parse::<usize>() {
                episodes.push(num.to_string());
            } else if !ep_num_str.is_empty() {
                episodes.push(ep_num_str.to_string());
            }
        }

        Ok(episodes)
    }

    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>> {
        let mut identifier = show_id.to_string();
        if translation == Translation::Dub && !identifier.ends_with("-dub") {
            identifier.push_str("-dub");
        }

        let anime_url = if identifier.starts_with('/') {
            format!("{ANIMEHUB_BASE_URL}{identifier}")
        } else {
            format!("{ANIMEHUB_BASE_URL}/{identifier}")
        };

        let slug = identifier
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&identifier);

        let server = 0;
        let ep_info_url =
            format!("{ANIMEHUB_BASE_URL}/ajax/episode/info?epr={slug}/{episode}/{server}");
        let req = self
            .client
            .get(&ep_info_url)
            .header("Referer", &anime_url);

        let body = self.fetch_string_with_retry(req).await?;

        #[derive(Deserialize)]
        struct EpInfoResponse {
            target: String,
        }

        let info_resp: EpInfoResponse = serde_json::from_str(&body)
            .context("failed to parse episode info JSON from AnimeHub")?;

        let target = info_resp.target;
        let target_url = Url::parse(&target).context("failed to parse target URL")?;
        let target_base = format!(
            "{}://{}",
            target_url.scheme(),
            target_url.host_str().unwrap_or_default()
        );

        let req = self
            .client
            .get(&target)
            .header("Referer", &anime_url);
        let target_html = self.fetch_string_with_retry(req).await?;

        let re_zrpart2 = regex::Regex::new(r#"var\s+zrpart2\s*=\s*['"]([^'"]+)['"]"#)?;
        let zrpart2 = re_zrpart2
            .captures(&target_html)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .ok_or_else(|| anyhow!("failed to extract zrpart2 from target page"))?;

        let encoded_zrpart2 =
            url::form_urlencoded::byte_serialize(zrpart2.as_bytes()).collect::<String>();
        let hs_url = format!("{target_base}/hs/{encoded_zrpart2}?pl_usn=1");

        let req = self.client.get(&hs_url);
        let hs_html = self.fetch_string_with_retry(req).await?;

        let doc = Html::parse_document(&hs_html);
        let sources_sel = Selector::parse("div#sources")
            .map_err(|e| anyhow!("invalid sources selector: {e}"))?;
        let sources_el = doc
            .select(&sources_sel)
            .next()
            .ok_or_else(|| anyhow!("div#sources not found in hs page"))?;

        let sources_text = sources_el.text().collect::<String>();

        #[derive(Deserialize)]
        struct SourcesObj {
            sources: serde_json::Value,
        }

        let sources_data: SourcesObj = serde_json::from_str(&sources_text)
            .context("failed to parse JSON inside div#sources")?;

        let sources_url = match sources_data.sources {
            serde_json::Value::String(s) => s,
            serde_json::Value::Array(ref arr) if !arr.is_empty() => {
                arr[0].as_str().unwrap_or_default().to_string()
            }
            _ => bail!("invalid sources format in div#sources"),
        };

        if sources_url.is_empty() {
            bail!("empty sources URL");
        }

        let req = self
            .client
            .get(&sources_url)
            .header("Referer", format!("{target_base}/"));
        let m3u8_text = self.fetch_string_with_retry(req).await?;

        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), format!("{target_base}/"));
        headers.insert("User-Agent".to_string(), USER_AGENT.to_string());

        let base_m3u8_url = Url::parse(&sources_url).ok();
        let mut streams = Vec::new();
        let lines: Vec<&str> = m3u8_text.lines().collect();
        let re_res = regex::Regex::new(r#"RESOLUTION=\d+x(\d+)"#)?;

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
                        let full_url = match &base_m3u8_url {
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
                        streams.push(StreamOption {
                            provider: "animehub".to_string(),
                            url: full_url,
                            quality_label,
                            quality_rank: height,
                            is_hls: true,
                            headers: headers.clone(),
                            subtitle: None,
                        });
                    }
                }
            }
        }

        if streams.is_empty() {
            streams.push(StreamOption {
                provider: "animehub".to_string(),
                url: sources_url,
                quality_label: "1080p".to_string(),
                quality_rank: 1080,
                is_hls: true,
                headers,
                subtitle: None,
            });
        }

        Ok(streams)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_zrpart2() {
        let html = r#"
            <html>
                <script>
                    var zrpart2 = 'abcd1234efgh5678';
                </script>
            </html>
        "#;
        let re_zrpart2 = regex::Regex::new(r#"var\s+zrpart2\s*=\s*['"]([^'"]+)['"]"#).unwrap();
        let cap = re_zrpart2.captures(html).unwrap();
        assert_eq!(&cap[1], "abcd1234efgh5678");
    }

    #[test]
    fn test_parse_sources_div_json() {
        let json_str = r#"{"sources":"https://example.com/stream/master.m3u8"}"#;
        #[derive(Deserialize)]
        struct SourcesObj {
            sources: serde_json::Value,
        }
        let parsed: SourcesObj = serde_json::from_str(json_str).unwrap();
        assert_eq!(
            parsed.sources.as_str().unwrap(),
            "https://example.com/stream/master.m3u8"
        );
    }

    #[test]
    fn test_dub_stripping_and_link_normalization() {
        let mut link = "/v/naruto-shippuden-dub".to_string();
        if link.ends_with("-dub") {
            link.truncate(link.len() - 4);
        }
        assert_eq!(link, "/v/naruto-shippuden");

        let mut name = "Naruto Shippuden (Dub)".to_string();
        if name.ends_with(" (Dub)") {
            name.truncate(name.len() - 6);
        }
        assert_eq!(name, "Naruto Shippuden");
    }

    #[test]
    fn test_episodes_html_parsing() {
        let html_json = r#"
            <ul class="episodes">
                <li><a data-id="naruto-shippuden/1">1</a></li>
                <li><a data-id="naruto-shippuden/2">2</a></li>
                <li><a data-id="naruto-shippuden/3">3</a></li>
            </ul>
        "#;
        let doc = Html::parse_fragment(html_json);
        let ep_sel = Selector::parse("ul.episodes li a[data-id]").unwrap();

        let mut episodes = Vec::new();
        for el in doc.select(&ep_sel) {
            let data_id = el.value().attr("data-id").unwrap_or_default();
            let ep_num_str = data_id.rsplit('/').next().unwrap_or(data_id);
            episodes.push(ep_num_str.to_string());
        }
        assert_eq!(episodes, vec!["1", "2", "3"]);
    }
}
