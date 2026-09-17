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

    let mut temp_sub: Option<std::path::PathBuf> = None;
    if let Some(sub) = &stream.subtitle {
        if is_remote_url(sub) {
            match download_subtitle_to_temp(sub, &stream.headers, episode).await {
                Ok(path) => {
                    cmd.arg(format!("--sub-file={}", path.display()));
                    temp_sub = Some(path);
                }
                Err(err) => {
                    crate::dbg_log!(
                        "player",
                        "subtitle prefetch failed ({err:#}); falling back to remote URL"
                    );
                    cmd.arg(format!("--sub-file={sub}"));
                }
            }
        } else {
            cmd.arg(format!("--sub-file={sub}"));
        }
    }
    apply_header_args(&mut cmd, &stream.headers);
    // Force HLS demuxing only when the URL gives no `.m3u8` hint (i.e. the
    // `#` workaround in `format_hls_player_url` kicked in). The flag is
    // process-global in mpv and would otherwise force HLS probing onto
    // `--sub-file` inputs too, breaking all external subtitles.
    if needs_hls_format_hint(&stream.url, stream.is_hls) {
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

    // Playback finished (or failed to start); the prefetched subtitle file is
    // no longer needed.
    if let Some(path) = &temp_sub {
        if let Err(err) = tokio::fs::remove_file(path).await {
            crate::dbg_log!(
                "player",
                "failed to remove temp subtitle {}: {err}",
                path.display()
            );
        }
    }

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

fn is_remote_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn sanitize_filename_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Fetch a remote subtitle file into the OS temp dir and return its path.
/// mpv opens `--sub-file` URLs through its own demuxer stack, which fails
/// against some CDN hosts (partial reads during probing), so playing from a
/// fully-fetched local file is strictly more reliable. The caller deletes the
/// file after playback.
async fn download_subtitle_to_temp(
    url: &str,
    headers: &std::collections::HashMap<String, String>,
    episode: &str,
) -> Result<std::path::PathBuf> {
    use reqwest::header::{HeaderName, HeaderValue};

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        // HTTP/1.1-only: some file-CDN WAFs (e.g. MegaPlay's) reject HTTP/2
        // requests from non-browser clients with 403 while HTTP/1.1 passes.
        .http1_only()
        .build()?;
    let mut req = client.get(url);
    for (k, v) in headers {
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) else {
            continue;
        };
        req = req.header(name, value);
    }
    let bytes = req.send().await?.error_for_status()?.bytes().await?;
    if bytes.is_empty() {
        bail!("empty subtitle file from {url}");
    }

    let ext = url
        .split('?')
        .next()
        .unwrap_or(url)
        .rsplit('.')
        .next()
        .filter(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("vtt");
    let path = std::env::temp_dir().join(format!(
        "anv-sub-{}-{}.{}",
        std::process::id(),
        sanitize_filename_part(episode),
        ext
    ));
    tokio::fs::write(&path, &bytes).await?;
    crate::dbg_log!("player", "prefetched subtitle to {}", path.display());
    Ok(path)
}

pub fn apply_header_args(cmd: &mut Command, headers: &std::collections::HashMap<String, String>) {
    for (key, value) in headers {
        if key.eq_ignore_ascii_case("user-agent") {
            cmd.arg(format!("--user-agent={value}"));
        } else if key.eq_ignore_ascii_case("referer") {
            cmd.arg(format!("--referrer={value}"));
            cmd.arg(format!("--http-header-fields=Referer: {value}"));
        } else {
            cmd.arg(format!("--http-header-fields={key}: {value}"));
        }
    }
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

/// Whether mpv needs an explicit HLS format hint: only for HLS streams whose
/// URL carries no `.m3u8` marker for auto-detection.
pub fn needs_hls_format_hint(url: &str, is_hls: bool) -> bool {
    is_hls && !url.contains(".m3u8")
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
    fn test_is_remote_url() {
        assert!(is_remote_url("https://example.com/s.vtt"));
        assert!(is_remote_url("http://example.com/s.vtt"));
        assert!(!is_remote_url("/tmp/subs.vtt"));
        assert!(!is_remote_url("file:///tmp/subs.vtt"));
    }

    #[test]
    fn test_sanitize_filename_part() {
        assert_eq!(sanitize_filename_part("12"), "12");
        assert_eq!(sanitize_filename_part("5-6"), "5-6");
        assert_eq!(sanitize_filename_part("1 (v2)"), "1__v2_");
    }

    #[test]
    fn test_needs_hls_format_hint() {
        assert!(needs_hls_format_hint(
            "https://cdn.example/stream/123",
            true
        ));
        assert!(!needs_hls_format_hint(
            "https://cdn.example/master.m3u8",
            true
        ));
        assert!(!needs_hls_format_hint(
            "https://cdn.example/video.mp4",
            false
        ));
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

    #[test]
    fn test_apply_header_args() {
        let mut cmd = Command::new("mpv");
        let mut headers = std::collections::HashMap::new();
        headers.insert("User-Agent".to_string(), "TestUA/1.0".to_string());
        headers.insert("Referer".to_string(), "https://example.com/ref".to_string());
        headers.insert("X-Custom-Header".to_string(), "custom_val".to_string());

        apply_header_args(&mut cmd, &headers);

        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();

        assert!(args.contains(&"--user-agent=TestUA/1.0".to_string()));
        assert!(args.contains(&"--referrer=https://example.com/ref".to_string()));
        assert!(
            args.contains(&"--http-header-fields=Referer: https://example.com/ref".to_string())
        );
        assert!(args.contains(&"--http-header-fields=X-Custom-Header: custom_val".to_string()));
    }
}
