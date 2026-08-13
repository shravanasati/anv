use anyhow::{Result, bail};
use dialoguer::Select;
use std::path::Path;

use crate::Cli;
use crate::cache::{MangaCacheState, cache_manga_pages};
use crate::cmd::media::{
    ConsumeOutcome, MediaContext, MediaEntry, MediaLoopConfig, run_media_loop,
};
use crate::cmd::search::aggregate_manga_search;
use crate::config::AppConfig;
use crate::history::{History, theme};
use crate::player::launch_image_viewer;
use crate::providers::MangaProvider;
use crate::types::{MangaInfo, Provider, Translation};
use crate::utils::search_single_with_timeout;

const INITIAL_MANGA_PAGE_PRELOAD: usize = 5;

pub async fn run_manga_flow(
    cli: &Cli,
    translation: Translation,
    history: &mut History,
    history_path: &Path,
    auto_play_next: bool,
    config: &AppConfig,
) -> Result<()> {
    if !cli.provider.is_manga() {
        bail!(
            "Provider '{}' does not support manga. Valid manga providers: all, mangadex, mangapill",
            cli.provider.display_name()
        );
    }

    if cli.query.is_empty() {
        println!("No query provided. Use `anv --manga <name>`.");
        return Ok(());
    }

    let query = cli.query.join(" ");
    let theme = theme();

    let timeout_secs = cli.timeout.unwrap_or(config.timeout);
    let search_timeout = std::time::Duration::from_secs(timeout_secs);

    match cli.provider {
        Provider::All => {
            let combined =
                aggregate_manga_search(&query, translation, search_timeout, timeout_secs).await?;

            if combined.is_empty() {
                bail!("No results for \"{}\" ({})", query, translation.label());
            }

            let selection = select_manga_with_provider(&combined, translation, &theme)?;
            let Some((selected_provider, manga)) = selection else {
                return Ok(());
            };

            let client = selected_provider.manga_client()?;
            read_manga(
                &client,
                translation,
                manga,
                history,
                history_path,
                cli.episode.clone(),
                auto_play_next,
                cli.cache_dir.as_deref(),
                selected_provider,
                config,
            )
            .await
        }
        _ => {
            let client = cli.provider.manga_client()?;
            let mangas = search_single_with_timeout(
                search_timeout,
                cli.provider.display_name(),
                timeout_secs,
                client.search_mangas(&query, translation),
            )
            .await?;
            if mangas.is_empty() {
                bail!(
                    "No results for \"{}\" ({}) on {}",
                    query,
                    translation.label(),
                    cli.provider.display_name()
                );
            }
            let manga = select_manga(&mangas, translation, &theme)?;
            let Some(manga) = manga else {
                return Ok(());
            };
            read_manga(
                &client,
                translation,
                manga,
                history,
                history_path,
                cli.episode.clone(),
                auto_play_next,
                cli.cache_dir.as_deref(),
                cli.provider,
                config,
            )
            .await
        }
    }
}

fn select_manga(
    mangas: &[MangaInfo],
    translation: Translation,
    theme: &dialoguer::theme::ColorfulTheme,
) -> Result<Option<MangaInfo>> {
    let options: Vec<String> = mangas
        .iter()
        .map(|m| {
            let count = m.chapter_count_for(translation);
            format!("{} [{} chapters]", m.title, count)
        })
        .collect();
    let selection = Select::with_theme(theme)
        .with_prompt("Select a manga (Esc to cancel)")
        .items(&options)
        .default(0)
        .interact_opt()?;
    Ok(selection.map(|idx| mangas[idx].clone()))
}

fn select_manga_with_provider(
    items: &[(Provider, MangaInfo)],
    translation: Translation,
    theme: &dialoguer::theme::ColorfulTheme,
) -> Result<Option<(Provider, MangaInfo)>> {
    let options: Vec<String> = items
        .iter()
        .map(|(provider, m)| {
            let count = m.chapter_count_for(translation);
            if count > 0 {
                format!(
                    "{} [{}] [{} chapters]",
                    m.title,
                    provider.display_name(),
                    count
                )
            } else {
                format!("{} [{}]", m.title, provider.display_name())
            }
        })
        .collect();
    let selection = Select::with_theme(theme)
        .with_prompt("Select a manga (Esc to cancel)")
        .items(&options)
        .default(0)
        .interact_opt()?;
    Ok(selection.map(|idx| items[idx].clone()))
}

pub async fn read_manga(
    client: &impl MangaProvider,
    translation: Translation,
    manga: MangaInfo,
    history: &mut History,
    history_path: &Path,
    prefer_chapter: Option<String>,
    auto_play_next: bool,
    cache_base_override: Option<&Path>,
    provider: Provider,
    config: &AppConfig,
) -> Result<()> {
    let chapters = match client.fetch_chapters(&manga.id, translation).await {
        Ok(c) => c,
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("connection closed")
                || msg.contains("SendRequest")
                || msg.contains("connect")
            {
                bail!(
                    "Could not connect to the provider \u{2014} your network may be blocking it.\nTry a different provider: --provider mangadex  or  --provider mangapill"
                );
            }
            return Err(err);
        }
    };
    if chapters.is_empty() {
        bail!(
            "No {} chapters available for {}",
            translation.label(),
            manga.title
        );
    }

    let items: Vec<MediaEntry> = chapters
        .iter()
        .map(|c| MediaEntry {
            id: manga.id.clone(),
            label: c.label.clone(),
        })
        .collect();

    let last_read = history.last_chapter(&manga.id, translation);

    let manga_id = manga.id.clone();
    let manga_title = manga.title.clone();
    let chapters_ref = &chapters;
    let cache_base = cache_base_override;
    let consume = move |entry: &MediaEntry, ctx: &MediaContext| {
        let label = entry.label.clone();
        let manga_id = manga_id.clone();
        let manga_title = manga_title.clone();
        let auto_advance = ctx.auto_advance;
        let next_candidate = ctx.next_candidate.map(str::to_string);
        async move {
            let chapter = chapters_ref
                .iter()
                .find(|c| c.label == label)
                .expect("chapter label came from the chapter list");
            let chapter_id = chapter.id.clone();

            let pages = match client
                .fetch_pages(&manga_id, translation, &chapter_id)
                .await
            {
                Ok(pages) => pages,
                Err(err) => {
                    eprintln!("Failed to fetch pages for chapter {}: {}", label, err);
                    return Ok(ConsumeOutcome::Retry);
                }
            };

            if pages.is_empty() {
                eprintln!("No pages found for chapter {}.", label);
                return Ok(ConsumeOutcome::Retry);
            }

            let cache_state = match cache_manga_pages(
                &pages,
                &manga_id,
                translation,
                &label,
                cache_base,
                INITIAL_MANGA_PAGE_PRELOAD,
            )
            .await
            {
                Ok(state) => {
                    let cached_count = state.cached_pages.iter().filter(|p| p.is_some()).count();
                    if cached_count > 0 {
                        println!("Caching chapter pages locally...");
                        println!(
                            "Cached {cached_count}/{} pages upfront for Chapter {} (first {} pages).",
                            pages.len(),
                            label,
                            INITIAL_MANGA_PAGE_PRELOAD
                        );
                        if pages.len() > INITIAL_MANGA_PAGE_PRELOAD {
                            println!("Continuing to cache remaining pages in background...");
                        }
                    }
                    state
                }
                Err(err) => {
                    eprintln!(
                        "Page cache unavailable for Chapter {} ({}). Falling back to streaming URLs.",
                        label, err
                    );
                    MangaCacheState {
                        cached_pages: vec![None; pages.len()],
                        cache_files: Vec::new(),
                        cdn_blocked: false,
                    }
                }
            };

            if cache_state.cdn_blocked {
                if auto_advance {
                    if let Some(next) = next_candidate {
                        return Ok(ConsumeOutcome::RetryWith(next));
                    }
                }
                return Ok(ConsumeOutcome::Retry);
            }

            launch_image_viewer(
                &pages,
                &cache_state.cached_pages,
                &cache_state.cache_files,
                &manga_title,
                &label,
                config,
            )
            .await?;

            Ok(ConsumeOutcome::Consumed)
        }
    };

    run_media_loop(
        &manga.title,
        items,
        prefer_chapter,
        last_read,
        auto_play_next,
        false,
        history,
        history_path,
        translation,
        provider,
        true,
        MediaLoopConfig {
            select_prompt: "Chapter to read (type to search, Esc to cancel)",
            use_fuzzy: true,
            unit_plural: "chapters",
            unit_singular: "chapter",
            last_verb: "read",
        },
        consume,
    )
    .await
}
