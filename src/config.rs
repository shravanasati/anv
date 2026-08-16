use anyhow::{Context, Result, anyhow};
use config::{Config, Environment, File, FileFormat};
use dirs_next::config_dir;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use toml::Value;

use crate::downloader::DownloaderEngine;
use crate::types::Provider;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default = "default_player")]
    pub player: String,

    #[serde(default)]
    pub binge: bool,

    #[serde(default)]
    pub auto_play_next: bool,

    /// Timeout in seconds for provider search requests (default: 15).
    #[serde(default = "default_timeout")]
    pub timeout: u64,

    /// When true, prefer English titles over the original title when displaying
    /// anime in search results, watchlists, and watching menus.
    #[serde(default)]
    pub prefer_english_titles: bool,

    #[serde(default)]
    pub preferred_provider: Provider,

    #[serde(default)]
    pub quality: Quality,

    #[serde(default)]
    pub mal: MalConfig,

    #[serde(default)]
    pub sync: SyncConfig,

    #[serde(default)]
    pub aniskip: AniskipConfig,

    #[serde(default)]
    pub download: DownloadConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AniskipConfig {
    #[serde(default = "default_timeout")]
    pub timeout: u64,

    #[serde(default = "default_true")]
    pub skip_op: bool,

    #[serde(default = "default_true")]
    pub skip_ed: bool,

    #[serde(default = "default_false")]
    pub skip_mixed_op: bool,

    #[serde(default = "default_false")]
    pub skip_mixed_ed: bool,

    #[serde(default = "default_false")]
    pub skip_recap: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MalConfig {
    /// MAL API client ID from https://myanimelist.net/apiconfig
    #[serde(default)]
    pub client_id: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// How to pick a quality variant when a provider returns multiple stream options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// Always pick the highest available resolution.
    Highest,
    /// Always pick the lowest available resolution.
    Lowest,
    /// Prompt the user to choose (default).
    #[default]
    Select,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DownloadConfig {
    /// Target directory for saving downloaded episodes (defaults to "." if not specified).
    #[serde(default)]
    pub dir: Option<PathBuf>,

    /// Executable binary/engine for downloading streams (default: DownloaderEngine::Ffmpeg).
    #[serde(default)]
    pub downloader: DownloaderEngine,
}

fn default_player() -> String {
    "mpv".to_string()
}

fn default_timeout() -> u64 {
    15
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

const CONFIG_HEADER: &str = "# anv configuration
# Docs: https://github.com/shravanasati/anv
#
# player -- media player command (default: \"mpv\")
#           also overridable with ANV_PLAYER env var
#
# binge          -- set to true to auto-play the next episode without prompting
#                  (can also be enabled per-session with the --binge flag)
#
# auto_play_next -- set to true to automatically resume from the next episode
#                  instead of the last watched episode in history/watchlist
#
# timeout        -- timeout in seconds for provider search requests (default: 15)
#                  can also be overridden per-command with the -T / --timeout flag
#
# prefer_english_titles -- set to true to prefer English titles over the original
#                         title in search results, watchlists, and watching menus
#                         (default: false)
#
# preferred_provider    -- content provider to use by default when -p / --provider flag
#                         is omitted (default: \"all\")
#                         possible values: all, anidb, anineko, animehub, senshi
#
# [mal]
#   client_id -- your MAL API client ID
#               register at https://myanimelist.net/apiconfig
#               redirect URI must be: http://localhost:11422/callback
#
# [sync]
#   enabled -- set to true to sync watch status to MAL after each episode
#
# [aniskip]
#   timeout       -- timeout in seconds for aniskip HTTP requests (default: 15)
#   skip_op       -- skip opening (default: true)
#   skip_ed       -- skip ending (default: true)
#   skip_mixed_op -- skip mixed opening (default: false)
#   skip_mixed_ed -- skip mixed ending (default: false)
#   skip_recap    -- skip recap (default: false)
#
# quality          -- stream quality selection strategy across all providers
#                     \"select\"  -- prompt to choose from available resolutions (default)
#                     \"highest\" -- always pick the highest available resolution
#                     \"lowest\"  -- always pick the lowest available resolution
#
# [download]
#   dir        -- download output directory (default: current directory \".\")
#   downloader -- downloader engine: \"ffmpeg\", \"ytdlp\" (or \"yt-dlp\"), \"ytdlp+aria2c\" (default: \"ffmpeg\")

";

impl Default for AniskipConfig {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            skip_op: true,
            skip_ed: true,
            skip_mixed_op: false,
            skip_mixed_ed: false,
            skip_recap: false,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            player: default_player(),
            binge: false,
            auto_play_next: false,
            timeout: default_timeout(),
            prefer_english_titles: false,
            preferred_provider: Provider::default(),
            quality: Quality::default(),
            mal: MalConfig::default(),
            sync: SyncConfig::default(),
            aniskip: AniskipConfig::default(),
            download: DownloadConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn config_path() -> Result<PathBuf> {
        let base = config_dir().ok_or_else(|| anyhow!("Could not determine config directory"))?;
        Ok(base.join("anv").join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;

        if !path.exists() {
            Self::write_defaults(&path)?;
        } else {
            Self::backfill_missing_fields(&path)?;
        }

        let cfg = Config::builder()
            .add_source(File::new(
                path.to_str()
                    .ok_or_else(|| anyhow!("Config path is not valid UTF-8"))?,
                FileFormat::Toml,
            ))
            .add_source(
                Environment::with_prefix("ANV")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()
            .context("failed to build config")?;

        cfg.try_deserialize::<AppConfig>()
            .context("failed to deserialize config")
    }

    /// Reads the on-disk config and inserts any keys that are present in the
    /// compiled-in defaults but absent in the file.  Existing values are never
    /// touched.  If the file was changed, a notice is printed so the user knows
    /// new options have been added to their config.
    fn backfill_missing_fields(path: &PathBuf) -> Result<()> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;

        let mut on_disk: toml::Table = raw
            .parse::<toml::Table>()
            .with_context(|| format!("failed to parse config file {}", path.display()))?;

        let default_value =
            toml::Value::try_from(AppConfig::default()).context("failed to serialize defaults")?;
        let default_table = default_value
            .as_table()
            .expect("AppConfig must serialize as a TOML table");

        let changed = merge_missing(&mut on_disk, default_table);

        if changed {
            let new_content =
                toml::to_string_pretty(&on_disk).context("failed to serialize updated config")?;
            fs::write(path, format!("{CONFIG_HEADER}{new_content}"))
                .with_context(|| format!("failed to write updated config to {}", path.display()))?;
            println!(
                "Note: new config options were added to your config at {} with their default values.",
                path.display()
            );
        }

        Ok(())
    }

    fn write_defaults(path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create config dir {}", parent.display()))?;
        }
        let default_cfg = AppConfig::default();
        let toml_str =
            toml::to_string_pretty(&default_cfg).context("failed to serialize default config")?;
        fs::write(path, format!("{CONFIG_HEADER}{toml_str}"))
            .with_context(|| format!("failed to write default config to {}", path.display()))?;
        println!("Created default config at {}", path.display());
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create config dir {}", parent.display()))?;
        }
        let toml_str = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(&path, format!("{CONFIG_HEADER}{toml_str}"))
            .with_context(|| format!("failed to write config to {}", path.display()))?;
        Ok(())
    }
}

/// Recursively inserts keys from `defaults` that are missing in `target`.
/// Returns `true` if any key was inserted.
fn merge_missing(target: &mut toml::Table, defaults: &toml::Table) -> bool {
    let mut changed = false;
    for (key, default_val) in defaults {
        match target.get_mut(key) {
            None => {
                target.insert(key.clone(), default_val.clone());
                changed = true;
            }
            Some(Value::Table(existing_table)) => {
                if let Value::Table(default_subtable) = default_val {
                    if merge_missing(existing_table, default_subtable) {
                        changed = true;
                    }
                }
            }
            Some(_) => {} // key already present, leave it alone
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backwards_compatibility_with_unknown_fields() {
        let legacy_toml = r#"
player = "mpv"
binge = true
max_results = 10

[aniskip]
skip_op = true
"#;
        let config: AppConfig = toml::from_str(legacy_toml)
            .expect("Should deserialize despite unknown max_results field");
        assert_eq!(config.player, "mpv");
        assert!(config.binge);
        assert_eq!(config.download.downloader, DownloaderEngine::Ffmpeg);
    }

    #[test]
    fn test_download_config_deserialization() {
        let toml_str = r#"
[download]
downloader = "ytdlp+aria2c"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.download.downloader, DownloaderEngine::YtdlpAria2c);
    }

    #[test]
    fn test_timeout_config_deserialization() {
        let default_config: AppConfig = toml::from_str("").unwrap();
        assert_eq!(default_config.timeout, 15);

        let custom_toml = r#"
timeout = 30
"#;
        let custom_config: AppConfig = toml::from_str(custom_toml).unwrap();
        assert_eq!(custom_config.timeout, 30);
    }

    #[test]
    fn test_aniskip_timeout_config_deserialization() {
        let default_config: AppConfig = toml::from_str("").unwrap();
        assert_eq!(default_config.aniskip.timeout, 15);

        let custom_toml = r#"
[aniskip]
timeout = 5
"#;
        let custom_config: AppConfig = toml::from_str(custom_toml).unwrap();
        assert_eq!(custom_config.aniskip.timeout, 5);
    }

    #[test]
    fn test_preferred_provider_config_deserialization() {
        let default_config: AppConfig = toml::from_str("").unwrap();
        assert_eq!(default_config.preferred_provider, Provider::All);

        let custom_toml = r#"
preferred_provider = "senshi"
"#;
        let custom_config: AppConfig = toml::from_str(custom_toml).unwrap();
        assert_eq!(custom_config.preferred_provider, Provider::Senshi);
    }
}
