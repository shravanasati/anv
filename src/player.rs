use anyhow::{Result, anyhow, bail};
use dialoguer::Select;
use tokio::process::Command;

use crate::aniskip::{SkipOptions, prepare_aniskip_args};
use crate::config::{AppConfig, Quality};
use crate::history::theme;
use crate::types::StreamOption;

pub const PLAYER_ENV_KEY: &str = "ANV_PLAYER";

pub fn detect_player(config: &AppConfig) -> String {
    std::env::var(PLAYER_ENV_KEY)
        .ok()
        .filter(|val| !val.trim().is_empty())
        .unwrap_or_else(|| config.player.clone())
}

fn build_command(player: &str) -> Result<Command> {
    let parts = shlex::split(player)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("Invalid player command: '{}'", player))?;
    let (bin, args) = parts
        .split_first()
        .ok_or_else(|| anyhow!("Player command is empty"))?;
    let mut cmd = Command::new(bin);
    cmd.args(args);
    Ok(cmd)
}

pub fn choose_stream(mut options: Vec<StreamOption>) -> Result<Option<StreamOption>> {
    if options.len() == 1 {
        return Ok(Some(options.remove(0)));
    }
    let labels: Vec<String> = options.iter().map(StreamOption::label).collect();
    let selection = Select::with_theme(&theme())
        .with_prompt("Select a stream")
        .items(&labels)
        .default(0)
        .interact_opt()?;
    let Some(idx) = selection else {
        return Ok(None);
    };
    Ok(Some(options.remove(idx)))
}

/// Pick a stream according to the global stream quality policy from config.
pub fn select_stream_by_quality(
    mut options: Vec<StreamOption>,
    quality: Quality,
) -> Result<Option<StreamOption>> {
    if options.is_empty() {
        return Ok(None);
    }
    match quality {
        Quality::Highest => {
            let max_idx = options
                .iter()
                .enumerate()
                .max_by_key(|(_, s)| s.quality_rank)
                .map(|(i, _)| i)
                .unwrap_or(0);
            Ok(Some(options.remove(max_idx)))
        }
        Quality::Lowest => {
            let mut min_idx = 0;
            let mut min_rank = i32::MAX;
            for (i, s) in options.iter().enumerate() {
                if s.quality_rank <= min_rank {
                    min_rank = s.quality_rank;
                    min_idx = i;
                }
            }
            Ok(Some(options.remove(min_idx)))
        }
        Quality::Select => choose_stream(options),
    }
}

pub async fn launch_player(
    stream: &StreamOption,
    title: &str,
    episode: &str,
    ep_num: usize,
    mal_id: Option<&str>,
    config: &AppConfig,
    skip_opts: SkipOptions,
) -> Result<()> {
    let debug = crate::logger::is_debug();
    let player = detect_player(config);
    let mut cmd = build_command(&player)?;
    let media_title = format!("{title} - Episode {episode}");
    if !debug {
        cmd.arg("--quiet");
        cmd.arg("--terminal=no");
    }
    cmd.arg(format!("--force-media-title={media_title}"));

    if let Some(mid) = mal_id {
        match prepare_aniskip_args(mid, ep_num, config, skip_opts).await {
            Ok(args) => {
                cmd.args(args);
            }
            Err(err) => {
                crate::dbg_log!("aniskip", "error: {err}");
            }
        }
    }

    if let Some(sub) = &stream.subtitle {
        cmd.arg(format!("--sub-file={sub}"));
    }
    for (key, value) in &stream.headers {
        if key.eq_ignore_ascii_case("user-agent") {
            cmd.arg(format!("--user-agent={value}"));
        } else if key.eq_ignore_ascii_case("referer") {
            cmd.arg(format!("--referrer={value}"));
            cmd.arg(format!("--http-header-fields=Referer: {value}"));
        } else {
            cmd.arg(format!("--http-header-fields={}: {value}", key));
        }
    }
    if stream.is_hls {
        cmd.arg("--demuxer-lavf-format=hls");
    }

    let player_url = format_hls_player_url(&stream.url, stream.is_hls);

    cmd.arg(&player_url);

    crate::dbg_log!("player", "Launching player command: {:?}", cmd);

    let output = match cmd.output().await {
        Ok(output) => output,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                let bin = shlex::split(&player)
                    .and_then(|v| v.into_iter().next())
                    .unwrap_or_else(|| player.to_string());
                return Err(anyhow!(
                    "Player binary '{}' not found. Install it or set {} to a valid command.",
                    bin,
                    PLAYER_ENV_KEY
                ));
            }
            return Err(anyhow!(err).context(format!("failed to launch player '{player}'")));
        }
    };

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str(stdout.trim());
    }
    if !stderr.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr.trim());
    }

    if !combined.is_empty() {
        crate::dbg_log!("player", "mpv output:\n{combined}");
    }

    if !output.status.success() {
        if !combined.is_empty() {
            bail!(
                "player exited with status {}\nmpv error:\n{combined}",
                output.status
            );
        } else {
            bail!("player exited with status {}", output.status);
        }
    }
    Ok(())
}



pub fn format_hls_player_url(url: &str, is_hls: bool) -> String {
    if is_hls && !url.contains(".m3u8") {
        if url.contains('#') {
            format!("{url}.m3u8")
        } else {
            format!("{url}#.m3u8")
        }
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_hls_player_url() {
        let raw_url = "https://imgcdn44.dpopdrop89.store/cdn/092e3d2d1473";
        let formatted = format_hls_player_url(raw_url, true);
        assert_eq!(
            formatted,
            "https://imgcdn44.dpopdrop89.store/cdn/092e3d2d1473#.m3u8"
        );

        let m3u8_url = "https://example.com/playlist.m3u8";
        assert_eq!(format_hls_player_url(m3u8_url, true), m3u8_url);
    }

    #[test]
    fn test_select_stream_by_quality() {
        use std::collections::HashMap;

        let make_option = |label: &str, rank: i32| StreamOption {
            provider: "test".to_string(),
            url: format!("http://test/{label}"),
            quality_label: label.to_string(),
            quality_rank: rank,
            is_hls: true,
            headers: HashMap::new(),
            subtitle: None,
        };

        let options = vec![
            make_option("1080p", 1080),
            make_option("720p", 720),
            make_option("360p", 360),
        ];

        let highest = select_stream_by_quality(options.clone(), Quality::Highest)
            .unwrap()
            .unwrap();
        assert_eq!(highest.quality_label, "1080p");

        let lowest = select_stream_by_quality(options.clone(), Quality::Lowest)
            .unwrap()
            .unwrap();
        assert_eq!(lowest.quality_label, "360p");

        // Test with options in increasing order (e.g. AnimeHub style before sorting)
        let inc_options = vec![
            make_option("360p", 360),
            make_option("720p", 720),
            make_option("1080p", 1080),
        ];
        let highest_inc = select_stream_by_quality(inc_options.clone(), Quality::Highest)
            .unwrap()
            .unwrap();
        assert_eq!(highest_inc.quality_label, "1080p");

        let lowest_inc = select_stream_by_quality(inc_options.clone(), Quality::Lowest)
            .unwrap()
            .unwrap();
        assert_eq!(lowest_inc.quality_label, "360p");
    }
}
