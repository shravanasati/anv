use anyhow::{Result, bail};
use dialoguer::Select;
use reqwest::StatusCode;

use crate::Cli;
use crate::aniskip::SkipOptions;
use crate::cmd::media::{
    ConsumeOutcome, MediaContext, MediaEntry, MediaLoopConfig, run_media_loop,
};
use crate::cmd::search::aggregate_anime_search;
use crate::config::AppConfig;
use crate::history::{History, theme};
use crate::player::{launch_player, select_stream_by_quality};
use crate::providers::AnimeProvider;
use crate::sync::SyncProvider;
use crate::types::{EpisodeCounts, Provider, ShowInfo, Translation};
use crate::utils::{search_single_with_timeout, sorted_episode_labels};

#[allow(clippy::too_many_arguments)]
pub async fn run_anime_flow<P: SyncProvider>(
    cli: &Cli,
    config: &AppConfig,
    translation: Translation,
    history_mode: bool,
    history: &mut History,
    sync_provider: Option<&P>,
    binge: bool,
    auto_play_next: bool,
    override_provider: Option<Provider>,
    download_range: Option<String>,
) -> Result<()> {
    let skip_opts = SkipOptions::from(cli);

    if history_mode {
        if let Some(entry) = history.select_entry()? {
            let target_provider = override_provider.unwrap_or(entry.provider);

            if !target_provider.is_anime() {
                bail!(
                    "The provider for this history entry ('{}') is no longer available. Please specify an active provider with -p/--provider (e.g. -p anineko).",
                    entry.provider.display_name()
                );
            }

            let client = target_provider.anime_client()?;
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
                entry.translation,
                target_provider,
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
        return Ok(());
    }

    if cli.query.is_empty() {
        println!("No query provided. Use `anv <name>` to search, or `anv history` to resume.");
        return Ok(());
    }

    let query = cli.query.join(" ");
    let theme = theme();

    let timeout_secs = cli.timeout.unwrap_or(config.timeout);
    let search_timeout = std::time::Duration::from_secs(timeout_secs);

    let provider = override_provider.unwrap_or(config.preferred_provider);
    if !provider.is_anime() {
        bail!(
            "Provider '{}' does not support anime. Valid anime providers: {}",
            provider.display_name(),
            Provider::valid_anime_providers()
        );
    }

    match provider {
        Provider::All => {
            let combined =
                aggregate_anime_search(&query, translation, search_timeout, timeout_secs).await?;

            if combined.is_empty() {
                bail!("No results for \"{}\" ({})", query, translation.label());
            }

            let selection = select_show_with_provider(&combined, translation, &theme)?;
            let Some((selected_provider, show)) = selection else {
                return Ok(());
            };

            let client = selected_provider.anime_client()?;
            play_show(
                &client,
                history,
                translation,
                selected_provider,
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
        _ => {
            let client = provider.anime_client()?;
            let shows = search_single_with_timeout(
                search_timeout,
                provider.display_name(),
                timeout_secs,
                client.search_shows(&query, translation),
            )
            .await?;
            if shows.is_empty() {
                bail!(
                    "No results for \"{}\" ({}) on {}",
                    query,
                    translation.label(),
                    provider.display_name()
                );
            }
            let show = select_show(&shows, translation, &theme)?;
            let Some(show) = show else {
                return Ok(());
            };
            play_show(
                &client,
                history,
                translation,
                provider,
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
            let count = s.episode_count_for(translation);
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
            let count = s.episode_count_for(translation);
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

#[allow(clippy::too_many_arguments)]
pub async fn play_show<P: SyncProvider>(
    client: &impl AnimeProvider,
    history: &mut History,
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

            let stream = select_stream_by_quality(streams, crate::config::Quality::Highest)?;

            let Some(stream) = stream else {
                eprintln!("No stream selected for episode {chosen}. Skipping.");
                continue;
            };

            crate::downloader::download_episode(&stream, &show.title, chosen, config).await?;
        }

        return Ok(());
    }

    if show.mal_id.is_none() {
        match client.fetch_mal_id(&show.id).await {
            Ok(Some(mid)) => show.mal_id = Some(mid),
            Ok(None) => {}
            Err(err) => crate::dbg_log!("anime", "fetch_mal_id error for {}: {err}", show.id),
        }
    }

    let sorted_episodes = sorted_episode_labels(&episodes);

    let last_watched_local = history.last_episode(&show.id, translation);
    // Resolve override_last_watched by index when the label isn't in the
    // episode list (e.g. MAL says "11 watched" but provider lists "67"-"78").
    let override_last_watched = override_last_watched.map(|label| {
        if episodes.contains(&label) {
            label.clone()
        } else {
            label
                .parse::<usize>()
                .ok()
                .filter(|&n| n >= 1)
                .and_then(|n| sorted_episodes.get(n - 1))
                .map(|resolved| {
                    println!(
                        "Last-watched label '{}' resolved by index to episode '{}'.",
                        label, resolved
                    );
                    resolved.clone()
                })
                .unwrap_or(label)
        }
    });
    let last_watched = override_last_watched.or(last_watched_local);

    let items: Vec<MediaEntry> = episodes
        .iter()
        .map(|label| MediaEntry {
            id: show.id.clone(),
            label: label.clone(),
        })
        .collect();

    let show_id = show.id.clone();
    let show_title = show.title.clone();
    let mal_id = show.mal_id.clone();
    let consume = move |entry: &MediaEntry, ctx: &MediaContext| {
        let label = entry.label.clone();
        let show_id = show_id.clone();
        let show_title = show_title.clone();
        let mal_id = mal_id.clone();
        let skip_opts = skip_opts.clone();
        let ep_num = ctx.ep_num;
        let latest = ctx.latest.to_string();
        async move {
            println!("Fetching streams for episode {}...", label);
            let streams = match client.fetch_streams(&show_id, translation, &label).await {
                Ok(s) if !s.is_empty() => s,
                res => {
                    if let Err(ref err) = res {
                        if let Some(req_err) = err.downcast_ref::<reqwest::Error>() {
                            if req_err.status() == Some(StatusCode::BAD_REQUEST) {
                                eprintln!(
                                    "Episode {} is not yet available for {} translation.",
                                    label,
                                    translation.label()
                                );
                                return Ok(ConsumeOutcome::RetryWith(latest.clone()));
                            }
                        }
                    }
                    Vec::new()
                }
            };

            if streams.is_empty() {
                eprintln!(
                    "No supported streams found for episode {label}. Try another episode or rerun later."
                );
                return Ok(ConsumeOutcome::RetryWith(latest.clone()));
            }

            let Some(stream) = select_stream_by_quality(streams, config.quality)? else {
                return Ok(ConsumeOutcome::Retry);
            };

            launch_player(
                &stream,
                &show_title,
                &label,
                ep_num as usize,
                mal_id.as_deref(),
                config,
                skip_opts,
            )
            .await?;

            if let Some(sync_prov) = sync_provider {
                // `ep_num` is the 1-based position of the label in the sorted
                // list, converting cumulative provider labels (e.g. "30" = ep 6
                // of a 12-ep season) to the season-relative count MAL expects.
                if let Err(err) = sync_prov
                    .sync_episode(
                        &show_id,
                        &show_title,
                        ep_num,
                        provider,
                        mal_id.as_deref().and_then(|s| s.parse().ok()),
                    )
                    .await
                {
                    eprintln!("[sync] error: {err}");
                }
            }

            Ok(ConsumeOutcome::Consumed)
        }
    };

    run_media_loop(
        &show.title,
        items,
        prefer_episode,
        last_watched,
        auto_play_next,
        binge,
        history,
        translation,
        provider,
        MediaLoopConfig {
            select_prompt: "Episode to play (type to search, Esc to cancel)",
            use_fuzzy: false,
            unit_plural: "episodes",
            unit_singular: "episode",
            last_verb: "watched",
        },
        consume,
    )
    .await
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
