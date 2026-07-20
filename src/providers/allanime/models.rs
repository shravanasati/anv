use serde::Deserialize;
use std::collections::HashMap;

/// Wrapper for the encrypted envelope: `{"data": {"_m": "...", "tobeparsed": "<base64>"}}`
#[derive(Debug, Deserialize)]
pub struct EncryptedEnvelope {
    pub data: EncryptedData,
}

#[derive(Debug, Deserialize)]
pub struct EncryptedData {
    pub tobeparsed: String,
}

#[derive(Debug, Deserialize)]
pub struct FilemoonResponse {
    pub iv: String,
    pub payload: String,
    #[serde(rename = "key_parts")]
    pub key_parts: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQlRawEnvelope {
    pub data: Option<serde_json::Value>,
    pub errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQlError {
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchPayload {
    pub shows: SearchShows,
}

#[derive(Debug, Deserialize)]
pub struct SearchShows {
    pub edges: Vec<SearchEdge>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SearchEdge {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(rename = "englishName")]
    pub english_name: Option<String>,
    #[serde(rename = "malId")]
    pub mal_id: Option<String>,
    #[serde(rename = "availableEpisodes")]
    #[serde(default)]
    pub available_episodes: AvailabilitySnapshot,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct AvailabilitySnapshot {
    #[serde(default)]
    pub sub: usize,
    #[serde(default)]
    pub dub: usize,
}

#[derive(Debug, Deserialize)]
pub struct SearchMangaPayload {
    pub mangas: SearchMangas,
}

#[derive(Debug, Deserialize)]
pub struct SearchMangas {
    pub edges: Vec<SearchMangaEdge>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SearchMangaEdge {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(rename = "availableChapters")]
    #[serde(default)]
    pub available_chapters: ChapterAvailabilitySnapshot,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ChapterAvailabilitySnapshot {
    #[serde(default)]
    pub sub: usize,
    #[serde(default)]
    pub raw: usize,
}

#[derive(Debug, Deserialize)]
pub struct MangaDetailPayload {
    pub manga: MangaDetail,
}

#[derive(Debug, Deserialize)]
pub struct MangaDetail {
    #[serde(rename = "availableChaptersDetail")]
    #[serde(default)]
    pub available_chapters_detail: ChapterDetail,
}

#[derive(Debug, Deserialize, Default)]
pub struct ChapterDetail {
    #[serde(default)]
    pub sub: Vec<String>,
    #[serde(default)]
    pub raw: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChapterPagesPayload {
    #[serde(rename = "chapterPages")]
    pub chapter_pages: ChapterPagesConnection,
}

#[derive(Debug, Deserialize)]
pub struct ChapterPagesConnection {
    pub edges: Vec<ChapterPageEdge>,
}

#[derive(Debug, Deserialize)]
pub struct ChapterPageEdge {
    #[serde(rename = "pictureUrlHead")]
    pub picture_url_head: String,
    #[serde(rename = "pictureUrls")]
    pub picture_urls: Vec<PictureUrl>,
}

#[derive(Debug, Deserialize)]
pub struct PictureUrl {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct ShowDetailPayload {
    pub show: ShowDetail,
}

#[derive(Debug, Deserialize)]
pub struct ShowDetail {
    #[serde(rename = "malId")]
    pub mal_id: Option<String>,
    #[serde(rename = "availableEpisodesDetail")]
    #[serde(default)]
    pub available_episodes_detail: EpisodeDetail,
}

#[derive(Debug, Deserialize, Default)]
pub struct EpisodeDetail {
    #[serde(default)]
    pub sub: Vec<String>,
    #[serde(default)]
    pub dub: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct EpisodePayload {
    pub episode: Option<EpisodeSources>,
}

#[derive(Debug, Deserialize)]
pub struct EpisodeSources {
    #[serde(rename = "sourceUrls")]
    pub source_urls: Vec<SourceDescriptor>,
}

#[derive(Debug, Deserialize)]
pub struct SourceDescriptor {
    #[serde(rename = "sourceUrl")]
    pub source_url: String,
    #[serde(rename = "sourceName")]
    pub source_name: String,
}

#[derive(Debug, Deserialize)]
pub struct ClockResponse {
    pub links: Vec<ClockLink>,
}

#[derive(Debug, Deserialize)]
pub struct ClockLink {
    pub link: String,
    #[serde(rename = "resolutionStr")]
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub hls: bool,
    #[serde(default)]
    pub subtitles: Vec<ClockSubtitle>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ClockSubtitle {
    pub src: String,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}
