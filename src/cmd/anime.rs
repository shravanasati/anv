use anyhow::{Result, bail};
use chrono::Utc;
use dialoguer::Select;
use reqwest::StatusCode;
use std::path::Path;

use crate::Cli;
use crate::aniskip::SkipOptions;
use crate::cmd::manga::read_manga;
use crate::config::AppConfig;
use crate::history::{History, HistoryEntry, theme};
use crate::player::{choose_stream, launch_player, select_stream_by_quality};
use crate::providers::{
    AnimeProvider, MangaProvider, anidb::AnidbClient, anineko::AninekoClient,
    mangadex::MangaDexClient, mangapill::MangapillClient, senshi::SenshiClient,
};
use crate::sync::SyncProvider;
use crate::types::{ChapterCounts, EpisodeCounts, MangaInfo, Provider, ShowInfo, Translation};
use crate::utils::{next_episode_label_presorted, sorted_episode_labels};

pub async fn run_anime_flow<P: SyncProvider>(
    cli: &Cli,
    config: &AppConfig,
    translation: Translation,
    history_mode: bool,
    history: &mut History,
    history_path: &Path,
    sync_provider: Option<&P>,
    binge: bool,
    auto_play_next: bool,
    provider: Provider,
    download_range: Option<String>,
) -> Result<()> {
    let skip_opts = SkipOptions {
        skip_op: cli.skip_op,
        skip_ed: cli.skip_ed,
        skip_mixed_op: cli.skip_mixed_op,
        skip_mixed_ed: cli.skip_mixed_ed,
        skip_recap: cli.skip_recap,
    };

    if history_mode {
        if let Some(entry) = history.select_entry()? {
            let target_provider = if provider != Provider::All {
                provider
            } else {
                entry.provider
            };

            if entry.is_manga {
                if download_range.is_some() {
                    bail!("The --download / -D flag is currently only supported for anime streaming.");
                }
                match target_provider {
                    Provider::Anidb => {
                        bail!("Provider 'AniDB' does not support manga.");
                    }
                    Provider::Anineko => {
                        bail!("Provider 'AniNeko' does not support manga.");
                    }
                    Provider::Senshi => {
                        bail!("Provider 'Senshi' does not support manga.");
                    }
                    Provider::All | Provider::Mangadex => {
                        let client = MangaDexClient::new()?;
                        let manga_info = if target_provider == entry.provider
                            || entry.provider == Provider::All
                        {
                            MangaInfo {
                                id: entry.show_id.clone(),
                                title: entry.show_title.clone(),
                                available_chapters: ChapterCounts::default(),
                            }
                        } else {
                            resolve_manga_info(&client, &entry.show_title, entry.translation)
                                .await?
                        };
                        read_manga(
                            &client,
                            entry.translation,
                            manga_info,
                            history,
                            history_path,
                            if auto_play_next {
                                None
                            } else {
                                Some(entry.episode.clone())
                            },
                            auto_play_next,
                            cli.cache_dir.as_deref(),
                            Provider::Mangadex,
                            config,
                        )
                        .await?;
                    }
                    Provider::Mangapill => {
                        let client = MangapillClient::new()?;
                        let manga_info = if target_provider == entry.provider {
                            MangaInfo {
                                id: entry.show_id.clone(),
                                title: entry.show_title.clone(),
                                available_chapters: ChapterCounts::default(),
                            }
                        } else {
                            resolve_manga_info(&client, &entry.show_title, entry.translation)
                                .await?
                        };
                        read_manga(
                            &client,
                            entry.translation,
                            manga_info,
                            history,
                            history_path,
                            if auto_play_next {
                                None
                            } else {
                                Some(entry.episode.clone())
                            },
                            auto_play_next,
                            cli.cache_dir.as_deref(),
                            Provider::Mangapill,
                            config,
                        )
                        .await?;
                    }
                }
            } else {
                match target_provider {
                    Provider::Anineko => {
                        let client = AninekoClient::new()?;
                        let show_info = if target_provider == entry.provider {
                            ShowInfo {
                                id: entry.show_id.clone(),
                                title: entry.show_title.clone(),
                                mal_id: None,
                                available_eps: EpisodeCounts::default(),
                            }
                        } else {
                            resolve_show_info(&client, &entry.show_title, entry.translation).await?
                        };
                        play_show(
                            &client,
                            history,
                            history_path,
                            entry.translation,
                            Provider::Anineko,
                            show_info,
                            if auto_play_next {
                                None
                            } else {
                                Some(entry.episode.clone())
                            },
                            if auto_play_next {
                                Some(entry.episode.clone())
                            } else {
                                None
                            },
                            auto_play_next,
                            sync_provider,
                            binge,
                            config,
                            skip_opts,
                            download_range.clone(),
                        )
                        .await?;
                    }
                    Provider::Senshi => {
                        let client = SenshiClient::new()?;
                        let show_info = if target_provider == entry.provider {
                            ShowInfo {
                                id: entry.show_id.clone(),
                                title: entry.show_title.clone(),
                                mal_id: Some(entry.show_id.clone()),
                                available_eps: EpisodeCounts::default(),
                            }
                        } else {
                            resolve_show_info(&client, &entry.show_title, entry.translation).await?
                        };
                        play_show(
                            &client,
                            history,
                            history_path,
                            entry.translation,
                            Provider::Senshi,
                            show_info,
                            if auto_play_next {
                                None
                            } else {
                                Some(entry.episode.clone())
                            },
                            if auto_play_next {
                                Some(entry.episode.clone())
                            } else {
                                None
                            },
                            auto_play_next,
                            sync_provider,
                            binge,
                            config,
                            skip_opts,
                            download_range.clone(),
                        )
                        .await?;
                    }
                    Provider::Anidb | Provider::All => {
                        let client = AnidbClient::new()?;
                        let show_info = if target_provider == entry.provider
                            || entry.provider == Provider::All
                        {
                            ShowInfo {
                                id: entry.show_id.clone(),
                                title: entry.show_title.clone(),
                                mal_id: None,
                                available_eps: EpisodeCounts::default(),
                            }
                        } else {
                            resolve_show_info(&client, &entry.show_title, entry.translation).await?
                        };
                        play_show(
                            &client,
                            history,
                            history_path,
                            entry.translation,
                            Provider::Anidb,
                            show_info,
                            if auto_play_next {
                                None
                            } else {
                                Some(entry.episode.clone())
                            },
                            if auto_play_next {
                                Some(entry.episode.clone())
                            } else {
                                None
                            },
                            auto_play_next,
                            sync_provider,
                            binge,
                            config,
                            skip_opts,
                            download_range.clone(),
                        )
                        .await?;
                    }
                    _ => {
                        bail!(
                            "Provider '{}' does not support anime streaming.",
                            target_provider.display_name()
                        );
                    }
                }
            }
        }
        return Ok(());
    }

    if cli.query.is_empty() {
        println!("No query provided. Use `anv <name>` to search, or `anv history` to resume.");
        return Ok(());
    }

    let query = cli.query.join(" ");
    let theme = theme();

    match provider {
        Provider::Senshi => {
            let client = SenshiClient::new()?;
            let shows = client.search_shows(&query, translation).await?;
            if shows.is_empty() {
                bail!(
                    "No results for \"{}\" ({}) on Senshi",
                    query,
                    translation.label()
                );
            }
            let show = select_show(&shows, translation, &theme)?;
            let Some(show) = show else {
                return Ok(());
            };
            play_show(
                &client,
                history,
                history_path,
                translation,
                Provider::Senshi,
                show,
                cli.episode.clone(),
                None,
                auto_play_next,
                sync_provider,
                binge,
                config,
                skip_opts,
                download_range.clone(),
            )
            .await
        }
        Provider::Anineko => {
            let client = AninekoClient::new()?;
            let shows = client.search_shows(&query, translation).await?;
            if shows.is_empty() {
                bail!(
                    "No results for \"{}\" ({}) on AniNeko",
                    query,
                    translation.label()
                );
            }
            let show = select_show(&shows, translation, &theme)?;
            let Some(show) = show else {
                return Ok(());
            };
            play_show(
                &client,
                history,
                history_path,
                translation,
                Provider::Anineko,
                show,
                cli.episode.clone(),
                None,
                auto_play_next,
                sync_provider,
                binge,
                config,
                skip_opts,
                download_range.clone(),
            )
            .await
        }
        Provider::Anidb => {
            let client = AnidbClient::new()?;
            let shows = client.search_shows(&query, translation).await?;
            if shows.is_empty() {
                bail!(
                    "No results for \"{}\" ({}) on AniDB",
                    query,
                    translation.label()
                );
            }
            let show = select_show(&shows, translation, &theme)?;
            let Some(show) = show else {
                return Ok(());
            };
            play_show(
                &client,
                history,
                history_path,
                translation,
                Provider::Anidb,
                show,
                cli.episode.clone(),
                None,
                auto_play_next,
                sync_provider,
                binge,
                config,
                skip_opts,
                download_range.clone(),
            )
            .await
        }
        Provider::All => {
            let anidb_client = AnidbClient::new().ok();
            let anineko_client = AninekoClient::new().ok();
            let senshi_client = SenshiClient::new().ok();

            println!("Searching across all anime providers for \"{}\"...", query);

            let (anidb_shows, anineko_shows, senshi_shows) = tokio::join!(
                async {
                    if let Some(ref client) = anidb_client {
                        client
                            .search_shows(&query, translation)
                            .await
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                },
                async {
                    if let Some(ref client) = anineko_client {
                        client
                            .search_shows(&query, translation)
                            .await
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                },
                async {
                    if let Some(ref client) = senshi_client {
                        client
                            .search_shows(&query, translation)
                            .await
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                },
            );

            let mut combined = Vec::new();
            for show in anidb_shows {
                combined.push((Provider::Anidb, show));
            }
            for show in anineko_shows {
                combined.push((Provider::Anineko, show));
            }
            for show in senshi_shows {
                combined.push((Provider::Senshi, show));
            }

            if combined.is_empty() {
                bail!("No results for \"{}\" ({})", query, translation.label());
            }

            let selection = select_show_with_provider(&combined, translation, &theme)?;
            let Some((selected_provider, show)) = selection else {
                return Ok(());
            };

            match selected_provider {
                Provider::Anidb => {
                    let client =
                        anidb_client.expect("AniDB client must be present if item was selected");
                    play_show(
                        &client,
                        history,
                        history_path,
                        translation,
                        Provider::Anidb,
                        show,
                        cli.episode.clone(),
                        None,
                        auto_play_next,
                        sync_provider,
                        binge,
                        config,
                        skip_opts,
                        download_range.clone(),
                    )
                    .await
                }
                Provider::Anineko => {
                    let client = anineko_client
                        .expect("AniNeko client must be present if item was selected");
                    play_show(
                        &client,
                        history,
                        history_path,
                        translation,
                        Provider::Anineko,
                        show,
                        cli.episode.clone(),
                        None,
                        auto_play_next,
                        sync_provider,
                        binge,
                        config,
                        skip_opts,
                        download_range.clone(),
                    )
                    .await
                }
                Provider::Senshi => {
                    let client =
                        senshi_client.expect("Senshi client must be present if item was selected");
                    play_show(
                        &client,
                        history,
                        history_path,
                        translation,
                        Provider::Senshi,
                        show,
                        cli.episode.clone(),
                        None,
                        auto_play_next,
                        sync_provider,
                        binge,
                        config,
                        skip_opts,
                        download_range.clone(),
                    )
                    .await
                }
                _ => unreachable!(),
            }
        }
        _ => {
            bail!(
                "Provider '{}' does not support anime streaming.",
                cli.provider.display_name()
            );
        }
    }
}

fn select_show(
    shows: &[ShowInfo],
    translation: Translation,
    theme: &dialoguer::theme::ColorfulTheme,
) -> Result<Option<ShowInfo>> {
    let options: Vec<String> = shows
        .iter()
        .map(|s| {
            let count = match translation {
                Translation::Sub => s.available_eps.sub,
                Translation::Dub => s.available_eps.dub,
                Translation::Raw => 0,
            };
            if count > 0 {
                format!("{} [{} episodes]", s.title, count)
            } else {
                s.title.clone()
            }
        })
        .collect();
    let selection = Select::with_theme(theme)
        .with_prompt("Select a show (Esc to cancel)")
        .items(&options)
        .default(0)
        .interact_opt()?;
    Ok(selection.map(|idx| shows[idx].clone()))
}

pub(crate) fn select_show_with_provider(
    items: &[(Provider, ShowInfo)],
    translation: Translation,
    theme: &dialoguer::theme::ColorfulTheme,
) -> Result<Option<(Provider, ShowInfo)>> {
    let options: Vec<String> = items
        .iter()
        .map(|(provider, s)| {
            let count = match translation {
                Translation::Sub => s.available_eps.sub,
                Translation::Dub => s.available_eps.dub,
                Translation::Raw => 0,
            };
            if count > 0 {
                format!(
                    "{} [{}] [{} episodes]",
                    s.title,
                    provider.display_name(),
                    count
                )
            } else {
                format!("{} [{}]", s.title, provider.display_name())
            }
        })
        .collect();
    let selection = Select::with_theme(theme)
        .with_prompt("Select a show (Esc to cancel)")
        .items(&options)
        .default(0)
        .interact_opt()?;
    Ok(selection.map(|idx| items[idx].clone()))
}

pub async fn play_show<P: SyncProvider>(
    client: &impl AnimeProvider,
    history: &mut History,
    history_path: &Path,
    translation: Translation,
    provider: Provider,
    mut show: ShowInfo,
    prefer_episode: Option<String>,
    override_last_watched: Option<String>,
    auto_play_next: bool,
    sync_provider: Option<&P>,
    binge: bool,
    config: &AppConfig,
    skip_opts: SkipOptions,
    download_range: Option<String>,
) -> Result<()> {
    let episodes = client.fetch_episodes(&show.id, translation).await?;
    if episodes.is_empty() {
        bail!(
            "No {} episodes available for {}",
            translation.label(),
            show.title
        );
    }

    if let Some(range_str) = download_range {
        let target_episodes = crate::downloader::parse_episode_range(&range_str, &episodes)?;
        println!(
            "Downloading {} episode(s) for '{}' ({}): {:?}",
            target_episodes.len(),
            show.title,
            translation.label(),
            target_episodes
        );

        for chosen in &target_episodes {
            println!("Fetching streams for episode {}...", chosen);
            let streams = match client.fetch_streams(&show.id, translation, chosen).await {
                Ok(s) if !s.is_empty() => s,
                _ => {
                    eprintln!("No supported streams found for episode {chosen}. Skipping.");
                    continue;
                }
            };

            let stream = if provider == Provider::Anidb {
                select_stream_by_quality(streams, config.anidb.quality)?
            } else {
                select_stream_by_quality(streams, crate::config::AnidbQuality::Highest)?
            };

            let Some(stream) = stream else {
                eprintln!("No stream selected for episode {chosen}. Skipping.");
                continue;
            };

            crate::downloader::download_episode(&stream, &show.title, chosen, config).await?;
        }

        return Ok(());
    }

    if show.mal_id.is_none() {
        if let Ok(Some(mid)) = client.fetch_mal_id(&show.id).await {
            show.mal_id = Some(mid);
        }
    }

    let sorted_episodes = sorted_episode_labels(&episodes);

    let latest_available = sorted_episodes
        .last()
        .cloned()
        .expect("episodes is non-empty; bail!() above ensures this");
    println!(
        "Found {} {} episodes. Latest available: {}.",
        episodes.len(),
        translation.label(),
        latest_available
    );

    let last_watched_local = history.last_episode(&show.id, translation);
    let last_watched = override_last_watched.or(last_watched_local);
    if let Some(prev) = &last_watched {
        println!("Last watched {} episode: {}.", translation.label(), prev);
    }

    let fallback_ep = last_watched
        .clone()
        .unwrap_or_else(|| latest_available.clone());
    let (mut current_episode, mut skip_selection) = match &prefer_episode {
        Some(ep) if episodes.contains(ep) => (ep.clone(), true),
        Some(ep) => {
            println!(
                "Episode '{}' does not exist for '{}'. Showing episode list.",
                ep, show.title
            );
            (fallback_ep, false)
        }
        None => {
            if auto_play_next {
                if let Some(last) = &last_watched {
                    if let Some(next) = next_episode_label_presorted(last, &sorted_episodes) {
                        (next, true)
                    } else {
                        (last.clone(), false)
                    }
                } else {
                    (sorted_episodes.first().unwrap().clone(), true)
                }
            } else {
                (fallback_ep, false)
            }
        }
    };

    let theme = theme();
    loop {
        let default_idx = episodes
            .iter()
            .position(|ep| ep == &current_episode)
            .or_else(|| episodes.iter().position(|ep| ep == &latest_available))
            .unwrap_or(0);

        let idx = if skip_selection || binge {
            skip_selection = false;
            default_idx
        } else {
            let selection = Select::with_theme(&theme)
                .with_prompt("Episode to play (type to search, Esc to cancel)")
                .items(&episodes)
                .default(default_idx)
                .interact_opt()?;
            let Some(i) = selection else {
                println!("Exiting playback loop.");
                return Ok(());
            };
            i
        };

        let chosen = episodes[idx].clone();
        let auto_advance = idx == default_idx;

        println!("Fetching streams for episode {}...", chosen);
        let streams = match client.fetch_streams(&show.id, translation, &chosen).await {
            Ok(s) if !s.is_empty() => s,
            res => {
                if let Err(ref err) = res {
                    if let Some(req_err) = err.downcast_ref::<reqwest::Error>() {
                        if req_err.status() == Some(StatusCode::BAD_REQUEST) {
                            eprintln!(
                                "Episode {chosen} is not yet available for {} translation.",
                                translation.label()
                            );
                            current_episode = latest_available.clone();
                            continue;
                        }
                    }
                }
                Vec::new()
            }
        };

        if streams.is_empty() {
            eprintln!(
                "No supported streams found for episode {chosen}. Try another episode or rerun later."
            );
            current_episode = latest_available.clone();
            continue;
        }

        let Some(stream) = (if provider == Provider::Anidb {
            select_stream_by_quality(streams, config.anidb.quality)?
        } else {
            choose_stream(streams)?
        }) else {
            continue;
        };

        let next_candidate = next_episode_label_presorted(&chosen, &sorted_episodes);

        launch_player(
            &stream,
            &show.title,
            &chosen,
            show.mal_id.as_deref(),
            config,
            skip_opts.clone(),
        )
        .await?;

        history.upsert(HistoryEntry {
            show_id: show.id.clone(),
            show_title: show.title.clone(),
            episode: chosen.clone(),
            translation,
            provider,
            is_manga: false,
            watched_at: Utc::now(),
        });
        history.save(history_path)?;

        if let Some(sync_prov) = sync_provider {
            let ep_num = chosen.parse::<u32>().unwrap_or(0);
            if let Err(err) = sync_prov
                .sync_episode(&show.id, &show.title, ep_num, provider)
                .await
            {
                eprintln!("[sync] error: {err}");
            }
        }

        match (auto_advance || binge, next_candidate) {
            (true, Some(next)) => current_episode = next,
            (true, None) => {
                println!("No further episodes found. Exiting.");
                return Ok(());
            }
            (false, candidate) => current_episode = candidate.unwrap_or(chosen),
        }
    }
}

async fn resolve_show_info<C: AnimeProvider>(
    client: &C,
    title: &str,
    translation: Translation,
) -> Result<ShowInfo> {
    let shows = client.search_shows(title, translation).await?;
    if let Some(matched) = shows.iter().find(|s| s.title.eq_ignore_ascii_case(title)) {
        Ok(matched.clone())
    } else if let Some(first) = shows.first() {
        Ok(first.clone())
    } else {
        bail!("No results found for \"{}\"", title);
    }
}

async fn resolve_manga_info<C: MangaProvider>(
    client: &C,
    title: &str,
    translation: Translation,
) -> Result<MangaInfo> {
    let mangas = client.search_mangas(title, translation).await?;
    if let Some(matched) = mangas.iter().find(|m| m.title.eq_ignore_ascii_case(title)) {
        Ok(matched.clone())
    } else if let Some(first) = mangas.first() {
        Ok(first.clone())
    } else {
        bail!("No results found for \"{}\"", title);
    }
}
