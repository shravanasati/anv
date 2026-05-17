use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::config::AppConfig;

const ANISKIP_API_BASE: &str = "https://api.aniskip.com/v2/skip-times";

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

local function check_skip()
    local time = mp.get_property_number("time-pos")
    if not time then return end

    if opts.skip_op_end > 0 and time >= opts.skip_op_start and time < opts.skip_op_end then
        mp.set_property_number("time-pos", opts.skip_op_end)
    elseif opts.skip_ed_end > 0 and time >= opts.skip_ed_start and time < opts.skip_ed_end then
        mp.set_property_number("time-pos", opts.skip_ed_end)
    elseif opts.skip_mixed_op_end > 0 and time >= opts.skip_mixed_op_start and time < opts.skip_mixed_op_end then
        mp.set_property_number("time-pos", opts.skip_mixed_op_end)
    elseif opts.skip_mixed_ed_end > 0 and time >= opts.skip_mixed_ed_start and time < opts.skip_mixed_ed_end then
        mp.set_property_number("time-pos", opts.skip_mixed_ed_end)
    elseif opts.skip_recap_end > 0 and time >= opts.skip_recap_start and time < opts.skip_recap_end then
        mp.set_property_number("time-pos", opts.skip_recap_end)
    end
end

mp.add_periodic_timer(0.25, check_skip)
"#;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkipTimes {
    pub op: Option<(f64, f64)>,
    pub ed: Option<(f64, f64)>,
    pub mixed_op: Option<(f64, f64)>,
    pub mixed_ed: Option<(f64, f64)>,
    pub recap: Option<(f64, f64)>,
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
    entries: HashMap<String, SkipTimes>,
}

impl SkipCache {
    fn load() -> Result<Self> {
        let path = get_aniskip_cache_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)?;
        serde_json::from_str(&content).context("failed to parse aniskip cache")
    }

    fn save(&self) -> Result<()> {
        let path = get_aniskip_cache_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content).context("failed to write aniskip cache")
    }
}

fn get_aniskip_cache_path() -> Result<PathBuf> {
    let base =
        dirs_next::cache_dir().ok_or_else(|| anyhow!("Could not determine cache directory"))?;
    Ok(base.join("anv").join("aniskip_cache.json"))
}

pub async fn fetch_skip_times(mal_id: &str, episode: &str) -> Result<SkipTimes> {
    let mut cache = SkipCache::load().unwrap_or_default();
    let cache_key = format!("{}_{}", mal_id, episode);
    let debug = std::env::var("ANV_DEBUG").is_ok();

    if let Some(cached) = cache.entries.get(&cache_key) {
        if debug {
            eprintln!("[ANV_DEBUG] AniSkip cache hit for key: {}", cache_key);
        }
        return Ok(cached.clone());
    }

    if debug {
        eprintln!(
            "[ANV_DEBUG] AniSkip cache miss for key: {}. Fetching from API...",
            cache_key
        );
    }

    let url = format!(
        "{}/{}/{}?types=op&types=ed&types=mixed-op&types=mixed-ed&types=recap&episodeLength=0",
        ANISKIP_API_BASE, mal_id, episode
    );
    let client = reqwest::Client::builder().user_agent("anv").build()?;
    let resp: AniskipResponse = client.get(url).send().await?.json().await?;

    let mut skip_times = SkipTimes::default();
    if resp.found {
        if let Some(results) = resp.results {
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

    if debug {
        eprintln!(
            "[ANV_DEBUG] AniSkip result for {}_{}: {:?}",
            mal_id, episode, skip_times
        );
    }

    cache.entries.insert(cache_key, skip_times.clone());
    let _ = cache.save();
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

pub async fn prepare_aniskip_args(
    mal_id: &str,
    episode: &str,
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

    let skip_times = fetch_skip_times(mal_id, episode).await?;

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
