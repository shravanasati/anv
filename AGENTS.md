# anv — Agent / Contributor Rules

This file documents the conventions and requirements that **must** be followed
when adding a new provider to the `anv` codebase.
It is derived from a full analysis of every existing anime provider
(`anidb`, `animehub`, `anineko`, `senshi`) and all the
systems that interact with them (history, MAL sync, CLI routing, config,
downloader, etc.).

---

## Adding a New Anime Provider — Complete Checklist

Work through every section below in order. Missing any item will result in a
provider that compiles but silently breaks history resume, MAL sync, the `all`
search mode, download support, or the `watchlist`/`watching` commands.

---

### 1. Provider implementation file (`src/providers/<name>.rs`)

- [ ] Define a `pub const <NAME>_BASE_URL: &str` constant.
- [ ] Define a `pub struct <Name>Client { client: reqwest::Client }` (derive `Debug, Clone`).
- [ ] Implement `<Name>Client::new() -> Result<Self>` that builds a
      `reqwest::Client` with:
  - `USER_AGENT` header (use `crate::providers::USER_AGENT` or define a
    provider-specific one if the site requires a specific browser UA).
  - A `timeout` (15 s is standard).
- [ ] Implement `Default for <Name>Client` that delegates to `new().expect(…)`.
- [ ] Add `dbg_log!` macro calls (from `crate::logger`) to every major async
      step, prefixed with the provider module name, e.g. `dbg_log!("anidb", ...)`
      / `dbg_log!("animehub", ...)`.
- [ ] Implement `AnimeProvider for <Name>Client`:
  - [ ] `search_shows` — return `Vec<ShowInfo>` with correct `id`, `title`,
        `mal_id` (see §4), and `available_eps` (`EpisodeCounts { sub, dub }`).
        If the site doesn't expose episode counts, use `EpisodeCounts::default()`.
  - [ ] `fetch_episodes` — return episode labels as `Vec<String>` (e.g.
        `["1", "2", "3"]`). Labels are displayed directly in the UI and passed
        back verbatim to `fetch_streams`, so they must be consistent.
  - [ ] `fetch_streams` — return `Vec<StreamOption>` sorted best-quality-first
        (`quality_rank` descending). Fill:
    - `provider: String` — a human-readable name (matches `Provider::display_name`).
    - `url` — absolute URL to the stream (HLS master or MP4).
    - `quality_label` — e.g. `"1080p"`, `"720p"`, `"auto"`.
    - `quality_rank: i32` — numeric height (1080, 720, …, 0 for "auto").
    - `is_hls: bool`.
    - `headers: HashMap<String, String>` — include at minimum `Referer` (and
      `User-Agent` if the CDN requires it).
    - `subtitle: Option<String>` — VTT/ASS URL or `None`.
  - [ ] `fetch_mal_id` (optional override, default returns `Ok(None)`):
    - Override this if the provider page contains a link to MAL (e.g. AniDB
      scrapes `myanimelist.net/anime/<id>` from the show detail page).
    - Senshi uses its numeric show ID directly as the MAL ID, so
      `fetch_mal_id` just returns `Ok(Some(show_id.to_string()))`.
    - If the provider has no MAL cross-reference at all, leave the default
      `Ok(None)` — the MAL sync layer will search MAL by title instead.

---

### 2. Register the provider in `src/types.rs`

- [ ] Add a new variant to the `Provider` enum:
  ```rust
  #[serde(alias = "alternative_old_name")] // only if needed for migration
  NewProvider,
  ```
  Keep `#[serde(other)] Unknown` as the last catch-all variant.
- [ ] Add the variant to `Provider::is_anime()`.
- [ ] Add a human-readable name in `Provider::display_name()`.
- [ ] Add it to the history deserialisation test in `src/types.rs` tests if
      you add a serde alias (follow the existing `test_allanime_deserialization_alias`
      pattern).

---

### 3. Declare the module in `src/providers/mod.rs`

- [ ] Add `pub mod <name>;` in alphabetical order.

---

### 4. MAL ID resolution and caching (`src/sync/mal.rs`)

The MAL sync layer maps each provider's internal show ID to a MAL anime ID.
Different providers resolve this differently:

| Provider  | Strategy |
|-----------|----------|
| Senshi    | show ID **is** the MAL ID — no cache bucket needed |
| AniDB     | `fetch_mal_id` scrapes the show page; cached in `anidb_entries` |
| AniNeko   | no page link; cached in `anineko_entries` after user-confirmed search |
| AnimeHub  | no page link; cached in `animehub_entries` after user-confirmed search |

For every **new provider** that does NOT expose the MAL ID natively:

- [ ] Add a new `HashMap<String, u32>` field in `MalIdCache`, e.g.:
  ```rust
  #[serde(default)]
  <name>_entries: HashMap<String, u32>,
  ```
- [ ] Add the provider variant in `MalIdCache::get()`.
- [ ] Add the provider variant in `MalIdCache::get_cached_id()`.
- [ ] Add the provider variant in `MalIdCache::insert_and_save()`.

If the provider returns MAL IDs natively (like Senshi):
- [ ] Add a comment in `MalIdCache::get()` explaining why no bucket is needed.
- [ ] The `_` arm in `get/get_cached_id/insert_and_save` already silently ignores it.

---

### 5. CLI routing in `src/cmd/anime.rs`

There are **two** routing sites — both must be updated:

#### 5a. Search + play path (`run_anime_flow`, direct query branch)

- [ ] Add a `Provider::<Name>` arm in the `match provider { … }` block that:
  1. Constructs `<Name>Client::new()?`.
  2. Calls `client.search_shows(&query, translation).await?`.
  3. Calls `select_show(…)`.
  4. Calls `play_show(…, Provider::<Name>, …)`.

#### 5b. History-resume path (`run_anime_flow`, `history_mode` branch)

- [ ] Add a `Provider::<Name>` arm in the inner `match target_provider { … }` block that:
  1. Constructs `<Name>Client::new()?`.
  2. Re-uses the stored `show_id` / `show_title` if the provider matches (avoids
     an extra network search), otherwise calls `resolve_show_info(…)`.
  3. Passes the correct episode (`prefer_episode` vs. `override_last_watched`)
     depending on `auto_play_next`.
  4. Calls `play_show(…, Provider::<Name>, …)`.

#### 5c. `Provider::All` search aggregation

- [ ] Instantiate `let <name>_client = <Name>Client::new().ok();` alongside the other
      `all` clients.
- [ ] Add a `tokio::join!` branch to search the new provider concurrently.
- [ ] Append the results with `(Provider::<Name>, show)` into `combined`.
- [ ] Add a `Provider::<Name>` arm to the `match selected_provider { … }` block that
      unwraps the client and calls `play_show(…)`.

---

### 6. `play_show` — special cases

`play_show` in `src/cmd/anime.rs` already handles every provider generically via
the `AnimeProvider` trait, but there are two provider-specific branches:

- [ ] **Quality selection**: `play_show` uses `select_stream_by_quality(streams, config.quality)`
      generically across all providers using the global `config.quality` setting.
- [ ] **MAL ID lazy-fetch**: `play_show` calls `client.fetch_mal_id(&show.id)`
      when `show.mal_id.is_none()` before entering the playback loop (needed for
      aniskip chapter markers). No code change needed here — handled generically
      through the `AnimeProvider` trait default.

---

### 7. Config (`src/config.rs`) — only if the provider needs settings

- [ ] Add a `pub struct <Name>Config { … }` and a `Default` impl.
- [ ] Add `#[serde(default)] pub <name>: <Name>Config` to `AppConfig`.
- [ ] Update `AppConfig::default()` and the `CONFIG_HEADER` doc comment.
- [ ] Pass the config into `run_anime_flow` / `play_show` as needed.
- [ ] Document the new settings in `README.md`.

---

### 8. History compatibility

The `HistoryEntry` struct stores the `Provider` enum via serde.
No changes are needed to `src/history.rs` itself, but:

- [ ] Ensure the new `Provider` variant serialises/deserialises correctly
      (checked automatically by the `serde(rename_all = "lowercase")` on the enum).
- [ ] If the provider replaces a deprecated one, add a `#[serde(alias = "…")]`
      attribute to handle existing history files that contain the old name
      (see the `allanime → anidb` migration for the pattern).
- [ ] Write (or update) the `test_deserialize_history_with_unknown_provider` style
      test in `src/types.rs` tests.

---

### 9. Watchlist / Watching commands (`src/cmd/sync.rs`)

These commands pull the user's MAL list, present titles, and then look up the
provider's show ID using `MalIdCache::get_cached_id`.

- [ ] Ensure `MalIdCache::get_cached_id` handles the new provider variant (done if
      you followed §4).
- [ ] In `src/cmd/sync.rs`, find the function that constructs a client and calls
      `play_show` after a MAL watchlist entry is selected, and add a
      `Provider::<Name>` arm there. Follow the existing Senshi / AniDB / AniNeko /
      AnimeHub pattern exactly.

---

### 10. Download support

`play_show` already calls `crate::downloader::download_episode(…)` generically
when `--download` / `-D` is passed. Stream quality selection for downloads
always uses `AnidbQuality::Highest` for non-AniDB providers.

- [ ] No code changes needed unless the new provider requires special stream
      headers or authentication that `download_episode` does not support.
- [ ] Verify that `StreamOption::headers` is populated correctly (Referer, UA)
      so the downloader (ffmpeg/yt-dlp) can access the stream.

---

### 11. Translation / sub/dub handling

- [ ] Make sure `search_shows` ignores `_translation` or uses it appropriately
      (some providers like AnimeHub track sub/dub separately, others like AniNeko
      and AniDB handle it at the stream level).
- [ ] Make sure `fetch_episodes` is consistent — if sub/dub share the same episode
      list (most providers), `_translation` can be ignored. If they differ
      (AnimeHub appends `-dub` to the slug), handle it there.
- [ ] In `fetch_streams`, map `Translation::Dub` / `Translation::Sub` /
      `Translation::Raw` to the correct language code or stream endpoint.
      If the provider doesn't support a given translation, return an empty `Vec`
      and let the caller (`play_show`) emit the error.

---

### 12. Error handling conventions

- [ ] Use `bail!(…)` for user-facing "no results" / "no streams" situations.
- [ ] Use `anyhow!` / `.context(…)` for unexpected API/parse failures.
- [ ] Log debug info via `dbg_log!` (not `println!` or `eprintln!`).
- [ ] All network requests must use `.error_for_status()?` or equivalent.
- [ ] Retry logic is optional but recommended for flaky providers (see
      `AnimehubClient::fetch_string_with_retry` for the pattern).

---

### 13. Tests

- [ ] Add at minimum a `#[cfg(test)] mod tests` block that unit-tests any
      non-trivial parsing logic (regex extraction, JSON parsing, slug manipulation,
      HTML scraping). No live network calls in unit tests.
- [ ] The pattern used by AniNeko (`test_extract_lang_embed_urls`,
      `test_strip_png_wrapper`) and AnimeHub (`test_parse_sources_div_json`,
      `test_dub_stripping_and_link_normalization`) is the standard.

---

### 14. README update

- [ ] Add the new provider to the provider table in `README.md`.
- [ ] List any known limitations (no dub, no subtitles, geo-restricted, etc.).

---

## Summary of touch-points for a new anime provider

| File | What to change |
|------|----------------|
| `src/providers/<name>.rs` | **Create**; implement `AnimeProvider` trait |
| `src/providers/mod.rs` | Add `pub mod <name>;` |
| `src/types.rs` | Add `Provider::<Name>` variant; update `is_anime`, `display_name` |
| `src/sync/mal.rs` | Add `<name>_entries` HashMap bucket in `MalIdCache`; update `get`, `get_cached_id`, `insert_and_save` |
| `src/cmd/anime.rs` | Add arm in: search path, history-resume path, and `Provider::All` aggregation |
| `src/cmd/sync.rs` | Add arm for watchlist/watching commands |
| `src/config.rs` | Add `<Name>Config` section if provider needs configurable settings |
| `README.md` | Document the new provider |

---

## Senshi special note — "show ID is MAL ID" pattern

Senshi is unique: its numeric show IDs are directly MAL anime IDs.
- `search_shows` sets `mal_id: Some(item.id.to_string())` on every result.
- `fetch_mal_id` returns `Ok(Some(show_id.to_string()))` (trivial parse).
- `MalIdCache` has no bucket for Senshi — `get/get_cached_id/insert_and_save`
  all hit the `_ => None / {}` arms.

If a future provider similarly exposes MAL IDs natively, follow the same pattern
rather than adding a needless cache bucket.

---

## AniNeko special note — local HLS proxy pattern

AniNeko (`anineko.rs`) implements a full local TCP proxy (`VibeProxy`) to handle
video segments wrapped in PNG headers. If a new provider uses a similarly
obfuscated transport:

- Keep proxy logic inside the provider file.
- Use a `OnceLock<Arc<Proxy>>` singleton so the proxy starts once per process.
- The proxy must register a session and return a `http://127.0.0.1:<port>/…`
  URL that is placed into `StreamOption::url` — the media player talks to the
  proxy, not the original CDN directly.
