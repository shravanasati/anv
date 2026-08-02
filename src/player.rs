use anyhow::{Context, Result, anyhow, bail};
use dialoguer::Select;
use std::path::PathBuf;
use tokio::process::Command;

use crate::aniskip::{SkipOptions, prepare_aniskip_args};
use crate::config::{AnidbQuality, AppConfig};
use crate::history::theme;
use crate::proxy::{CachedPageTarget, LocalPageProxy};
use crate::types::{Page, StreamOption};

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

/// Pick a stream according to the AniDB quality policy from config.
/// Streams must already be sorted highest-quality-first (as `fetch_streams` guarantees).
pub fn select_stream_by_quality(
    mut options: Vec<StreamOption>,
    quality: AnidbQuality,
) -> Result<Option<StreamOption>> {
    if options.is_empty() {
        return Ok(None);
    }
    match quality {
        AnidbQuality::Highest => Ok(Some(options.remove(0))),
        AnidbQuality::Lowest => Ok(Some(options.remove(options.len() - 1))),
        AnidbQuality::Select => choose_stream(options),
    }
}

pub async fn launch_player(
    stream: &StreamOption,
    title: &str,
    episode: &str,
    mal_id: Option<&str>,
    config: &AppConfig,
    skip_opts: SkipOptions,
) -> Result<()> {
    let debug = std::env::var("ANV_DEBUG").is_ok();
    let player = detect_player(config);
    let mut cmd = build_command(&player)?;
    let media_title = format!("{title} - Episode {episode}");
    if !debug {
        cmd.arg("--quiet");
        cmd.arg("--terminal=no");
    }
    cmd.arg(format!("--force-media-title={media_title}"));

    if let Some(mid) = mal_id {
        match prepare_aniskip_args(mid, episode, config, skip_opts).await {
            Ok(args) => {
                cmd.args(args);
            }
            Err(err) => {
                eprintln!("[aniskip] error: {err}");
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
    cmd.arg(&stream.url);

    if debug {
        eprintln!("[ANV_DEBUG] Launching player command: {:?}", cmd);
    }

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

    if debug && !combined.is_empty() {
        eprintln!("[ANV_DEBUG] mpv output:\n{combined}");
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

pub async fn launch_image_viewer(
    pages: &[Page],
    cached_pages: &[Option<PathBuf>],
    cache_files: &[PathBuf],
    title: &str,
    chapter: &str,
    config: &AppConfig,
) -> Result<()> {
    let player = detect_player(config);
    let mut cmd = build_command(&player)?;
    let media_title = format!("{title} - Chapter {chapter}");
    cmd.arg("--quiet");
    cmd.arg("--terminal=no");
    cmd.arg(format!("--force-media-title={media_title}"));
    cmd.arg("--image-display-duration=inf");

    if !cached_pages.iter().any(|p| p.is_some()) {
        add_direct_url_args(&mut cmd, pages);
    } else if cached_pages.iter().all(|p| p.is_some()) {
        for path in cached_pages.iter().flatten() {
            cmd.arg(path);
        }
    } else {
        let targets: Vec<CachedPageTarget> = pages
            .iter()
            .cloned()
            .zip(cache_files.iter().cloned())
            .map(|(page, path)| CachedPageTarget { page, path })
            .collect();
        match LocalPageProxy::start(targets) {
            Ok(mut proxy) => {
                for idx in 0..pages.len() {
                    cmd.arg(proxy.page_url(idx));
                }
                println!("Launching viewer for Chapter {chapter}...");
                let status = cmd.status().await.context("failed to launch viewer")?;
                proxy.shutdown();
                if !status.success() && status.code() != Some(2) {
                    bail!("viewer exited with status {status}");
                }
                return Ok(());
            }
            Err(err) => {
                eprintln!("Local cache proxy unavailable ({err}). Falling back to direct URLs.");
                add_direct_url_args(&mut cmd, pages);
            }
        }
    }

    println!("Launching viewer for Chapter {chapter}...");
    let status = cmd.status().await.context("failed to launch viewer")?;
    if !status.success() && status.code() != Some(2) {
        bail!("viewer exited with status {status}");
    }
    Ok(())
}

fn add_direct_url_args(cmd: &mut Command, pages: &[Page]) {
    if let Some(first) = pages.first() {
        for (key, value) in &first.headers {
            if key.eq_ignore_ascii_case("referer") {
                cmd.arg(format!("--referrer={value}"));
                cmd.arg(format!("--http-header-fields=Referer: {value}"));
            } else {
                cmd.arg(format!("--http-header-fields={}: {value}", key));
            }
        }
    }
    for page in pages {
        cmd.arg(&page.url);
    }
}
