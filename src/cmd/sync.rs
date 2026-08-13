use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Duration;

use crate::Cli;
use crate::aniskip::SkipOptions;
use crate::cmd::anime::{play_show, select_show_with_provider};
use crate::cmd::search::aggregate_anime_search;
use crate::config::AppConfig;
use crate::history::{History, theme};
use crate::providers::{AnimeProvider, AnyAnimeClient};
use crate::sync::mal::{MalClient, MalToken, MalWatchlistEntry};
use crate::types::{EpisodeCounts, Provider, ShowInfo, Translation};

pub async fn run_mal_list(
    list_type: &str,
    list_name: &str,
    translation: Translation,
    binge: bool,
    episode: Option<String>,
    auto_play_next: bool,
    history: &mut History,
    history_path: &Path,
    mal_client: &MalClient,
    config: &AppConfig,
    cli: &Cli,
    provider: Provider,
    download_range: Option<String>,
) -> Result<()> {
    if !provider.is_anime() {
        bail!(
            "Provider '{}' does not support anime. Valid anime providers: {}",
            provider.display_name(),
            Provider::valid_anime_providers()
        );
    }

    println!("Fetching your MAL '{}' list...", list_name);
    let watchlist = match list_type {
        "watching" => mal_client.fetch_watching().await?,
        _ => mal_client.fetch_plan_to_watch().await?,
    };

    if watchlist.is_empty() {
        println!("Your MAL '{}' list is empty.", list_name);
        return Ok(());
    }

    let theme = theme();

    let skip_opts = SkipOptions::from(cli);

    let timeout_secs = cli.timeout.unwrap_or(config.timeout);
    let search_timeout = Duration::from_secs(timeout_secs);

    loop {
        let items: Vec<String> = watchlist
            .iter()
            .map(|e| {
                if list_type == "watching" {
                    let total = if e.num_episodes == 0 {
                        "?".to_string()
                    } else {
                        e.num_episodes.to_string()
                    };
                    let watched = e.num_episodes_watched.unwrap_or(0);
                    format!("{} [{}/{} ep]", e.title, watched, total)
                } else {
                    let ep_count = if e.num_episodes == 0 {
                        "? ep".to_string()
                    } else {
                        format!("{} ep", e.num_episodes)
                    };
                    let status_tag = match e.airing_status.as_str() {
                        "currently_airing" => " · airing",
                        "finished_airing" => " · finished",
                        _ => "",
                    };
                    format!("{} [{}{}]", e.title, ep_count, status_tag)
                }
            })
            .collect();

        let selection = dialoguer::FuzzySelect::with_theme(&theme)
            .with_prompt(format!("{} (type to search, Esc to cancel)", list_name))
            .items(&items)
            .default(0)
            .interact_opt()?;

        let Some(idx) = selection else {
            println!("Cancelled.");
            return Ok(());
        };

        let entry: &MalWatchlistEntry = &watchlist[idx];

        let resolved = resolve_mal_entry(
            entry,
            provider,
            translation,
            mal_client,
            &theme,
            search_timeout,
            timeout_secs,
        )
        .await?;
        let Some((selected_provider, show)) = resolved else {
            continue;
        };

        let client = selected_provider.anime_client()?;
        return play_with(
            &client,
            selected_provider,
            show,
            entry,
            mal_client,
            history,
            history_path,
            translation,
            episode.clone(),
            auto_play_next,
            binge,
            config,
            &skip_opts,
            download_range.clone(),
        )
        .await;
    }
}

/// Resolve the provider `(ShowInfo, Provider)` for a MAL list entry: the cached
/// provider ID if known, otherwise an "all providers" or single-provider search.
async fn resolve_mal_entry(
    entry: &MalWatchlistEntry,
    provider: Provider,
    translation: Translation,
    mal_client: &MalClient,
    theme: &dialoguer::theme::ColorfulTheme,
    search_timeout: Duration,
    timeout_secs: u64,
) -> Result<Option<(Provider, ShowInfo)>> {
    if provider == Provider::All {
        return resolve_via_all(entry, translation, theme, search_timeout, timeout_secs).await;
    }

    if let Some(cached_id) = mal_client.cached_id(entry.mal_id, provider) {
        return Ok(Some((
            provider,
            ShowInfo {
                id: cached_id,
                title: entry.title.clone(),
                mal_id: Some(entry.mal_id.to_string()),
                available_eps: EpisodeCounts::default(),
            },
        )));
    }

    resolve_via_single(entry, provider, translation, theme).await
}

/// Search every anime provider for the MAL entry's title and let the user pick.
async fn resolve_via_all(
    entry: &MalWatchlistEntry,
    translation: Translation,
    theme: &dialoguer::theme::ColorfulTheme,
    search_timeout: Duration,
    timeout_secs: u64,
) -> Result<Option<(Provider, ShowInfo)>> {
    let mut search_query = entry.title.clone();

    loop {
        let combined =
            aggregate_anime_search(&search_query, translation, search_timeout, timeout_secs)
                .await?;

        if combined.is_empty() {
            println!(
                "No results for \"{}\" across all providers. Try a different search query (or Esc to go back).",
                search_query
            );
            let query: String = dialoguer::Input::with_theme(theme)
                .with_prompt("Search query")
                .allow_empty(true)
                .interact_text()?;
            if query.trim().is_empty() {
                return Ok(None);
            }
            search_query = query.trim().to_string();
            continue;
        }

        let selection = select_show_with_provider(&combined, translation, theme)?;
        return Ok(selection);
    }
}

/// Search a single provider for the MAL entry's title, confirming the right
/// match when multiple results come back.
async fn resolve_via_single(
    entry: &MalWatchlistEntry,
    provider: Provider,
    translation: Translation,
    theme: &dialoguer::theme::ColorfulTheme,
) -> Result<Option<(Provider, ShowInfo)>> {
    let mut search_query = entry.title.clone();
    let mut chosen_show: Option<ShowInfo> = None;

    loop {
        println!(
            "Searching {} for \"{}\"...",
            provider.display_name(),
            search_query
        );
        let client = provider.anime_client()?;
        let results = client.search_shows(&search_query, translation).await?;

        if let Some(matched) = results
            .iter()
            .find(|s| s.mal_id.as_deref() == Some(&entry.mal_id.to_string()))
        {
            chosen_show = Some(matched.clone());
            break;
        }

        match results.len() {
            0 => {
                println!(
                    "No {} results for \"{}\". Try a different search query (or Esc to go back).",
                    provider.display_name(),
                    search_query
                );
                let query: String = dialoguer::Input::with_theme(theme)
                    .with_prompt("Search query")
                    .allow_empty(true)
                    .interact_text()?;
                if query.trim().is_empty() {
                    break;
                }
                search_query = query.trim().to_string();
            }
            1 => {
                chosen_show = Some(results[0].clone());
                break;
            }
            _ => {
                let opts: Vec<String> = results
                    .iter()
                    .map(|s| {
                        let count = s.episode_count_for(translation);
                        format!("{} [{} ep]", s.title, count)
                    })
                    .collect();
                let pick = dialoguer::Select::with_theme(theme)
                    .with_prompt(format!(
                        "Which {} entry matches \"{}\"? (Esc = back)",
                        provider.display_name(),
                        entry.title
                    ))
                    .items(&opts)
                    .default(0)
                    .interact_opt()?;
                let Some(i) = pick else {
                    break;
                };
                chosen_show = Some(results[i].clone());
                break;
            }
        }
    }

    let Some(show) = chosen_show else {
        return Ok(None);
    };
    Ok(Some((provider, show)))
}

/// Set the entry's MAL ID on the show, remember the mapping, and start playback.
async fn play_with(
    client: &AnyAnimeClient,
    provider: Provider,
    mut show: ShowInfo,
    entry: &MalWatchlistEntry,
    mal_client: &MalClient,
    history: &mut History,
    history_path: &Path,
    translation: Translation,
    episode: Option<String>,
    auto_play_next: bool,
    binge: bool,
    config: &AppConfig,
    skip_opts: &SkipOptions,
    download_range: Option<String>,
) -> Result<()> {
    show.mal_id = Some(entry.mal_id.to_string());
    mal_client.cache_id(&show.id, entry.mal_id, provider);

    play_show(
        client,
        history,
        history_path,
        translation,
        provider,
        show,
        episode,
        entry.num_episodes_watched.map(|n| n.to_string()),
        auto_play_next,
        Some(mal_client),
        binge,
        config,
        skip_opts.clone(),
        download_range,
    )
    .await
}

pub async fn run_sync_enable_mal(mut cfg: AppConfig) -> Result<()> {
    if cfg.mal.client_id.is_empty() {
        bail!(
            "MAL client_id is not set.\n\
             1. Go to https://myanimelist.net/apiconfig and create an application.\n\
             2. Set the app type to 'other' and redirect URI to: http://localhost:11422/callback\n\
             3. Copy the Client ID and add it to your config:\n\
             \n\
             [mal]\n\
             client_id = \"<your-client-id>\"\n\
             \n\
             Config location: {}",
            AppConfig::config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown>".into())
        );
    }

    match MalToken::load()? {
        Some(token) if !token.is_expired() => {
            println!("Already authenticated with MyAnimeList.");
            if !cfg.sync.enabled {
                cfg.sync.enabled = true;
                cfg.save().context("failed to save config")?;
                println!("Sync enabled in config.");
            } else {
                println!("Sync is already enabled.");
            }
            return Ok(());
        }
        _ => {}
    }

    let client_id = cfg.mal.client_id.clone();
    let _token = MalClient::authenticate(&client_id)
        .await
        .context("MAL OAuth flow failed")?;

    println!("\n Successfully authenticated with MyAnimeList!");
    println!(
        "Token stored at: {}",
        MalToken::token_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".into())
    );

    cfg.sync.enabled = true;
    cfg.save().context("failed to save config")?;
    println!("Sync enabled in config.");

    Ok(())
}

pub fn run_sync_status(cfg: &AppConfig) -> Result<()> {
    let config_path = AppConfig::config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".into());

    println!("── MAL Sync Status ──");
    println!(
        "  sync.enabled : {}",
        if cfg.sync.enabled { "yes" } else { "no" }
    );

    if cfg.mal.client_id.is_empty() {
        println!("  client_id    : not set  (add to {})", config_path);
    } else {
        let masked = format!("{}…", &cfg.mal.client_id[..cfg.mal.client_id.len().min(8)]);
        println!("  client_id    : {}", masked);
    }

    match MalToken::load() {
        Ok(Some(token)) => {
            if token.is_expired() {
                println!("  token        : expired  (run `anv sync enable mal` to refresh)");
            } else {
                println!(
                    "  token        : valid, expires {}",
                    token.expires_at.format("%Y-%m-%d %H:%M UTC")
                );
            }
        }
        Ok(None) => println!("  token        : not found  (run `anv sync enable mal`)"),
        Err(err) => println!("  token        : error reading ({err})"),
    }
    Ok(())
}

pub async fn run_sync_disable(mut cfg: AppConfig) -> Result<()> {
    if !cfg.sync.enabled {
        println!("Sync is already disabled.");
        return Ok(());
    }
    cfg.sync.enabled = false;
    cfg.save().context("failed to save config")?;
    println!("Sync disabled. Run `anv sync enable mal` to re-enable.");
    Ok(())
}
