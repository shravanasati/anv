use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Translation {
    Sub,
    Dub,
    Raw,
}

impl Translation {
    pub fn as_str(self) -> &'static str {
        match self {
            Translation::Sub => "sub",
            Translation::Dub => "dub",
            Translation::Raw => "raw",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Translation::Sub => "Sub",
            Translation::Dub => "Dub",
            Translation::Raw => "Raw",
        }
    }
}

impl fmt::Display for Translation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone)]
pub struct ShowInfo {
    pub id: String,
    pub title: String,
    pub mal_id: Option<String>,
    pub available_eps: EpisodeCounts,
}

impl ShowInfo {
    pub fn episode_count_for(&self, translation: Translation) -> usize {
        match translation {
            Translation::Sub => self.available_eps.sub,
            Translation::Dub => self.available_eps.dub,
            Translation::Raw => 0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EpisodeCounts {
    pub sub: usize,
    pub dub: usize,
}

#[derive(Debug, Clone)]
pub struct MangaInfo {
    pub id: String,
    pub title: String,
    pub available_chapters: ChapterCounts,
}

impl MangaInfo {
    pub fn chapter_count_for(&self, translation: Translation) -> usize {
        match translation {
            Translation::Sub => self.available_chapters.sub,
            Translation::Raw => self.available_chapters.raw,
            Translation::Dub => 0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChapterCounts {
    pub sub: usize,
    pub raw: usize,
}

/// A manga chapter with a human-readable display label (e.g. `"271.5"`) and a
/// provider-specific identifier used to fetch pages (may differ from the label,
/// e.g. a UUID on MangaDex or a URL slug on Mangapill).
#[derive(Debug, Clone)]
pub struct Chapter {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct StreamOption {
    pub provider: String,
    pub url: String,
    pub quality_label: String,
    pub quality_rank: i32,
    pub is_hls: bool,
    pub headers: HashMap<String, String>,
    pub subtitle: Option<String>,
}

impl StreamOption {
    pub fn label(&self) -> String {
        let kind = if self.is_hls { "HLS" } else { "MP4" };
        format!("{} {} ({})", self.provider, self.quality_label, kind)
    }
}

#[derive(Debug, Clone)]
pub struct Page {
    pub url: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    All,
    #[serde(alias = "allanime")]
    Anidb,
    Anineko,
    #[serde(alias = "123animehub")]
    Animehub,
    Mangadex,
    Mangapill,
    Senshi,
    #[value(skip)]
    #[serde(other)]
    Unknown,
}

impl Provider {
    pub fn is_anime(self) -> bool {
        matches!(
            self,
            Provider::All
                | Provider::Anidb
                | Provider::Anineko
                | Provider::Animehub
                | Provider::Senshi
        )
    }

    pub fn is_manga(self) -> bool {
        matches!(
            self,
            Provider::All | Provider::Mangadex | Provider::Mangapill
        )
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Provider::All => "All Providers",
            Provider::Anidb => "AniDB",
            Provider::Anineko => "AniNeko",
            Provider::Animehub => "AnimeHub",
            Provider::Mangadex => "MangaDex",
            Provider::Mangapill => "Mangapill",
            Provider::Senshi => "Senshi",
            Provider::Unknown => "Unknown",
        }
    }

    pub fn cli_name(self) -> &'static str {
        match self {
            Provider::All => "all",
            Provider::Anidb => "anidb",
            Provider::Anineko => "anineko",
            Provider::Animehub => "animehub",
            Provider::Mangadex => "mangadex",
            Provider::Mangapill => "mangapill",
            Provider::Senshi => "senshi",
            Provider::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allanime_deserialization_alias() {
        let p: Provider = serde_json::from_str("\"allanime\"").unwrap();
        assert_eq!(p, Provider::Anidb);
    }

    #[test]
    fn test_unknown_provider_deserialization_fallback() {
        let p: Provider = serde_json::from_str("\"future_provider\"").unwrap();
        assert_eq!(p, Provider::Unknown);
    }
}
