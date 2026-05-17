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
use crate::player::{choose_stream, launch_player};
use crate::providers::{
    AnimeProvider, allanime::AllAnimeClient, mangadex::MangaDexClient, mangapill::MangapillClient,
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
) -> Result<()> {
    let client = AllAnimeClient::new()?;

    let skip_opts = SkipOptions {
        skip_op: cli.skip_op,
        skip_ed: cli.skip_ed,
        skip_mixed_op: cli.skip_mixed_op,
        skip_mixed_ed: cli.skip_mixed_ed,
        skip_recap: cli.skip_recap,
    };

    if history_mode {
        if let Some(entry) = history.select_entry()? {
            if entry.is_manga {
                let manga_info = MangaInfo {
                    id: entry.show_id.clone(),
                    title: entry.show_title.clone(),
                    available_chapters: ChapterCounts::default(),
                };
                match entry.provider {
                    Provider::Allanime => {
                        read_manga(
                            &AllAnimeClient::new()?,
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
                            entry.provider,
                            config,
                        )
                        .await?
                    }
                    Provider::Mangadex => {
                        read_manga(
                            &MangaDexClient::new()?,
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
                            entry.provider,
                            config,
                        )
                        .await?
                    }
                    Provider::Mangapill => {
                        read_manga(
                            &MangapillClient::new()?,
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
                            entry.provider,
                            config,
                        )
                        .await?
                    }
                }
            } else {
                play_show(
                    &client,
                    history,
                    history_path,
                    entry.translation,
                    Provider::Allanime,
                    ShowInfo {
                        id: entry.show_id.clone(),
                        title: entry.show_title.clone(),
                        mal_id: None,
                        available_eps: EpisodeCounts::default(),
                    },
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
                )
                .await?;
            }
        }
        return Ok(());
    }

    if cli.query.is_empty() {
        println!("No query provided. Use `anv <name>` to search, or `anv history` to resume.");
        return Ok(());
    }

    let query = cli.query.join(" ");
    let shows = client.search_shows(&query, translation).await?;
    if shows.is_empty() {
        bail!("No results for \"{}\" ({})", query, translation.label());
    }

    let theme = theme();
    let options: Vec<String> = shows
        .iter()
        .map(|s| {
            let count = match translation {
                Translation::Sub => s.available_eps.sub,
                Translation::Dub => s.available_eps.dub,
                Translation::Raw => 0,
            };
            format!("{} [{} episodes]", s.title, count)
        })
        .collect();
    let selection = Select::with_theme(&theme)
        .with_prompt("Select a show (Esc to cancel)")
        .items(&options)
        .default(0)
        .interact_opt()?;
    let Some(idx) = selection else {
        println!("Cancelled.");
        return Ok(());
    };
    let show = shows[idx].clone();
    play_show(
        &client,
        history,
        history_path,
        translation,
        Provider::Allanime,
        show,
        cli.episode.clone(),
        None,
        auto_play_next,
        sync_provider,
        binge,
        config,
        skip_opts,
    )
    .await
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
) -> Result<()> {
    let episodes = client.fetch_episodes(&show.id, translation).await?;
    if episodes.is_empty() {
        bail!(
            "No {} episodes available for {}",
            translation.label(),
            show.title
        );
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

    let fallback = last_watched
        .clone()
        .unwrap_or_else(|| latest_available.clone());
    let (mut current_episode, mut skip_selection) = match &prefer_episode {
        Some(ep) if episodes.contains(ep) => (ep.clone(), true),
        Some(ep) => {
            println!(
                "Episode '{}' does not exist for '{}'. Showing episode list.",
                ep, show.title
            );
            (fallback, false)
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
                (fallback, false)
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
            Ok(streams) => streams,
            Err(err) => {
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
                eprintln!("Error fetching streams: {}", err);
                continue;
            }
        };

        if streams.is_empty() {
            eprintln!(
                "No supported streams found for episode {chosen}. Try another episode or rerun later."
            );
            current_episode = latest_available.clone();
            continue;
        }

        let Some(stream) = choose_stream(streams)? else {
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

        if let Some(provider) = sync_provider {
            let ep_num = chosen.parse::<u32>().unwrap_or(0);
            if let Err(err) = provider.sync_episode(&show.id, &show.title, ep_num).await {
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
