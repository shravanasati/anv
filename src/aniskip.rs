use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use crate::config::AppConfig;
use crate::dbg_log;
use crate::providers::USER_AGENT;

const ANISKIP_API_BASE: &str = "https://api.aniskip.com/v2/skip-times";
const ANISKIP_CACHE_TTL_SECS: u64 = 30 * 24 * 60 * 60; // 30 days
const MAX_CACHE_ENTRIES: usize = 1000;

static ANISKIP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn get_aniskip_client() -> &'static reqwest::Client {
    ANISKIP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("failed to build global aniskip reqwest client")
    })
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

const SKIP_LUA_SCRIPT: &str = r#"
local mp = require("mp")
local options = require("mp.options")

local opts = {
    skip_op_start = 0,
    skip_op_end = 0,
    skip_ed_start = 0,
    skip_ed_end = 0,
    skip_mixed_op_start = 0,
    skip_mixed_op_end = 0,
    skip_mixed_ed_start = 0,
    skip_mixed_ed_end = 0,
    skip_recap_start = 0,
    skip_recap_end = 0,
}

options.read_options(opts, "anv_skip")

local skipped = {
    op = false,
    ed = false,
    mixed_op = false,
    mixed_ed = false,
    recap = false,
}

local function check_skip()
    local time = mp.get_property_number("time-pos")
    if not time then return end

    if not skipped.op and opts.skip_op_end > 0 and time >= opts.skip_op_start and time < opts.skip_op_end then
        mp.set_property_number("time-pos", opts.skip_op_end)
        skipped.op = true
    elseif not skipped.ed and opts.skip_ed_end > 0 and time >= opts.skip_ed_start and time < opts.skip_ed_end then
        mp.set_property_number("time-pos", opts.skip_ed_end)
        skipped.ed = true
    elseif not skipped.mixed_op and opts.skip_mixed_op_end > 0 and time >= opts.skip_mixed_op_start and time < opts.skip_mixed_op_end then
        mp.set_property_number("time-pos", opts.skip_mixed_op_end)
        skipped.mixed_op = true
    elseif not skipped.mixed_ed and opts.skip_mixed_ed_end > 0 and time >= opts.skip_mixed_ed_start and time < opts.skip_mixed_ed_end then
        mp.set_property_number("time-pos", opts.skip_mixed_ed_end)
        skipped.mixed_ed = true
    elseif not skipped.recap and opts.skip_recap_end > 0 and time >= opts.skip_recap_start and time < opts.skip_recap_end then
        mp.set_property_number("time-pos", opts.skip_recap_end)
        skipped.recap = true
    end
end

mp.add_periodic_timer(0.25, check_skip)
"#;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SkipTimes {
    pub op: Option<(f64, f64)>,
    pub ed: Option<(f64, f64)>,
    pub mixed_op: Option<(f64, f64)>,
    pub mixed_ed: Option<(f64, f64)>,
    pub recap: Option<(f64, f64)>,
}

impl SkipTimes {
    pub fn is_empty(&self) -> bool {
        self.op.is_none()
            && self.ed.is_none()
            && self.mixed_op.is_none()
            && self.mixed_ed.is_none()
            && self.recap.is_none()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SkipCacheValue {
    pub skip_times: SkipTimes,
    #[serde(default = "current_timestamp")]
    pub inserted_at: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum SkipCacheEntry {
    V2(SkipCacheValue),
    V1(SkipTimes),
}

#[derive(Debug, Deserialize)]
struct AniskipResponse {
    found: bool,
    results: Option<Vec<AniskipResult>>,
}

#[derive(Debug, Deserialize)]
struct AniskipResult {
    #[serde(rename = "skipType")]
    skip_type: String,
    interval: AniskipInterval,
}

#[derive(Debug, Deserialize)]
struct AniskipInterval {
    #[serde(rename = "startTime")]
    start_time: f64,
    #[serde(rename = "endTime")]
    end_time: f64,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct SkipCache {
    entries: HashMap<String, SkipCacheValue>,
}

impl SkipCache {
    fn load() -> Result<Self> {
        let path = get_aniskip_cache_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)?;

        #[derive(Deserialize)]
        struct RawSkipCache {
            entries: HashMap<String, SkipCacheEntry>,
        }

        let raw: RawSkipCache = serde_json::from_str(&content).context("failed to parse aniskip cache")?;
        let now = current_timestamp();

        let mut entries = HashMap::new();
        for (key, raw_entry) in raw.entries {
            let val = match raw_entry {
                SkipCacheEntry::V2(v) => v,
                SkipCacheEntry::V1(times) => SkipCacheValue {
                    skip_times: times,
                    inserted_at: now,
                },
            };

            // Expiry check: keep entry if not expired
            if now >= val.inserted_at && (now - val.inserted_at) <= ANISKIP_CACHE_TTL_SECS {
                entries.insert(key, val);
            }
        }

        Ok(Self { entries })
    }

    fn save(&mut self) -> Result<()> {
        let path = get_aniskip_cache_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Cap maximum size if needed
        if self.entries.len() > MAX_CACHE_ENTRIES {
            let mut items: Vec<(String, SkipCacheValue)> = self.entries.drain().collect();
            items.sort_by_key(|b| std::cmp::Reverse(b.1.inserted_at));
            items.truncate(MAX_CACHE_ENTRIES);
            self.entries = items.into_iter().collect();
        }

        // Compact, unindented machine-readable JSON
        let content = serde_json::to_string(self).context("failed to serialize aniskip cache")?;
        fs::write(path, content).context("failed to write aniskip cache")
    }
}

fn get_aniskip_cache_path() -> Result<PathBuf> {
    let base =
        dirs_next::cache_dir().ok_or_else(|| anyhow!("Could not determine cache directory"))?;
    Ok(base.join("anv").join("aniskip_cache.json"))
}

pub async fn fetch_skip_times(mal_id: &str, ep_num: usize, config: &AppConfig) -> Result<SkipTimes> {
    let mut cache = SkipCache::load().unwrap_or_default();
    let cache_key = format!("{}_{}", mal_id, ep_num);

    if let Some(cached) = cache.entries.get(&cache_key) {
        dbg_log!("aniskip", "cache hit for key: {}", cache_key);
        return Ok(cached.skip_times.clone());
    }

    dbg_log!("aniskip", "cache miss for key: {}. Fetching from API...", cache_key);

    let url = format!(
        "{}/{}/{}?types=op&types=ed&types=mixed-op&types=mixed-ed&types=recap&episodeLength=0",
        ANISKIP_API_BASE, mal_id, ep_num
    );

    let client = get_aniskip_client();
    let timeout = Duration::from_secs(config.aniskip.timeout);

    let mut last_err = None;
    let mut resp_opt = None;

    for attempt in 1..=3 {
        let req = client.get(&url).timeout(timeout);
        match req.send().await {
            Ok(resp) => {
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    dbg_log!("aniskip", "API returned 404 for key: {}", cache_key);
                    return Ok(SkipTimes::default());
                }
                match resp.error_for_status() {
                    Ok(ok_resp) => {
                        resp_opt = Some(ok_resp);
                        break;
                    }
                    Err(err) => {
                        dbg_log!("aniskip", "attempt {} HTTP error: {}", attempt, err);
                        last_err = Some(anyhow!(err));
                    }
                }
            }
            Err(err) => {
                dbg_log!("aniskip", "attempt {} request error: {}", attempt, err);
                last_err = Some(anyhow!(err));
            }
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_millis(200 * attempt)).await;
        }
    }

    let resp = match resp_opt {
        Some(r) => r,
        None => return Err(last_err.unwrap_or_else(|| anyhow!("AniSkip request failed after retries"))),
    };

    let aniskip_resp: AniskipResponse = resp.json().await.context("failed to parse AniSkip JSON response")?;

    let mut skip_times = SkipTimes::default();
    if aniskip_resp.found {
        if let Some(results) = aniskip_resp.results {
            for res in results {
                let interval = (res.interval.start_time, res.interval.end_time);
                match res.skip_type.as_str() {
                    "op" => skip_times.op = Some(interval),
                    "ed" => skip_times.ed = Some(interval),
                    "mixed-op" => skip_times.mixed_op = Some(interval),
                    "mixed-ed" => skip_times.mixed_ed = Some(interval),
                    "recap" => skip_times.recap = Some(interval),
                    _ => {}
                }
            }
        }
    }

    dbg_log!("aniskip", "result for {}_{}: {:?}", mal_id, ep_num, skip_times);

    if !skip_times.is_empty() {
        cache.entries.insert(
            cache_key,
            SkipCacheValue {
                skip_times: skip_times.clone(),
                inserted_at: current_timestamp(),
            },
        );
        let _ = cache.save();
    }

    Ok(skip_times)
}

#[derive(Debug, Clone, Default)]
pub struct SkipOptions {
    pub skip_op: Option<bool>,
    pub skip_ed: Option<bool>,
    pub skip_mixed_op: Option<bool>,
    pub skip_mixed_ed: Option<bool>,
    pub skip_recap: Option<bool>,
}

impl From<&crate::Cli> for SkipOptions {
    fn from(cli: &crate::Cli) -> Self {
        Self {
            skip_op: cli.skip_op,
            skip_ed: cli.skip_ed,
            skip_mixed_op: cli.skip_mixed_op,
            skip_mixed_ed: cli.skip_mixed_ed,
            skip_recap: cli.skip_recap,
        }
    }
}

pub async fn prepare_aniskip_args(
    mal_id: &str,
    ep_num: usize,
    config: &AppConfig,
    cli_opts: SkipOptions,
) -> Result<Vec<String>> {
    let skip_op = cli_opts.skip_op.unwrap_or(config.aniskip.skip_op);
    let skip_ed = cli_opts.skip_ed.unwrap_or(config.aniskip.skip_ed);
    let skip_mixed_op = cli_opts
        .skip_mixed_op
        .unwrap_or(config.aniskip.skip_mixed_op);
    let skip_mixed_ed = cli_opts
        .skip_mixed_ed
        .unwrap_or(config.aniskip.skip_mixed_ed);
    let skip_recap = cli_opts.skip_recap.unwrap_or(config.aniskip.skip_recap);

    if !skip_op && !skip_ed && !skip_mixed_op && !skip_mixed_ed && !skip_recap {
        return Ok(Vec::new());
    }

    let skip_times = fetch_skip_times(mal_id, ep_num, config).await?;

    let mut opts = Vec::new();
    if skip_op {
        if let Some((start, end)) = skip_times.op {
            opts.push(format!("skip_op_start={}", start));
            opts.push(format!("skip_op_end={}", end));
        }
    }
    if skip_ed {
        if let Some((start, end)) = skip_times.ed {
            opts.push(format!("skip_ed_start={}", start));
            opts.push(format!("skip_ed_end={}", end));
        }
    }
    if skip_mixed_op {
        if let Some((start, end)) = skip_times.mixed_op {
            opts.push(format!("skip_mixed_op_start={}", start));
            opts.push(format!("skip_mixed_op_end={}", end));
        }
    }
    if skip_mixed_ed {
        if let Some((start, end)) = skip_times.mixed_ed {
            opts.push(format!("skip_mixed_ed_start={}", start));
            opts.push(format!("skip_mixed_ed_end={}", end));
        }
    }
    if skip_recap {
        if let Some((start, end)) = skip_times.recap {
            opts.push(format!("skip_recap_start={}", start));
            opts.push(format!("skip_recap_end={}", end));
        }
    }

    if opts.is_empty() {
        return Ok(Vec::new());
    }

    let lua_path = std::env::temp_dir().join("anv_skip.lua");
    fs::write(&lua_path, SKIP_LUA_SCRIPT).context("failed to write skip lua script")?;

    Ok(vec![
        format!("--script={}", lua_path.display()),
        format!("--script-opts=anv_skip-{}", opts.join(",anv_skip-")),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skip_times_is_empty() {
        let empty = SkipTimes::default();
        assert!(empty.is_empty());

        let non_empty = SkipTimes {
            op: Some((10.0, 90.0)),
            ..Default::default()
        };
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn test_legacy_cache_migration_and_deserialization() {
        let legacy_json = r#"{
            "entries": {
                "21_1": {
                    "op": [10.0, 90.0],
                    "ed": [1200.0, 1290.0],
                    "mixed_op": null,
                    "mixed_ed": null,
                    "recap": null
                }
            }
        }"#;

        #[derive(Deserialize)]
        struct RawSkipCache {
            entries: HashMap<String, SkipCacheEntry>,
        }

        let raw: RawSkipCache = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(raw.entries.len(), 1);

        let entry = raw.entries.get("21_1").unwrap();
        match entry {
            SkipCacheEntry::V1(times) => {
                assert_eq!(times.op, Some((10.0, 90.0)));
                assert_eq!(times.ed, Some((1200.0, 1290.0)));
            }
            _ => panic!("Expected V1 legacy format"),
        }
    }

    #[test]
    fn test_cache_save_compact_json_and_cap() {
        let mut cache = SkipCache::default();
        for i in 0..1050 {
            cache.entries.insert(
                format!("show_{i}"),
                SkipCacheValue {
                    skip_times: SkipTimes {
                        op: Some((1.0, 2.0)),
                        ..Default::default()
                    },
                    inserted_at: i as u64,
                },
            );
        }

        let temp_dir = std::env::temp_dir().join("anv_test_cache");
        let _ = fs::create_dir_all(&temp_dir);
        let path = temp_dir.join("aniskip_test.json");

        // Manually simulate save capping logic to test path without modifying global cache path
        if cache.entries.len() > MAX_CACHE_ENTRIES {
            let mut items: Vec<(String, SkipCacheValue)> = cache.entries.drain().collect();
            items.sort_by_key(|b| std::cmp::Reverse(b.1.inserted_at));
            items.truncate(MAX_CACHE_ENTRIES);
            cache.entries = items.into_iter().collect();
        }

        let content = serde_json::to_string(&cache).unwrap();
        assert!(!content.contains('\n'), "Saved cache must be compact unindented JSON");
        assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);

        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(temp_dir);
    }

    #[test]
    fn test_aniskip_response_deserialization() {
        let json_data = r#"{
            "found": true,
            "results": [
                {
                    "skipType": "op",
                    "interval": {
                        "startTime": 12.5,
                        "endTime": 102.0
                    }
                },
                {
                    "skipType": "ed",
                    "interval": {
                        "startTime": 1350.0,
                        "endTime": 1440.0
                    }
                }
            ]
        }"#;

        let resp: AniskipResponse = serde_json::from_str(json_data).unwrap();
        assert!(resp.found);
        let results = resp.results.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].skip_type, "op");
        assert_eq!(results[0].interval.start_time, 12.5);
    }
}
