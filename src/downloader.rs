use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::config::AppConfig;
use crate::types::StreamOption;

/// Supported download engines maintained internally by anv.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DownloaderEngine {
    #[default]
    Ffmpeg,
    Ytdlp,
    YtdlpAria2c,
}

impl DownloaderEngine {
    pub fn all() -> &'static [DownloaderEngine] {
        &[
            DownloaderEngine::Ffmpeg,
            DownloaderEngine::Ytdlp,
            DownloaderEngine::YtdlpAria2c,
        ]
    }
}

impl std::fmt::Display for DownloaderEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ffmpeg => write!(f, "ffmpeg"),
            Self::Ytdlp => write!(f, "ytdlp"),
            Self::YtdlpAria2c => write!(f, "ytdlp+aria2c"),
        }
    }
}

impl std::str::FromStr for DownloaderEngine {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().trim() {
            "ffmpeg" => Ok(Self::Ffmpeg),
            "ytdlp" | "yt-dlp" => Ok(Self::Ytdlp),
            "ytdlp+aria2c" | "ytdlp_aria2c" | "ytdlp-aria2c" | "yt-dlp+aria2c" | "yt-dlp-aria2c" | "yt-dlp_aria2c" => {
                Ok(Self::YtdlpAria2c)
            }
            _ => Err(format!(
                "Unknown downloader engine '{s}'. Supported downloaders: ffmpeg, ytdlp, ytdlp+aria2c"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for DownloaderEngine {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for DownloaderEngine {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Parses an episode range string into a list of matching episode labels.
/// Range string formats:
/// - Single episode: `"4"`
/// - Dash range: `"1-5"`
/// - Double dot range: `"1..5"`
pub fn parse_episode_range(range_str: &str, available_eps: &[String]) -> Result<Vec<String>> {
    let trimmed = range_str.trim();
    if trimmed.is_empty() {
        bail!("Episode range cannot be empty.");
    }

    if let Ok(single) = trimmed.parse::<f64>() {
        let matched: Vec<String> = available_eps
            .iter()
            .filter(|ep| {
                ep.parse::<f64>()
                    .map(|v| (v - single).abs() < f64::EPSILON)
                    .unwrap_or(ep.as_str() == trimmed)
            })
            .cloned()
            .collect();
        if matched.is_empty() {
            bail!("Episode '{trimmed}' not found in available episodes.");
        }
        return Ok(matched);
    }

    let (start_str, end_str) = if let Some((s, e)) = trimmed.split_once("..") {
        (s.trim(), e.trim())
    } else if let Some((s, e)) = trimmed.split_once('-') {
        (s.trim(), e.trim())
    } else {
        let matched: Vec<String> = available_eps
            .iter()
            .filter(|ep| ep.as_str() == trimmed)
            .cloned()
            .collect();
        if matched.is_empty() {
            bail!("Episode '{trimmed}' not found in available episodes.");
        }
        return Ok(matched);
    };

    let start: f64 = start_str
        .parse()
        .with_context(|| format!("Invalid start of range: '{start_str}'"))?;
    let end: f64 = end_str
        .parse()
        .with_context(|| format!("Invalid end of range: '{end_str}'"))?;

    let (min, max) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };

    let matched: Vec<String> = available_eps
        .iter()
        .filter(|ep| {
            if let Ok(val) = ep.parse::<f64>() {
                val >= min - f64::EPSILON && val <= max + f64::EPSILON
            } else {
                false
            }
        })
        .cloned()
        .collect();

    if matched.is_empty() {
        bail!("No episodes found matching range '{range_str}'.");
    }

    Ok(matched)
}

/// Sanitizes a show title or filename to be cross-platform filesystem safe
/// while preserving valid Unicode symbols (e.g. Japanese Kanji/Kana, accented text, punctuation).
pub fn sanitize_filename(title: &str) -> String {
    let sanitized: String = title
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | '?' | '*' | '<' | '>' | '|' => '_',
            ':' => ' ',
            '"' => '\'',
            c if (c as u32) < 32 || (c as u32) == 127 => '_',
            c => c,
        })
        .collect();

    // Collapse multiple consecutive spaces
    let mut cleaned = String::with_capacity(sanitized.len());
    let mut last_space = false;
    for ch in sanitized.chars() {
        if ch == ' ' {
            if !last_space {
                cleaned.push(ch);
                last_space = true;
            }
        } else {
            cleaned.push(ch);
            last_space = false;
        }
    }

    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return "download".to_string();
    }

    // Check for Windows reserved device names
    let upper = trimmed.to_uppercase();
    let is_reserved = matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );

    if is_reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// Formats an episode file name cleanly, padding single-digit numbers (e.g. ep04.mp4).
pub fn format_episode_filename(label: &str) -> String {
    if let Ok(num) = label.parse::<usize>() {
        format!("ep{:02}.mp4", num)
    } else {
        format!("ep{label}.mp4")
    }
}

pub async fn download_with_ffmpeg(stream: &StreamOption, output_path: &Path) -> Result<()> {
    let mut cmd = Command::new("ffmpeg");

    cmd.arg("-y");
    cmd.arg("-hide_banner");
    cmd.arg("-loglevel").arg("error");
    cmd.arg("-stats");

    if !stream.headers.is_empty() {
        let mut header_str = String::new();
        for (k, v) in &stream.headers {
            header_str.push_str(&format!("{k}: {v}\r\n"));
        }
        cmd.arg("-headers").arg(header_str);
    }

    cmd.arg("-protocol_whitelist")
        .arg("file,http,https,tcp,tls,crypto,data");
    cmd.arg("-allowed_extensions")
        .arg("ALL");
    cmd.arg("-allowed_segment_extensions")
        .arg("ALL");
    cmd.arg("-extension_picky")
        .arg("0");
    cmd.arg("-i").arg(&stream.url);
    cmd.arg("-c").arg("copy");
    cmd.arg("-bsf:a").arg("aac_adtstoasc");
    cmd.arg(output_path);

    let status = match cmd.status().await {
        Ok(s) => s,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                bail!(
                    "Downloader binary 'ffmpeg' not found. Please install ffmpeg or update [download].downloader in config."
                );
            }
            return Err(anyhow!(err).context("failed to execute downloader 'ffmpeg'"));
        }
    };

    if !status.success() {
        if output_path.exists() {
            let _ = fs::remove_file(output_path);
        }
        bail!("ffmpeg process failed with status {status}");
    }

    Ok(())
}

pub async fn download_with_ytdlp(stream: &StreamOption, output_path: &Path) -> Result<()> {
    let mut cmd = Command::new("yt-dlp");

    cmd.arg("-o").arg(output_path);
    cmd.arg("--no-playlist");

    for (k, v) in &stream.headers {
        cmd.arg("--add-headers").arg(format!("{k}:{v}"));
    }

    cmd.arg(&stream.url);

    let status = match cmd.status().await {
        Ok(s) => s,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                bail!(
                    "Downloader binary 'yt-dlp' not found. Please install yt-dlp or update [download].downloader in config."
                );
            }
            return Err(anyhow!(err).context("failed to execute downloader 'yt-dlp'"));
        }
    };

    if !status.success() {
        if output_path.exists() {
            let _ = fs::remove_file(output_path);
        }
        bail!("yt-dlp process failed with status {status}");
    }

    Ok(())
}

pub async fn download_with_ytdlp_aria2c(stream: &StreamOption, output_path: &Path) -> Result<()> {
    let mut cmd = Command::new("yt-dlp");

    cmd.arg("-o").arg(output_path);
    cmd.arg("--no-playlist");
    cmd.arg("--downloader").arg("aria2c");
    cmd.arg("--downloader-args").arg("aria2c:-x 16 -s 16 -k 1M");

    for (k, v) in &stream.headers {
        cmd.arg("--add-headers").arg(format!("{k}:{v}"));
    }

    cmd.arg(&stream.url);

    let status = match cmd.status().await {
        Ok(s) => s,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                bail!(
                    "Downloader binary 'yt-dlp' not found. Please install yt-dlp and aria2c or update [download].downloader in config."
                );
            }
            return Err(anyhow!(err).context("failed to execute downloader 'yt-dlp' with aria2c"));
        }
    };

    if !status.success() {
        if output_path.exists() {
            let _ = fs::remove_file(output_path);
        }
        bail!("yt-dlp (with aria2c) process failed with status {status}");
    }

    Ok(())
}

pub async fn download_episode(
    stream: &StreamOption,
    show_title: &str,
    episode_label: &str,
    config: &AppConfig,
) -> Result<PathBuf> {
    let base_dir = config
        .download
        .dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));

    let show_folder = sanitize_filename(show_title);
    let target_dir = base_dir.join(show_folder);
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("Failed to create output directory {}", target_dir.display()))?;

    let filename = format_episode_filename(episode_label);
    let output_path = target_dir.join(&filename);

    if output_path.exists() {
        println!(
            "[{show_title}] Episode {episode_label} already downloaded at {}",
            output_path.display()
        );
        return Ok(output_path);
    }

    let engine = config.download.downloader;
    println!(
        "[{show_title}] Downloading Episode {episode_label} to {} (using {engine})...",
        output_path.display()
    );

    match engine {
        DownloaderEngine::Ffmpeg => download_with_ffmpeg(stream, &output_path).await?,
        DownloaderEngine::Ytdlp => download_with_ytdlp(stream, &output_path).await?,
        DownloaderEngine::YtdlpAria2c => download_with_ytdlp_aria2c(stream, &output_path).await?,
    }

    println!(
        "✓ [{show_title}] Episode {episode_label} downloaded successfully: {}",
        output_path.display()
    );

    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_episode_range_single() {
        let eps = vec!["1".into(), "2".into(), "3".into(), "4".into(), "5".into()];
        let res = parse_episode_range("4", &eps).unwrap();
        assert_eq!(res, vec!["4"]);
    }

    #[test]
    fn test_parse_episode_range_dash() {
        let eps = vec!["1".into(), "2".into(), "3".into(), "4".into(), "5".into()];
        let res = parse_episode_range("2-4", &eps).unwrap();
        assert_eq!(res, vec!["2", "3", "4"]);
    }

    #[test]
    fn test_parse_episode_range_dots() {
        let eps = vec!["1".into(), "2".into(), "3".into(), "4".into(), "5".into()];
        let res = parse_episode_range("1..3", &eps).unwrap();
        assert_eq!(res, vec!["1", "2", "3"]);
    }

    #[test]
    fn test_sanitize_filename_unicode() {
        assert_eq!(
            sanitize_filename("Re:Zero - Starting Life in Another World"),
            "Re Zero - Starting Life in Another World"
        );
        assert_eq!(
            sanitize_filename("SPY×FAMILY Season 2/Part:1?"),
            "SPY×FAMILY Season 2_Part 1_"
        );
        assert_eq!(
            sanitize_filename("「Kage no Jitsuryokusha ni Naritakute!」"),
            "「Kage no Jitsuryokusha ni Naritakute!」"
        );
        assert_eq!(sanitize_filename("CON"), "_CON");
    }

    #[test]
    fn test_downloader_engine_parse_and_display() {
        use std::str::FromStr;

        assert_eq!(DownloaderEngine::all().len(), 3);
        assert_eq!(DownloaderEngine::from_str("ffmpeg").unwrap(), DownloaderEngine::Ffmpeg);
        assert_eq!(DownloaderEngine::from_str("ytdlp").unwrap(), DownloaderEngine::Ytdlp);
        assert_eq!(DownloaderEngine::from_str("yt-dlp").unwrap(), DownloaderEngine::Ytdlp);
        assert_eq!(DownloaderEngine::from_str("ytdlp+aria2c").unwrap(), DownloaderEngine::YtdlpAria2c);
        assert_eq!(DownloaderEngine::from_str("yt-dlp+aria2c").unwrap(), DownloaderEngine::YtdlpAria2c);
        assert_eq!(DownloaderEngine::from_str("ytdlp-aria2c").unwrap(), DownloaderEngine::YtdlpAria2c);
        assert_eq!(DownloaderEngine::from_str("ytdlp_aria2c").unwrap(), DownloaderEngine::YtdlpAria2c);

        assert_eq!(DownloaderEngine::Ffmpeg.to_string(), "ffmpeg");
        assert_eq!(DownloaderEngine::Ytdlp.to_string(), "ytdlp");
        assert_eq!(DownloaderEngine::YtdlpAria2c.to_string(), "ytdlp+aria2c");

        assert!(DownloaderEngine::from_str("unknown_downloader").is_err());
    }
}
