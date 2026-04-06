use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use dialoguer::{Confirm, Input, Select, theme::ColorfulTheme};
use dirs_next::data_dir;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::AppConfig;

use super::{SyncProvider, SyncUpdate, WatchStatus, should_confirm_sync};

const MAL_AUTH_URL: &str = "https://myanimelist.net/v1/oauth2/authorize";
const MAL_TOKEN_URL: &str = "https://myanimelist.net/v1/oauth2/token";
const MAL_API_BASE: &str = "https://api.myanimelist.net/v2";
const OAUTH_PORT: u16 = 11422;
const OAUTH_REDIRECT_URI: &str = "http://localhost:11422/callback";
const CODE_VERIFIER_LEN: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MalToken {
    pub access_token: String,
    pub refresh_token: String,
    /// UTC timestamp when the access token expires.
    pub expires_at: DateTime<Utc>,
}

impl MalToken {
    pub fn is_expired(&self) -> bool {
        // Treat as expired 60 s before actual expiry so we refresh proactively.
        Utc::now() >= self.expires_at - chrono::Duration::seconds(60)
    }

    pub fn token_path() -> Result<PathBuf> {
        let base = data_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
        Ok(base.join("anv").join("mal_token.json"))
    }

    pub fn load() -> Result<Option<Self>> {
        let path = Self::token_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read token file {}", path.display()))?;
        let token: MalToken = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse token file {}", path.display()))?;
        Ok(Some(token))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::token_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create data directory {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self).context("failed to serialize token")?;
        fs::write(&path, data)
            .with_context(|| format!("failed to write token to {}", path.display()))?;
        Ok(())
    }
}

/// Persistent cache that maps AllAnime show IDs to MAL anime IDs.
/// The confirmation dialog is shown only for IDs not yet in this cache.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MalIdCache {
    entries: HashMap<String, u32>,
}

impl MalIdCache {
    fn cache_path() -> Result<PathBuf> {
        let base = data_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
        Ok(base.join("anv").join("mal_id_cache.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::cache_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read ID cache {}", path.display()))?;
        serde_json::from_str(&data)
            .with_context(|| format!("failed to parse ID cache {}", path.display()))
    }

    pub fn get(&self, allanime_id: &str) -> Option<u32> {
        self.entries.get(allanime_id).copied()
    }

    pub fn insert_and_save(&mut self, allanime_id: &str, mal_id: u32) -> Result<()> {
        self.entries.insert(allanime_id.to_string(), mal_id);
        let path = Self::cache_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create data directory {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self).context("failed to serialize ID cache")?;
        fs::write(&path, data)
            .with_context(|| format!("failed to write ID cache to {}", path.display()))?;
        Ok(())
    }
}

fn generate_code_verifier() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    (0..CODE_VERIFIER_LEN)
        .map(|_| CHARSET[rand::random_range(0..CHARSET.len())] as char)
        .collect()
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct AnimeSearchResponse {
    data: Vec<AnimeNode>,
}

#[derive(Debug, Deserialize)]
struct AnimeNode {
    node: AnimeDetail,
}

#[derive(Debug, Deserialize)]
struct AnimeDetail {
    id: u32,
    title: String,
    #[serde(default)]
    alternative_titles: AlternativeTitles,
}

#[derive(Debug, Default, Deserialize)]
struct AlternativeTitles {
    #[serde(default)]
    en: String,
    #[serde(default)]
    ja: String,
}

pub struct MalClient {
    client_id: String,
    http: Client,
    pub token: MalToken,
    /// Persistent cache mapping AllAnime show IDs → MAL anime IDs.
    /// Loaded once when the client is constructed; saved on every new mapping.
    id_cache: std::sync::Mutex<MalIdCache>,
    /// Show IDs the user has declined to sync during this session.
    /// Any show_id in here is silently skipped for the rest of the process.
    skipped_ids: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl MalClient {
    /// Build a `MalClient` from an existing (possibly expired) token.
    /// Call `MalClient::authenticate` first if no token exists.
    pub async fn from_token(client_id: String, token: MalToken) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;

        let id_cache = MalIdCache::load().unwrap_or_else(|err| {
            eprintln!("[sync] Warning: could not load ID cache ({err}), starting fresh.");
            MalIdCache::default()
        });

        let mut client = Self {
            client_id,
            http,
            token,
            id_cache: std::sync::Mutex::new(id_cache),
            skipped_ids: std::sync::Mutex::new(std::collections::HashSet::new()),
        };

        if client.token.is_expired() {
            let refresh = client.token.refresh_token.clone();
            client.token = client.refresh_token_inner(&refresh).await?;
            client.token.save()?;
        }

        Ok(client)
    }

    /// Full OAuth PKCE flow: opens browser, spins up local callback server,
    /// exchanges code for tokens, and saves them to disk.
    pub async fn authenticate(client_id: &str) -> Result<MalToken> {
        let verifier = generate_code_verifier();
        let challenge = verifier.clone();

        let auth_url = {
            let mut u = Url::parse(MAL_AUTH_URL).expect("MAL auth URL is valid");
            u.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", client_id)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "plain")
                .append_pair("redirect_uri", OAUTH_REDIRECT_URI);
            u.to_string()
        };

        println!("Opening MAL authorization page in your browser...");
        println!("If it doesn't open automatically, visit:\n  {auth_url}");
        let _ = open::that(&auth_url);

        // The callback listener uses blocking I/O; run it on a blocking thread
        // so we don't block the async executor.
        let code = tokio::task::spawn_blocking(Self::wait_for_callback)
            .await
            .context("OAuth callback task panicked")?
            .context("Failed to receive OAuth callback from browser")?;

        println!("Authorization code received. Exchanging for token...");

        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;

        let token = Self::exchange_code(&http, client_id, &code, &verifier).await?;
        token.save()?;
        Ok(token)
    }

    /// Wait on localhost:11422 for MAL's redirect and return the `code` param.
    fn wait_for_callback() -> Result<String> {
        let listener = TcpListener::bind(("127.0.0.1", OAUTH_PORT)).with_context(|| {
            format!(
                "Failed to bind OAuth listener on port {OAUTH_PORT}. Is another process using it?"
            )
        })?;

        println!(
            "Waiting for authorization callback on http://localhost:{OAUTH_PORT}/callback ..."
        );

        let (mut stream, _) = listener
            .accept()
            .context("Failed to accept OAuth connection")?;
        let mut reader = BufReader::new(stream.try_clone().context("failed to clone stream")?);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .context("failed to read OAuth request line")?;

        // Parse "GET /callback?code=...&... HTTP/1.1"
        let path = request_line.split_whitespace().nth(1).unwrap_or_default();

        let dummy_base = "http://localhost";
        let full_url = format!("{dummy_base}{path}");
        let parsed = Url::parse(&full_url).context("failed to parse OAuth callback URL")?;

        let code = parsed
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| {
                anyhow!("No 'code' parameter in OAuth callback. MAL may have returned an error.")
            })?;

        // Send a small success page back to the browser
        let body = b"<html><body><h2>anv: Authorization successful!</h2><p>You can close this tab.</p></body></html>";
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(body);

        Ok(code)
    }

    async fn exchange_code(
        http: &Client,
        client_id: &str,
        code: &str,
        verifier: &str,
    ) -> Result<MalToken> {
        let params = [
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", OAUTH_REDIRECT_URI),
        ];

        let resp: TokenResponse = http
            .post(MAL_TOKEN_URL)
            .form(&params)
            .send()
            .await
            .context("token exchange request failed")?
            .error_for_status()
            .context("MAL returned error on token exchange")?
            .json::<TokenResponse>()
            .await
            .context("failed to parse token response")?;

        Ok(MalToken {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            expires_at: Utc::now() + chrono::Duration::seconds(resp.expires_in as i64),
        })
    }

    async fn refresh_token_inner(&self, refresh_token: &str) -> Result<MalToken> {
        let params = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ];

        let resp: TokenResponse = self
            .http
            .post(MAL_TOKEN_URL)
            .form(&params)
            .send()
            .await
            .context("token refresh request failed")?
            .error_for_status()
            .context("MAL returned error on token refresh")?
            .json::<TokenResponse>()
            .await
            .context("failed to parse refresh token response")?;

        Ok(MalToken {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            expires_at: Utc::now() + chrono::Duration::seconds(resp.expires_in as i64),
        })
    }

    // Strips non alphanumeric characters from the title (otherwise leads to 400 from MAL search API)
    fn sanitize_query(query: &str) -> String {
        query
            .chars()
            .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(64)
            .collect()
    }

    /// Helper: fetch up to 5 MAL results for `query`.
    async fn search_mal(&self, query: &str) -> Vec<AnimeDetail> {
        let sanitized = Self::sanitize_query(query);
        if sanitized.is_empty() {
            eprintln!("[sync] MAL search: query is empty after sanitization.");
            return Vec::new();
        }

        let result = async {
            let resp = self
                .http
                .get(format!("{MAL_API_BASE}/anime"))
                .bearer_auth(&self.token.access_token)
                .query(&[
                    ("q", sanitized.as_str()),
                    ("limit", "5"),
                    ("fields", "id,title,alternative_titles"),
                ])
                .send()
                .await
                .context("MAL anime search request failed")?
                .error_for_status()
                .context("MAL returned error on anime search")?
                .json::<AnimeSearchResponse>()
                .await
                .context("failed to parse MAL anime search response")?;

            Ok::<_, anyhow::Error>(resp.data.into_iter().map(|n| n.node).collect::<Vec<_>>())
        }
        .await;

        match result {
            Ok(items) => items,
            Err(err) => {
                eprintln!("[sync] MAL search error for \"{sanitized}\": {err:#}");
                Vec::new()
            }
        }
    }

    /// Format an `AnimeDetail` for display in a menu item.
    fn format_result(detail: &AnimeDetail) -> String {
        let en = if detail.alternative_titles.en.is_empty() {
            &detail.title
        } else {
            &detail.alternative_titles.en
        };
        let ja = &detail.alternative_titles.ja;
        if ja.is_empty() {
            format!("{} [id:{}]", en, detail.id)
        } else {
            format!("{} ({}) [id:{}]", en, ja, detail.id)
        }
    }

    /// Search MAL for `title`, present a y/n/c prompt:
    ///   y – accept first result
    ///   n – skip sync for this show
    ///   c – correction mode: shows a dropdown of all results, lets the user
    ///       pick one, search again with a new query, or skip entirely.
    ///
    /// Returns `Some(mal_id)` when resolved, `None` when the user skips.
    pub async fn resolve_and_confirm_mal_id(&self, title: &str) -> Result<Option<u32>> {
        let mut results = self.search_mal(title).await;

        if results.is_empty() {
            println!("  [sync] No MAL results found for \"{title}\". Skipping sync.");
            return Ok(None);
        }

        // Show the best match and ask y / n / c.
        let best = Self::format_result(&results[0]);
        println!("[sync] Best MAL match: {best}");
        println!("       y = accept  |  n = skip  |  c = choose / search again");

        // Read a single character from stdin.
        let choice: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("[sync] Your choice (y/n/c)")
            .validate_with(|s: &String| -> std::result::Result<(), &str> {
                match s.trim().to_lowercase().as_str() {
                    "y" | "n" | "c" => Ok(()),
                    _ => Err("Please enter y, n, or c"),
                }
            })
            .interact_text()?;

        match choice.trim().to_lowercase().as_str() {
            "y" => return Ok(Some(results[0].id)),
            "n" => {
                println!("  [sync] Skipping MAL sync for this show.");
                return Ok(None);
            }
            _ => {} // "c" — fall through to correction mode
        }

        loop {
            let mut items: Vec<String> = results.iter().map(Self::format_result).collect();
            items.push("🔍  Search again (new query)".to_string());
            items.push("🔢  Enter MAL ID directly".to_string());
            items.push("⏭   Skip (no MAL sync for this show)".to_string());

            let idx = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("[sync] Select the correct anime or choose an action")
                .items(&items)
                .default(0)
                .interact()?;

            let skip_idx = items.len() - 1;
            let manual_idx = items.len() - 2;
            let search_idx = items.len() - 3;

            if idx == skip_idx {
                println!("  [sync] Skipping MAL sync for this show.");
                return Ok(None);
            }

            if idx == manual_idx {
                let id: String = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("[sync] Enter MAL anime ID (numeric)")
                    .validate_with(|s: &String| -> std::result::Result<(), &str> {
                        match s.trim().parse::<u32>() {
                            Ok(n) if n > 0 => Ok(()),
                            _ => Err("Please enter a valid non-zero MAL ID"),
                        }
                    })
                    .interact_text()?;
                return Ok(Some(id.trim().parse::<u32>().unwrap()));
            }

            if idx == search_idx {
                let new_query: String = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("[sync] Enter new search query")
                    .interact_text()?;

                let new_results = self.search_mal(new_query.trim()).await;
                if new_results.is_empty() {
                    println!(
                        "  [sync] No results found for \"{new_query}\". Try a different query."
                    );
                } else {
                    results = new_results;
                }
                continue;
            }

            // User picked one of the actual results.
            return Ok(Some(results[idx].id));
        }
    }

    /// Update MAL anime list status.
    async fn update_list_status(&self, mal_id: u32, update: &SyncUpdate) -> Result<()> {
        let total = update
            .total_episodes
            .map_or_else(|| "?".to_string(), |n| n.to_string());
        println!(
            "[sync] Posting update for \"{}\" — ep {}/{} ({})",
            update.title,
            update.episode,
            total,
            update.status.label()
        );

        let mut form: HashMap<&str, String> = HashMap::new();
        form.insert("status", update.status.as_str().to_string());
        form.insert("num_watched_episodes", update.episode.to_string());
        if let Some(ref d) = update.start_date {
            form.insert("start_date", d.clone());
        }
        if let Some(ref d) = update.finish_date {
            form.insert("finish_date", d.clone());
        }
        if let Some(score) = update.score {
            form.insert("score", score.to_string());
        }

        self.http
            .patch(format!("{MAL_API_BASE}/anime/{mal_id}/my_list_status"))
            .bearer_auth(&self.token.access_token)
            .form(&form)
            .send()
            .await
            .context("MAL update list status request failed")?
            .error_for_status()
            .context("MAL returned error on list status update")?;

        Ok(())
    }
}

impl SyncProvider for MalClient {
    async fn sync_episode(&self, show_id: &str, show_title: &str, ep_num: u32) -> Result<()> {
        self.do_sync_episode(show_id, show_title, ep_num).await
    }
}

impl MalClient {
    /// Fetch the anime's list status and planned episode count in one request.
    ///
    /// `anime_info.num_episodes == 0` means MAL doesn't know the total yet
    /// (still airing or not yet announced), so callers must not use it to
    /// infer completion.
    pub async fn get_anime_info(&self, mal_id: u32) -> Result<AnimeInfo> {
        #[derive(Deserialize)]
        struct AnimeWithStatus {
            my_list_status: Option<CurrentListStatus>,
            #[serde(default)]
            num_episodes: u32,
        }

        let resp = self
            .http
            .get(format!("{MAL_API_BASE}/anime/{mal_id}"))
            .bearer_auth(&self.token.access_token)
            .query(&[("fields", "my_list_status,num_episodes")])
            .send()
            .await
            .context("MAL get anime info request failed")?
            .error_for_status()
            .context("MAL returned error fetching anime info")?
            .json::<AnimeWithStatus>()
            .await
            .context("failed to parse MAL anime info response")?;

        Ok(AnimeInfo {
            list_status: resp.my_list_status,
            num_episodes: resp.num_episodes,
        })
    }

    /// Full sync flow for one episode. Resolves the MAL ID (from internal cache
    /// or via search + user confirmation), checks current remote state, skips
    /// if MAL already tracks at least this episode, prompts the user when the
    /// status is changing, and posts the patch request.
    async fn do_sync_episode(&self, show_id: &str, show_title: &str, ep_num: u32) -> Result<()> {
        // 1. Resolve MAL ID — check internal cache first.
        if self.skipped_ids.lock().unwrap().contains(show_id) {
            return Ok(()); // user already declined this show this session
        }
        let cached_id = self.id_cache.lock().unwrap().get(show_id);
        let mal_id = if let Some(id) = cached_id {
            id
        } else {
            match self.resolve_and_confirm_mal_id(show_title).await {
                Ok(Some(id)) => {
                    if let Err(err) = self.id_cache.lock().unwrap().insert_and_save(show_id, id) {
                        eprintln!("[sync] Warning: could not save ID cache: {err}");
                    }
                    id
                }
                Ok(None) => {
                    self.skipped_ids.lock().unwrap().insert(show_id.to_string());
                    return Ok(());
                }
                Err(err) => {
                    eprintln!("[sync] MAL ID resolution failed: {err}");
                    return Ok(());
                }
            }
        };

        // 2. Fetch current remote state.
        let anime_info = self.get_anime_info(mal_id).await.unwrap_or_else(|err| {
            eprintln!("[sync] Warning: could not fetch anime info ({err}), assuming Watching.");
            AnimeInfo {
                list_status: None,
                num_episodes: 0,
            }
        });
        let current = anime_info.list_status;

        // 3. Skip if MAL already records at least this episode.
        if let Some(ref status) = current {
            if status.num_episodes_watched >= ep_num {
                println!(
                    "[sync] MAL already at ep {} for \"{}\" — skipping update.",
                    status.num_episodes_watched, show_title
                );
                return Ok(());
            }
        }

        // 4. Determine new watch status.
        //    Never auto-complete when MAL doesn't know the total (num_episodes == 0).
        let new_status = if anime_info.num_episodes > 0 && ep_num >= anime_info.num_episodes {
            WatchStatus::Completed
        } else {
            WatchStatus::Watching
        };

        // 5. Confirm with the user if the status is changing.
        let needs_confirm = should_confirm_sync(&current, new_status);
        let should_update = if needs_confirm {
            Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt(format!(
                    "[sync] Update MAL: \"{}\" ep {} → {}?",
                    show_title,
                    ep_num,
                    new_status.label()
                ))
                .default(false)
                .interact()
                .unwrap_or(false)
        } else {
            true
        };

        if !should_update {
            println!("[sync] Skipped MAL update for \"{}\".", show_title);
            return Ok(());
        }

        // 6. Build dates.
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let is_first_start = new_status == WatchStatus::Watching
            && match &current {
                None => true,
                Some(cur) => cur.status == "plan_to_watch",
            };
        let start_date = if is_first_start {
            Some(today.clone())
        } else {
            None
        };
        let finish_date = if new_status == WatchStatus::Completed {
            Some(today)
        } else {
            None
        };

        // 7. Prompt for a score when completing.
        let score: Option<u8> = if new_status == WatchStatus::Completed {
            let mut opts: Vec<String> = (1u8..=10).map(|n| format!("{}/10", n)).collect();
            opts.push("Skip (no rating)".to_string());
            let idx = Select::with_theme(&ColorfulTheme::default())
                .with_prompt(format!("Rate \"{}\" on MAL (Esc to skip)", show_title))
                .items(&opts)
                .default(opts.len() - 1)
                .interact_opt()?;
            match idx {
                Some(i) if i < 10 => Some(i as u8 + 1),
                _ => None,
            }
        } else {
            None
        };

        // 8. Post the update.
        let update = SyncUpdate {
            title: show_title.to_string(),
            episode: ep_num,
            total_episodes: if anime_info.num_episodes > 0 {
                Some(anime_info.num_episodes)
            } else {
                None
            },
            status: new_status,
            start_date,
            finish_date,
            score,
        };

        match self.update_list_status(mal_id, &update).await {
            Ok(()) => {
                if needs_confirm {
                    println!(
                        "[sync] MAL updated: \"{}\" ep {} → {}",
                        show_title,
                        ep_num,
                        new_status.label()
                    );
                } else {
                    println!(
                        "[sync] MAL progress saved: \"{}\" ep {}",
                        show_title, ep_num
                    );
                }
                if let Some(score_val) = score {
                    println!("[sync] MAL score submitted: {}/10", score_val);
                } else if new_status == WatchStatus::Completed {
                    println!("[sync] Rating skipped.");
                }
            }
            Err(err) => eprintln!("[sync] Failed to update MAL: {err}"),
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct CurrentListStatus {
    pub status: String,
    pub num_episodes_watched: u32,
}

/// Combined per-anime info returned by [`MalClient::get_anime_info`].
pub struct AnimeInfo {
    /// The user's current list entry, or `None` if not on their list.
    pub list_status: Option<CurrentListStatus>,
    /// MAL's planned total episode count. `0` means unknown / still airing.
    pub num_episodes: u32,
}

/// Build a MalClient only when sync is enabled and a stored token exists.
pub async fn build_mal_client_if_enabled(cfg: &AppConfig) -> Option<MalClient> {
    if !cfg.sync.enabled {
        return None;
    }
    if cfg.mal.client_id.is_empty() {
        eprintln!("[sync] mal.client_id is not set in config — sync disabled.");
        return None;
    }
    match MalToken::load() {
        Ok(Some(token)) => match MalClient::from_token(cfg.mal.client_id.clone(), token).await {
            Ok(client) => Some(client),
            Err(err) => {
                eprintln!("[sync] Failed to initialize MAL client: {err}");
                None
            }
        },
        Ok(None) => {
            eprintln!(
                "[sync] Sync is enabled but no MAL token found. Run `anv sync enable mal` first."
            );
            None
        }
        Err(err) => {
            eprintln!("[sync] Failed to load MAL token: {err}");
            None
        }
    }
}
