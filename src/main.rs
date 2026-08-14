use anyhow::Result;
use clap::{Parser, Subcommand};
use config::AppConfig;

mod aniskip;
mod cmd;
mod config;
mod downloader;
mod history;
mod logger;
mod player;
mod providers;
mod sync;
mod types;
mod utils;

use history::{History, history_path};
use sync::mal::build_mal_client_if_enabled;
use types::{Provider, Translation};

#[derive(Debug, Parser)]
#[command(
    name = "anv",
    about = "Stream anime via mpv.",
    long_about = "anv lets you search and stream anime directly from the terminal.",
    version
)]
pub struct Cli {
    /// Play dubbed audio instead of the default subtitled version.
    #[arg(short = 'd', long)]
    pub dub: bool,

    /// Use raw/untranslated source (no subtitles).
    #[arg(short = 'r', long)]
    pub raw: bool,

    /// Automatically play the next episode without prompting (binge mode).
    #[arg(short = 'b', long)]
    pub binge: bool,

    /// Content provider to use for streaming.
    #[arg(short = 'p', long, value_enum, value_name = "PROVIDER")]
    pub provider: Option<Provider>,

    /// Timeout in seconds for provider search requests (overrides config).
    #[arg(short = 'T', long, value_name = "SECONDS")]
    pub timeout: Option<u64>,

    /// Start playback from a specific episode number.
    #[arg(short = 'e', long, value_name = "EPISODE")]
    pub episode: Option<String>,

    /// Download episode(s) instead of playing. Range can be a single episode (e.g. 4) or range (e.g. 1-5).
    #[arg(short = 'D', long = "download", value_name = "RANGE")]
    pub download: Option<String>,

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
    /// Browse your watch history and resume from where you left off.
    History {
        /// Automatically play the next episode without prompting (binge mode).
        #[arg(short = 'b', long)]
        binge: bool,

        /// Resume from the next episode instead of the last watched.
        #[arg(short = 'n', long = "next-episode")]
        next_episode: bool,

        /// Content provider to use for streaming.
        #[arg(short = 'p', long, value_enum, value_name = "PROVIDER")]
        provider: Option<Provider>,

        /// Download episode(s) instead of playing. Range can be a single episode (e.g. 4) or range (e.g. 1-5).
        #[arg(short = 'D', long = "download", value_name = "RANGE")]
        download: Option<String>,
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

        /// Resume from the next episode instead of the last watched.
        #[arg(short = 'n', long = "next-episode")]
        next_episode: bool,

        /// Content provider to use for streaming.
        #[arg(short = 'p', long, value_enum, value_name = "PROVIDER")]
        provider: Option<Provider>,

        /// Download episode(s) instead of playing. Range can be a single episode (e.g. 4) or range (e.g. 1-5).
        #[arg(short = 'D', long = "download", value_name = "RANGE")]
        download: Option<String>,
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

        /// Resume from the next episode instead of the last watched.
        #[arg(short = 'n', long = "next-episode")]
        next_episode: bool,

        /// Content provider to use for streaming.
        #[arg(short = 'p', long, value_enum, value_name = "PROVIDER")]
        provider: Option<Provider>,

        /// Download episode(s) instead of playing. Range can be a single episode (e.g. 4) or range (e.g. 1-5).
        #[arg(short = 'D', long = "download", value_name = "RANGE")]
        download: Option<String>,
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

/// Per-subcommand flags for the Watchlist / Watching handlers.
struct ListSubFlags<'a> {
    binge: bool,
    dub: bool,
    episode: &'a Option<String>,
    next_episode: bool,
    provider: &'a Option<Provider>,
    download: &'a Option<String>,
}

/// Shared handler for the Watchlist / Watching subcommands. Merges the
/// subcommand flags with global CLI flags and config before launching the list.
async fn run_list_command(
    list_type: &'static str,
    list_name: &'static str,
    sub: ListSubFlags<'_>,
    cli: &Cli,
    cfg: &AppConfig,
) -> Result<()> {
    let history_path = history_path()?;
    let mut history = History::load(&history_path)?;
    let mal_client = build_mal_client_if_enabled(cfg).await;
    let binge = sub.binge || cli.binge || cfg.binge;
    let auto_play_next = sub.next_episode || cfg.auto_play_next;
    let translation = if sub.dub || cli.dub {
        Translation::Dub
    } else {
        Translation::Sub
    };
    let episode = sub.episode.clone().or_else(|| cli.episode.clone());
    let provider = sub.provider.or(cli.provider).unwrap_or(cfg.preferred_provider);
    let download = sub.download.clone().or_else(|| cli.download.clone());

    match mal_client.as_ref() {
        None => {
            eprintln!(
                "error: MAL sync is not configured.\n\
                 Run `anv sync enable mal` to authenticate."
            );
            Ok(())
        }
        Some(client) => {
            cmd::sync::run_mal_list(
                list_type,
                list_name,
                translation,
                binge,
                episode,
                auto_play_next,
                &mut history,
                &history_path,
                client,
                cfg,
                cli,
                provider,
                download,
            )
            .await
        }
    }
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
            provider: history_provider,
            download: history_download,
        }) => {
            let history_path = history_path()?;
            let mut history = History::load(&history_path)?;
            let mal_client = build_mal_client_if_enabled(&cfg).await;
            let binge = *history_binge || cli.binge || cfg.binge;
            let auto_play_next = *history_next || cfg.auto_play_next;
            let provider_override = history_provider.or(cli.provider);
            let download = history_download.clone().or_else(|| cli.download.clone());
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
                provider_override,
                download,
            )
            .await;
        }
        Some(Commands::Watchlist {
            binge,
            dub,
            episode,
            next_episode,
            provider,
            download,
        }) => {
            return run_list_command(
                "plan_to_watch",
                "Plan to Watch",
                ListSubFlags {
                    binge: *binge,
                    dub: *dub,
                    episode,
                    next_episode: *next_episode,
                    provider,
                    download,
                },
                &cli,
                &cfg,
            )
            .await;
        }
        Some(Commands::Watching {
            binge,
            dub,
            episode,
            next_episode,
            provider,
            download,
        }) => {
            return run_list_command(
                "watching",
                "Watching",
                ListSubFlags {
                    binge: *binge,
                    dub: *dub,
                    episode,
                    next_episode: *next_episode,
                    provider,
                    download,
                },
                &cli,
                &cfg,
            )
            .await;
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

    let translation = if cli.dub {
        Translation::Dub
    } else {
        Translation::Sub
    };

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
        cli.provider,
        cli.download.clone(),
    )
    .await
}
