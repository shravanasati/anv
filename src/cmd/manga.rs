use anyhow::{Result, bail};
use chrono::Utc;
use dialoguer::{FuzzySelect, Select};
use std::path::Path;

use crate::Cli;
use crate::cache::{MangaCacheState, cache_manga_pages};
use crate::config::AppConfig;
use crate::history::{History, HistoryEntry, theme};
use crate::player::launch_image_viewer;
use crate::providers::{MangaProvider, mangadex::MangaDexClient, mangapill::MangapillClient};
use crate::types::{MangaInfo, Provider, Translation};
use crate::utils::{next_episode_label_presorted, sorted_episode_labels};

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

    match cli.provider {
        Provider::Mangadex => {
            let client = MangaDexClient::new()?;
            let mangas = client.search_mangas(&query, translation).await?;
            if mangas.is_empty() {
                bail!(
                    "No results for \"{}\" ({}) on MangaDex",
                    query,
                    translation.label()
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
                Provider::Mangadex,
                config,
            )
            .await
        }
        Provider::Mangapill => {
            let client = MangapillClient::new()?;
            let mangas = client.search_mangas(&query, translation).await?;
            if mangas.is_empty() {
                bail!(
                    "No results for \"{}\" ({}) on Mangapill",
                    query,
                    translation.label()
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
                Provider::Mangapill,
                config,
            )
            .await
        }
        Provider::All => {
            let mangadex = MangaDexClient::new().ok();
            let mangapill = MangapillClient::new().ok();

            println!("Searching across all manga providers for \"{}\"...", query);

            let (mangadex_mangas, mangapill_mangas) = tokio::join!(
                async {
                    if let Some(ref client) = mangadex {
                        client
                            .search_mangas(&query, translation)
                            .await
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                },
                async {
                    if let Some(ref client) = mangapill {
                        client
                            .search_mangas(&query, translation)
                            .await
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                },
            );

            let mut combined = Vec::new();
            for manga in mangadex_mangas {
                combined.push((Provider::Mangadex, manga));
            }
            for manga in mangapill_mangas {
                combined.push((Provider::Mangapill, manga));
            }

            if combined.is_empty() {
                bail!("No results for \"{}\" ({})", query, translation.label());
            }

            let selection = select_manga_with_provider(&combined, translation, &theme)?;
            let Some((selected_provider, manga)) = selection else {
                return Ok(());
            };

            match selected_provider {
                Provider::Mangadex => {
                    let client = mangadex.expect("MangaDex client must exist if selected");
                    read_manga(
                        &client,
                        translation,
                        manga,
                        history,
                        history_path,
                        cli.episode.clone(),
                        auto_play_next,
                        cli.cache_dir.as_deref(),
                        Provider::Mangadex,
                        config,
                    )
                    .await
                }
                Provider::Mangapill => {
                    let client = mangapill.expect("Mangapill client must exist if selected");
                    read_manga(
                        &client,
                        translation,
                        manga,
                        history,
                        history_path,
                        cli.episode.clone(),
                        auto_play_next,
                        cli.cache_dir.as_deref(),
                        Provider::Mangapill,
                        config,
                    )
                    .await
                }
                _ => unreachable!(),
            }
        }
        _ => bail!(
            "Provider '{}' does not support manga.",
            cli.provider.display_name()
        ),
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
            let count = match translation {
                Translation::Sub => m.available_chapters.sub,
                Translation::Raw => m.available_chapters.raw,
                Translation::Dub => 0,
            };
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
            let count = match translation {
                Translation::Sub => m.available_chapters.sub,
                Translation::Raw => m.available_chapters.raw,
                Translation::Dub => 0,
            };
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

    let chapter_labels: Vec<String> = chapters.iter().map(|c| c.label.clone()).collect();
    let sorted_labels = sorted_episode_labels(&chapter_labels);

    let latest_available = sorted_labels
        .last()
        .cloned()
        .expect("chapters is non-empty; bail!() above ensures this");
    println!(
        "Found {} {} chapters. Latest available: {}.",
        chapters.len(),
        translation.label(),
        latest_available
    );

    let last_read = history.last_chapter(&manga.id, translation);
    if let Some(prev) = &last_read {
        println!("Last read {} chapter: {}.", translation.label(), prev);
    }

    let fallback = last_read
        .clone()
        .unwrap_or_else(|| latest_available.clone());
    let (mut current_label, mut skip_selection) = match &prefer_chapter {
        Some(ch) if chapter_labels.contains(ch) => (ch.clone(), true),
        Some(ch) => {
            println!(
                "Chapter '{}' does not exist for '{}'. Showing chapter list.",
                ch, manga.title
            );
            (fallback, false)
        }
        None => {
            if auto_play_next {
                if let Some(last) = &last_read {
                    if let Some(next) = next_episode_label_presorted(last, &sorted_labels) {
                        (next, true)
                    } else {
                        (last.clone(), false)
                    }
                } else {
                    (sorted_labels.first().unwrap().clone(), true)
                }
            } else {
                (fallback, false)
            }
        }
    };

    let theme = theme();
    loop {
        let default_idx = chapter_labels
            .iter()
            .position(|ch| ch == &current_label)
            .or_else(|| chapter_labels.iter().position(|ch| ch == &latest_available))
            .unwrap_or(0);

        let idx = if skip_selection {
            skip_selection = false;
            default_idx
        } else {
            let selection = FuzzySelect::with_theme(&theme)
                .with_prompt("Chapter to read (type to search, Esc to cancel)")
                .items(&chapter_labels)
                .default(default_idx)
                .interact_opt()?;
            let Some(i) = selection else {
                println!("Exiting reading loop.");
                return Ok(());
            };
            i
        };

        let chosen_label = chapter_labels[idx].clone();
        let chapter_id = chapters[idx].id.clone();
        let auto_advance = idx == default_idx;

        let pages = match client
            .fetch_pages(&manga.id, translation, &chapter_id)
            .await
        {
            Ok(pages) => pages,
            Err(err) => {
                eprintln!(
                    "Failed to fetch pages for chapter {}: {}",
                    chosen_label, err
                );
                continue;
            }
        };

        if pages.is_empty() {
            eprintln!("No pages found for chapter {}.", chosen_label);
            continue;
        }

        let next_candidate = next_episode_label_presorted(&chosen_label, &sorted_labels);

        let cache_state = match cache_manga_pages(
            &pages,
            &manga.id,
            translation,
            &chosen_label,
            cache_base_override,
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
                        chosen_label,
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
                    chosen_label, err
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
                    current_label = next;
                }
            }
            continue;
        }

        launch_image_viewer(
            &pages,
            &cache_state.cached_pages,
            &cache_state.cache_files,
            &manga.title,
            &chosen_label,
            config,
        )
        .await?;

        history.upsert(HistoryEntry {
            show_id: manga.id.clone(),
            show_title: manga.title.clone(),
            episode: chosen_label.clone(),
            translation,
            provider,
            is_manga: true,
            watched_at: Utc::now(),
        });
        history.save(history_path)?;

        match (auto_advance, next_candidate) {
            (true, Some(next)) => current_label = next,
            (true, None) => {
                println!("No further chapters found. Exiting.");
                return Ok(());
            }
            (false, candidate) => current_label = candidate.unwrap_or(chosen_label),
        }
    }
}
