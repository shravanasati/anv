use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::Cli;
use crate::cmd::anime::play_show;
use crate::config::AppConfig;
use crate::history::{History, theme};
use crate::providers::{AnimeProvider, allanime::AllAnimeClient};
use crate::sync::mal::{MalClient, MalToken, MalWatchlistEntry};
use crate::types::{EpisodeCounts, Provider, ShowInfo, Translation};

use crate::aniskip::SkipOptions;

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
) -> Result<()> {
    println!("Fetching your MAL '{}' list...", list_name);
    let watchlist = match list_type {
        "watching" => mal_client.fetch_watching().await?,
        _ => mal_client.fetch_plan_to_watch().await?,
    };

    if watchlist.is_empty() {
        println!("Your MAL '{}' list is empty.", list_name);
        return Ok(());
    }

    let allanime = AllAnimeClient::new(config.prefer_english_titles, Some(&config.api_proxy))?;
    let theme = theme();

    let skip_opts = SkipOptions {
        skip_op: cli.skip_op,
        skip_ed: cli.skip_ed,
        skip_mixed_op: cli.skip_mixed_op,
        skip_mixed_ed: cli.skip_mixed_ed,
        skip_recap: cli.skip_recap,
    };

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
        let cached_allanime_id = mal_client.cached_allanime_id(entry.mal_id);

        let (allanime_id, mal_id) = if let Some(id) = cached_allanime_id {
            (id, Some(entry.mal_id.to_string()))
        } else {
            println!("Searching AllAnime for \"{}\"...", entry.title);
            let results = allanime.search_shows(&entry.title, translation).await?;

            if let Some(matched) = results
                .iter()
                .find(|s| s.mal_id.as_deref() == Some(&entry.mal_id.to_string()))
            {
                mal_client.cache_allanime_id(&matched.id, entry.mal_id);
                (matched.id.clone(), matched.mal_id.clone())
            } else {
                match results.len() {
                    0 => {
                        println!(
                            "No AllAnime results for \"{}\". Try a different search query (or Esc to go back).",
                            entry.title
                        );
                        let query: String = dialoguer::Input::with_theme(&theme)
                            .with_prompt("Search query")
                            .allow_empty(true)
                            .interact_text()?;
                        if query.trim().is_empty() {
                            continue;
                        }
                        let retry = allanime.search_shows(query.trim(), translation).await?;
                        if retry.is_empty() {
                            println!("Still no results. Going back to watchlist.");
                            continue;
                        }
                        let opts: Vec<String> = retry
                            .iter()
                            .map(|s| format!("{} [{} ep]", s.title, s.available_eps.sub))
                            .collect();
                        let pick = dialoguer::Select::with_theme(&theme)
                            .with_prompt(format!(
                                "Which AllAnime entry matches \"{}\"? (Esc = back)",
                                entry.title
                            ))
                            .items(&opts)
                            .default(0)
                            .interact_opt()?;
                        let Some(i) = pick else { continue };
                        let chosen = &retry[i];
                        mal_client.cache_allanime_id(&chosen.id, entry.mal_id);
                        (chosen.id.clone(), chosen.mal_id.clone())
                    }
                    _ => {
                        let opts: Vec<String> = results
                            .iter()
                            .map(|s| format!("{} [{} ep]", s.title, s.available_eps.sub))
                            .collect();
                        let pick = dialoguer::Select::with_theme(&theme)
                            .with_prompt(format!(
                                "Which AllAnime entry matches \"{}\"? (Esc = back)",
                                entry.title
                            ))
                            .items(&opts)
                            .default(0)
                            .interact_opt()?;
                        let Some(i) = pick else {
                            continue;
                        };
                        let chosen = &results[i];
                        mal_client.cache_allanime_id(&chosen.id, entry.mal_id);
                        (chosen.id.clone(), chosen.mal_id.clone())
                    }
                }
            }
        };

        let show = ShowInfo {
            id: allanime_id,
            title: entry.title.clone(),
            mal_id,
            available_eps: EpisodeCounts::default(),
        };

        return play_show(
            &allanime,
            history,
            history_path,
            translation,
            Provider::Allanime,
            show,
            episode.clone(),
            entry.num_episodes_watched.map(|n| n.to_string()),
            auto_play_next,
            Some(mal_client),
            binge,
            config,
            skip_opts,
        )
        .await;
    }
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
