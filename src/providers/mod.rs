use crate::types::{Provider, ShowInfo, StreamOption, Translation};
use anyhow::{Result, bail};

pub mod anidb;
pub mod animehub;
pub mod anineko;
pub mod senshi;

pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0 Safari/537.36";

pub const AUTO_QUALITY_LABEL: &str = "auto";
pub const AUTO_QUALITY_RANK: i32 = 0;

pub fn parse_quality_rank(label: &str) -> i32 {
    let clean = label.trim().to_lowercase();
    if clean.starts_with("1080") {
        1080
    } else if clean.starts_with("720") {
        720
    } else if clean.starts_with("480") {
        480
    } else if clean.starts_with("360") {
        360
    } else if clean.starts_with("240") {
        240
    } else {
        AUTO_QUALITY_RANK
    }
}


pub trait AnimeProvider {
    async fn search_shows(&self, query: &str, translation: Translation) -> Result<Vec<ShowInfo>>;
    async fn fetch_episodes(&self, show_id: &str, translation: Translation) -> Result<Vec<String>>;
    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>>;
    async fn fetch_mal_id(&self, _show_id: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

macro_rules! delegate_anime {
    ($self:expr, $fn:ident ($($arg:expr),* $(,)?)) => {
        match $self {
            Self::Anidb(c) => c.$fn($($arg),*).await,
            Self::Animehub(c) => c.$fn($($arg),*).await,
            Self::Anineko(c) => c.$fn($($arg),*).await,
            Self::Senshi(c) => c.$fn($($arg),*).await,
        }
    };
}

#[derive(Debug, Clone)]
pub enum AnyAnimeClient {
    Anidb(anidb::AnidbClient),
    Animehub(animehub::AnimehubClient),
    Anineko(anineko::AninekoClient),
    Senshi(senshi::SenshiClient),
}

impl AnimeProvider for AnyAnimeClient {
    async fn search_shows(&self, query: &str, translation: Translation) -> Result<Vec<ShowInfo>> {
        delegate_anime!(self, search_shows(query, translation))
    }

    async fn fetch_episodes(&self, show_id: &str, translation: Translation) -> Result<Vec<String>> {
        delegate_anime!(self, fetch_episodes(show_id, translation))
    }

    async fn fetch_streams(
        &self,
        show_id: &str,
        translation: Translation,
        episode: &str,
    ) -> Result<Vec<StreamOption>> {
        delegate_anime!(self, fetch_streams(show_id, translation, episode))
    }

    async fn fetch_mal_id(&self, show_id: &str) -> Result<Option<String>> {
        delegate_anime!(self, fetch_mal_id(show_id))
    }
}

impl Provider {
    pub fn anime_client(&self) -> Result<AnyAnimeClient> {
        match self {
            Provider::Anidb => Ok(AnyAnimeClient::Anidb(anidb::AnidbClient::new()?)),
            Provider::Animehub => Ok(AnyAnimeClient::Animehub(animehub::AnimehubClient::new()?)),
            Provider::Anineko => Ok(AnyAnimeClient::Anineko(anineko::AninekoClient::new()?)),
            Provider::Senshi => Ok(AnyAnimeClient::Senshi(senshi::SenshiClient::new()?)),
            _ => bail!(
                "Provider '{}' does not support anime streaming.",
                self.display_name()
            ),
        }
    }

    pub fn all_anime() -> &'static [Provider] {
        &[
            Provider::Anidb,
            Provider::Animehub,
            Provider::Anineko,
            Provider::Senshi,
        ]
    }

    pub fn valid_anime_providers() -> String {
        let mut names = vec!["all"];
        names.extend(Provider::all_anime().iter().map(|p| p.cli_name()));
        names.join(", ")
    }
}
