use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::AppConfig;

mod aniskip;
mod cache;
mod cmd;
mod config;
mod history;
mod player;
mod providers;
mod proxy;
mod sync;
mod types;
mod utils;

use history::{History, history_path};
use providers::{allanime::AllAnimeClient, mangadex::MangaDexClient, mangapill::MangapillClient};
use sync::mal::build_mal_client_if_enabled;
use types::{Provider, Translation};

#[derive(Debug, Parser)]
#[command(
    name = "anv",
    about = "Stream anime or read manga via mpv.",
    long_about = "anv lets you search, stream anime, and read manga directly from the terminal.",
    version
)]
pub struct Cli {
    /// Play dubbed audio instead of the default subtitled version.
    #[arg(short = 'd', long)]
    pub dub: bool,

    /// Use raw/untranslated source (no subtitles). For manga: show raw scans.
    #[arg(short = 'r', long)]
    pub raw: bool,

    /// Search and read manga instead of anime.
    #[arg(short = 'm', long)]
    pub manga: bool,

    /// Automatically play the next episode without prompting (binge mode).
    #[arg(short = 'b', long)]
    pub binge: bool,

    /// Content provider to use for streaming or reading.
    #[arg(
        short = 'p',
        long,
        default_value = "allanime",
        value_enum,
        value_name = "PROVIDER"
    )]
    pub provider: Provider,

    /// Override the directory used to cache manga page images.
    #[arg(short = 'C', long, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Start playback/reading from a specific episode or chapter number.
    #[arg(short = 'e', long, value_name = "EPISODE")]
    pub episode: Option<String>,

    /// Skip opening sequences (override config).
    #[arg(long, action = clap::ArgAction::Set)]
    pub skip_op: Option<bool>,

    /// Skip ending sequences (override config).
    #[arg(long, action = clap::ArgAction::Set)]
    pub skip_ed: Option<bool>,

    /// Skip mixed opening sequences (override config).
    #[arg(long, action = clap::ArgAction::Set)]
    pub skip_mixed_op: Option<bool>,

    /// Skip mixed ending sequences (override config).
    #[arg(long, action = clap::ArgAction::Set)]
    pub skip_mixed_ed: Option<bool>,

    /// Skip recap sequences (override config).
    #[arg(long, action = clap::ArgAction::Set)]
    pub skip_recap: Option<bool>,

    /// Title to search for (e.g. `anv "attack on titan"`).
    #[arg(value_name = "QUERY")]
    query: Vec<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Browse your watch/read history and resume from where you left off.
    History {
        /// Automatically play the next episode without prompting (binge mode).
        #[arg(short = 'b', long)]
        binge: bool,

        /// Resume from the next episode/chapter instead of the last watched.
        #[arg(short = 'n', long = "next-episode")]
        next_episode: bool,
    },

    /// Browse your MAL "Plan to Watch" list and start streaming.
    /// Requires MAL sync to be configured (`anv sync enable mal`).
    Watchlist {
        /// Automatically play the next episode without prompting (binge mode).
        #[arg(short = 'b', long)]
        binge: bool,

        /// Play dubbed audio instead of the default subtitled version.
        #[arg(short = 'd', long)]
        dub: bool,

        /// Start playback from a specific episode number.
        #[arg(short = 'e', long, value_name = "EPISODE")]
        episode: Option<String>,

        /// Resume from the next episode/chapter instead of the last watched.
        #[arg(short = 'n', long = "next-episode")]
        next_episode: bool,
    },

    /// Browse your MAL "Watching" list and start streaming.
    /// Requires MAL sync to be configured (`anv sync enable mal`).
    Watching {
        /// Automatically play the next episode without prompting (binge mode).
        #[arg(short = 'b', long)]
        binge: bool,

        /// Play dubbed audio instead of the default subtitled version.
        #[arg(short = 'd', long)]
        dub: bool,

        /// Start playback from a specific episode number.
        #[arg(short = 'e', long, value_name = "EPISODE")]
        episode: Option<String>,

        /// Resume from the next episode/chapter instead of the last watched.
        #[arg(short = 'n', long = "next-episode")]
        next_episode: bool,
    },

    /// Manage sync with external anime list services (e.g. MyAnimeList).
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum SyncAction {
    /// Enable sync with a list provider and complete the authentication flow.
    Enable {
        #[command(subcommand)]
        provider: SyncProviderCmd,
    },
    /// Show current sync status and MAL authentication state.
    Status,
    /// Disable MAL sync and write the updated config.
    Disable,
}

#[derive(Debug, Subcommand)]
pub enum SyncProviderCmd {
    /// Authenticate with MyAnimeList via OAuth PKCE. Stores a token locally.
    Mal,
}

#[tokio::main]
async fn main() -> Result<()> {
    run().await.map_err(|err| {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    })
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let cfg = AppConfig::load().unwrap_or_else(|err| {
        eprintln!("Warning: failed to load config ({err}), using defaults.");
        AppConfig::default()
    });

    // Handle subcommands
    match &cli.command {
        Some(Commands::History {
            binge: history_binge,
            next_episode: history_next,
        }) => {
            let history_path = history_path()?;
            let mut history = History::load(&history_path)?;
            let mal_client = build_mal_client_if_enabled(&cfg).await;
            let binge = *history_binge || cli.binge || cfg.binge;
            let auto_play_next = *history_next || cfg.auto_play_next;
            return cmd::anime::run_anime_flow(
                &cli,
                &cfg,
                Translation::Sub,
                true,
                &mut history,
                &history_path,
                mal_client.as_ref(),
                binge,
                auto_play_next,
            )
            .await;
        }
        Some(Commands::Watchlist {
            binge: wl_binge,
            dub: wl_dub,
            episode: wl_episode,
            next_episode: wl_next,
        }) => {
            let history_path = history_path()?;
            let mut history = History::load(&history_path)?;
            let mal_client = build_mal_client_if_enabled(&cfg).await;
            let binge = *wl_binge || cli.binge || cfg.binge;
            let auto_play_next = *wl_next || cfg.auto_play_next;
            let translation = if *wl_dub || cli.dub {
                Translation::Dub
            } else {
                Translation::Sub
            };
            let episode = wl_episode.clone().or_else(|| cli.episode.clone());
            return match mal_client.as_ref() {
                None => {
                    eprintln!(
                        "error: MAL sync is not configured.\n\
                         Run `anv sync enable mal` to authenticate."
                    );
                    Ok(())
                }
                Some(client) => {
                    cmd::sync::run_mal_list(
                        "plan_to_watch",
                        "Plan to Watch",
                        translation,
                        binge,
                        episode,
                        auto_play_next,
                        &mut history,
                        &history_path,
                        client,
                        &cfg,
                        &cli,
                    )
                    .await
                }
            };
        }
        Some(Commands::Watching {
            binge: w_binge,
            dub: w_dub,
            episode: w_episode,
            next_episode: w_next,
        }) => {
            let history_path = history_path()?;
            let mut history = History::load(&history_path)?;
            let mal_client = build_mal_client_if_enabled(&cfg).await;
            let binge = *w_binge || cli.binge || cfg.binge;
            let auto_play_next = *w_next || cfg.auto_play_next;
            let translation = if *w_dub || cli.dub {
                Translation::Dub
            } else {
                Translation::Sub
            };
            let episode = w_episode.clone().or_else(|| cli.episode.clone());
            return match mal_client.as_ref() {
                None => {
                    eprintln!(
                        "error: MAL sync is not configured.\n\
                         Run `anv sync enable mal` to authenticate."
                    );
                    Ok(())
                }
                Some(client) => {
                    cmd::sync::run_mal_list(
                        "watching",
                        "Watching",
                        translation,
                        binge,
                        episode,
                        auto_play_next,
                        &mut history,
                        &history_path,
                        client,
                        &cfg,
                        &cli,
                    )
                    .await
                }
            };
        }
        Some(Commands::Sync {
            action:
                SyncAction::Enable {
                    provider: SyncProviderCmd::Mal,
                },
        }) => return cmd::sync::run_sync_enable_mal(cfg).await,
        Some(Commands::Sync {
            action: SyncAction::Status,
        }) => return cmd::sync::run_sync_status(&cfg),
        Some(Commands::Sync {
            action: SyncAction::Disable,
        }) => return cmd::sync::run_sync_disable(cfg).await,
        _ => {}
    }

    let history_path = history_path()?;
    let mut history = History::load(&history_path)?;

    // Build MAL client if sync is enabled and a token exists
    let mal_client = build_mal_client_if_enabled(&cfg).await;

    if cli.manga {
        let translation = if cli.raw {
            Translation::Raw
        } else {
            Translation::Sub
        };
        match cli.provider {
            Provider::Allanime => {
                let client = AllAnimeClient::new()?;
                return cmd::manga::run_manga_flow(
                    &cli,
                    translation,
                    &mut history,
                    &history_path,
                    &client,
                    cfg.auto_play_next,
                    &cfg,
                )
                .await;
            }
            Provider::Mangadex => {
                let client = MangaDexClient::new()?;
                return cmd::manga::run_manga_flow(
                    &cli,
                    translation,
                    &mut history,
                    &history_path,
                    &client,
                    cfg.auto_play_next,
                    &cfg,
                )
                .await;
            }
            Provider::Mangapill => {
                let client = MangapillClient::new()?;
                return cmd::manga::run_manga_flow(
                    &cli,
                    translation,
                    &mut history,
                    &history_path,
                    &client,
                    cfg.auto_play_next,
                    &cfg,
                )
                .await;
            }
        }
    }

    let translation = if cli.dub {
        Translation::Dub
    } else {
        Translation::Sub
    };

    if !matches!(cli.provider, Provider::Allanime) {
        eprintln!("Warning: Only 'allanime' provider supports anime. Switching to 'allanime'.");
    }
    let binge = cli.binge || cfg.binge;
    let auto_play_next = cfg.auto_play_next;
    cmd::anime::run_anime_flow(
        &cli,
        &cfg,
        translation,
        false,
        &mut history,
        &history_path,
        mal_client.as_ref(),
        binge,
        auto_play_next,
    )
    .await
}
