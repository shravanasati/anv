use anyhow::{Result, bail};
use std::collections::HashMap;

use super::{ALLANIME_IMAGE_REFERER, AllAnimeClient, models::*, queries::*};
use crate::providers::MangaProvider;
use crate::types::{Chapter, ChapterCounts, MangaInfo, Page, Translation};

impl AllAnimeClient {
    pub(super) async fn fetch_manga_detail(&self, manga_id: &str) -> Result<MangaDetail> {
        let body = serde_json::json!({
            "query": MANGA_DETAIL_QUERY,
            "variables": { "mangaId": manga_id }
        });
        let payload: MangaDetailPayload = self.post_graphql(&body).await?;
        Ok(payload.manga)
    }
}

impl MangaProvider for AllAnimeClient {
    async fn search_mangas(&self, query: &str, translation: Translation) -> Result<Vec<MangaInfo>> {
        let body = serde_json::json!({
            "query": SEARCH_MANGAS_QUERY,
            "variables": {
                "search": {
                    "allowAdult": false,
                    "allowUnknown": false,
                    "query": query,
                },
                "limit": 25,
                "page": 1,
                "translationType": translation.as_str(),
                "countryOrigin": "ALL"
            }
        });
        let payload: SearchMangaPayload = self.post_graphql(&body).await?;
        Ok(payload
            .mangas
            .edges
            .into_iter()
            .map(|edge| MangaInfo {
                id: edge.id,
                title: edge.name,
                available_chapters: ChapterCounts {
                    sub: edge.available_chapters.sub,
                    raw: edge.available_chapters.raw,
                },
            })
            .collect())
    }

    async fn fetch_chapters(
        &self,
        manga_id: &str,
        translation: Translation,
    ) -> Result<Vec<Chapter>> {
        let detail = self.fetch_manga_detail(manga_id).await?;
        let raw_chapters = match translation {
            Translation::Sub => detail.available_chapters_detail.sub,
            Translation::Raw => detail.available_chapters_detail.raw,
            Translation::Dub => bail!("Dub translation is not supported for manga"),
        };
        Ok(raw_chapters
            .into_iter()
            .map(|ch| Chapter {
                id: ch.clone(),
                label: ch,
            })
            .collect())
    }

    async fn fetch_pages(
        &self,
        manga_id: &str,
        translation: Translation,
        chapter_id: &str,
    ) -> Result<Vec<Page>> {
        let body = serde_json::json!({
            "query": CHAPTER_PAGES_QUERY,
            "variables": {
                "mangaId": manga_id,
                "translationType": translation.as_str(),
                "chapterString": chapter_id
            }
        });
        let payload: ChapterPagesPayload = self.post_graphql(&body).await?;
        Ok(if let Some(edge) = payload.chapter_pages.edges.first() {
            let head = &edge.picture_url_head;
            edge.picture_urls
                .iter()
                .map(|p| {
                    let url = if p.url.starts_with("http") {
                        p.url.clone()
                    } else {
                        format!("{}{}", head, p.url)
                    };
                    let mut headers = HashMap::new();
                    headers.insert("Referer".to_string(), ALLANIME_IMAGE_REFERER.to_string());
                    headers.insert("Origin".to_string(), ALLANIME_IMAGE_REFERER.to_string());
                    Page { url, headers }
                })
                .collect()
        } else {
            Vec::new()
        })
    }
}
