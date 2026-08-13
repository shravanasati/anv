# REFACTOR.md

Technical debt audit of the `anv` codebase, categorized by type. Each item lists
the problem, why it matters, and a proposed solution. Work through sections in
order (docs → arch → code). Most items are independent and can be done alongside
feature work.

## 1. Docs & Dependency Debt

Finished.

## 2. Architecture Debt

### 2.1 MAL-ID resolution is triplicated and the resolved ID is dropped
- **Files:** `src/cmd/anime.rs:404,589-591`, `src/sync/mal.rs:807-832`, `src/cmd/sync.rs:218,317`
- **Problem:** `play_show` resolves `show.mal_id` via `fetch_mal_id()` (for
  aniskip) but then calls `sync_episode(&show.id, ...)` **without passing the
  resolved ID**. `do_sync_episode` re-resolves via cache bucket → MAL title
  search → interactive confirm. Senshi's native "show ID == MAL ID" property is
  wasted here; every Senshi sync re-prompts the user. AniDB double-resolves.
- **Why it matters:** The #1 user-facing friction: spurious confirmation prompts
  during sync, and aniskip/sync can disagree on the episode's MAL ID.
- **Solution:** Thread `Option<u32> mal_id` through `SyncProvider::sync_episode` /
  `do_sync_episode`. Trust it when `Some`, fall back to cache/search only when
  `None`. Delete the `Provider::Senshi` special case in `cmd/anime.rs:94-98`.

### 2.2 "Search all providers" aggregation triplicated
- **Files:** `src/cmd/anime.rs:146-249`, `src/cmd/manga.rs:47-116`, `src/cmd/sync.rs:101-216`
- **Problem:** Three copies of: construct clients with `.ok()`, `tokio::join!`
  the searches, merge `Vec<(Provider, ShowInfo)>`, select. sync.rs re-implements
  `search_opt_with_timeout` inline with **worse** error handling
  (`.unwrap_or_default()` on every search).
- **Why it matters:** Adding a provider requires editing all three sites (the
  exact failure the AGENTS.md checklist warns about); behavior already diverged.
- **Solution:** One `async fn aggregate_search(providers, query, ...) ->
  Result<Vec<(Provider, ShowInfo)>>` used by all three call sites, with the
  timeout-warning heuristic (anime.rs:196-206 / manga.rs:73-83) folded in.

### 2.3 `play_show` and `read_manga` are the same playback loop, twice
- **Files:** `src/cmd/anime.rs:345-606`, `src/cmd/manga.rs:204-414`
- **Problem:** Both implement: build sorted labels → resolve preferred
  episode/chapter → `loop { select / fetch / launch / history.upsert+save /
  advance via next_candidate }`. The advance-match blocks are line-for-line
  identical.
- **Why it matters:** The single biggest de-duplication win in the codebase;
  also the reason the 2.1 `mal_id` drop is easy to miss (260-line function).
- **Solution:** Extract a generic media-loop helper parameterized over a
  `Episode`/`Chapter`-like trait, or at minimum split `play_show` into
  `select_episode_and_play` + `play_one_episode`.

### 2.4 `run_mal_list` is a 322-line monolith
- **Files:** `src/cmd/sync.rs:17-338`
- **Problem:** One function contains list-fetch, a `loop`, a re-implemented
  "search all providers" block, a cached-ID path, a single-provider search loop,
  and three near-identical 14-arg `play_show(...)` call blocks (197-213,
  227-243, 320-336).
- **Why it matters:** Every new provider or play_show signature change touches a
  monster function; high regression risk.
- **Solution:** Extract `fn play_with(...)` and a single "resolve provider show
  for MAL entry" function returning `(ShowInfo, Provider)`.

### 2.5 Watchlist / Watching handlers are copy-paste twins
- **Files:** `src/main.rs:250-297` vs `src/main.rs:299-346`
- **Problem:** Identical 48-line blocks differing only in
  `"plan_to_watch"`/`"Plan to Watch"` vs `"watching"`/`"Watching"`. The
  `binge` merge and dub/translation ternaries are duplicated.
- **Why it matters:** Fixing a UX bug in one risks missing the other.
- **Solution:** One handler taking `(list_type, list_name)`; compute merged
  flags once before the subcommand match.

### 2.6 MalIdCache: three methods with identical per-provider match arms
- **Files:** `src/sync/mal.rs:115-123,127-137,139-167`
- **Problem:** `get`/`get_cached_id`/`insert_and_save` each repeat the same
  3-arm match over `anidb_entries`/`anineko_entries`/`animehub_entries`. Adding
  a provider means editing 3 methods + 3 fields + doc comment. Senshi's
  "no bucket needed" case is a comment, not enforced.
- **Why it matters:** Structural cause of 2.1; high chance of a missed arm.
- **Solution:** One private `bucket(&self, p)`/`bucket_mut` accessor pair (or a
  single `HashMap<Provider, HashMap<String,u32>>`), shrinking each method to ~3
  lines.

### 2.7 Skip-type definition quintuplicated in aniskip
- **Files:** `src/aniskip.rs:16-26,63-70,165-172,192-198,223-252`
- **Problem:** The 5 skip types (`op`, `ed`, `mixed-op`, `mixed-ed`, `recap`)
  are encoded independently in the Lua script opts, `SkipTimes` fields, the
  response match, `SkipOptions`, and the arg-building if-blocks.
- **Why it matters:** Adding a skip type (e.g. "preview") touches 5 places.
- **Solution:** Drive arg-building and response mapping from a single
  `const SKIP_TYPES: [(&str, ...); 5]` table.

### 2.8 No cache expiry / invalidation anywhere
- **Files:** `src/sync/mal.rs:84-96`, `src/aniskip.rs:103-126`
- **Problem:** `MalIdCache` entries and `SkipCache` entries are inserted and
  never evicted. No timestamps, no TTL, no size cap.
- **Why it matters:** Unbounded growth; stale `show_id → mal_id` mappings outlive
  provider re-slugging and poison sync/aniskip.
- **Solution:** Add an `inserted_at` timestamp and prune entries older than N
  days on load; cap size.

### 2.9 VibeProxy singleton and unbounded session map
- **Files:** `src/providers/anineko.rs:449,488-500`
- **Problem:** `VIBE_PROXY: OnceLock<Arc<VibeProxy>>` can't be torn down, and
  `register()` inserts into `sessions: Mutex<HashMap<...>>` with no removal path.
- **Why it matters:** Unbounded memory growth on long sessions.
- **Solution:** Store an `Instant` per session and prune on `register`/`handle_connection`.

### 2.10 Dead `all_anime()` / `all_manga()` while cmd hardcodes the same lists
- **Files:** `src/providers/mod.rs:141-152`, `src/cmd/anime.rs:147-150`, `src/cmd/manga.rs:48-49`
- **Problem:** The provider-list helpers are never called; the `Provider::All`
  branches hardcode the same lists.
- **Why it matters:** Provider list lives in two places; 2.2's aggregation helper
  should consume these instead.
- **Solution:** Consume them in the shared aggregation (2.2), or delete.

## 3. Code Debt

### 3.1 AniSkip HTTP: no timeout, no error_for_status, no retry, per-call client
- **Files:** `src/aniskip.rs:157-158`
- **Problem:** `reqwest::Client::builder().user_agent("anv").build()?` has no
  timeout (reqwest default = none), no `.error_for_status()?`, no retry, and the
  client is rebuilt per call.
- **Why it matters:** A hung AniSkip API blocks `launch_player` indefinitely; a
  4xx/5xx surfaces as a confusing JSON-parse error.
- **Solution:** Cache the client in a `OnceLock`, set
  `.timeout(Duration::from_secs(15))`, use the shared browser `USER_AGENT`, add
  `.error_for_status()?`, and reuse the retry pattern from 3.10.

### 3.2 Mangapill client has no HTTP timeout
- **Files:** `src/providers/mangapill.rs:18-21`
- **Problem:** The reqwest client omits `.timeout()`; every other provider uses
  15s (anidb/animehub/anineko/senshi) or 30s (mangadex).
- **Why it matters:** A hung origin hangs the whole search command forever.
- **Solution:** Add `build_client(user_agent, timeout_secs)` in
  `src/providers/mod.rs` and use it in all six providers (fixes ordering
  inconsistencies too).

### 3.3 AniSkip episode semantics wrong for cumulative labels
- **Files:** `src/aniskip.rs:134-136`, `src/cmd/anime.rs:584-591`
- **Problem:** `fetch_skip_times(mid, episode)` uses the raw provider episode
  label, but labels can be cumulative (e.g. `"30"` = ep 6 of season 4). The sync
  path already converts to a 1-based index; aniskip doesn't, so it can fetch
  skip-times for the wrong episode and cache under the wrong key
  (`"{mal_id}_{label}"`).
- **Why it matters:** Wrong intro/recap skipping on multi-season shows.
- **Solution:** Pass the 1-based `ep_num` computed in `play_show` into
  `prepare_aniskip_args`/`fetch_skip_times`.

### 3.4 `Translation::Raw`/Dub semantics diverge across providers
- **Files:** `src/providers/anidb.rs:276-279`, `animehub.rs:223-225,284-286`,
  `anineko.rs:171-189`, `senshi.rs:355-358,430-436`, `mangadex.rs:131-135`
- **Problem:** Raw falls through `_` arms to Sub in every anime provider; AniDB
  maps Dub→"eng" language code, AnimeHub→"-dub" slug, Senshi→status flag,
  AniNeko→dub group. Mangadex correctly bails on Dub but everyone else silently
  degrades. Error messages differ (`translation.as_str()` vs `.label()`).
- **Why it matters:** Inconsistent, sometimes wrong behavior for sub/dub/raw with
  no shared contract.
- **Solution:** Define one shared translation→(lang code, endpoint) mapping in
  `mod.rs`; decide once whether Raw = Sub for anime or returns an explicit error.

### 3.5 Error swallowing
- **Files:** `src/providers/anidb.rs:320-328`, `src/cmd/sync.rs:121-151`,
  `src/cmd/anime.rs:147-150,404-407`, `src/cmd/manga.rs:48-49`
- **Problem:** (a) m3u8 body read `.unwrap_or_default()` silently yields `""` →
  degraded "auto" stream, no error. (b) sync.rs `Provider::All` search
  `.unwrap_or_default()` discards real failures → misleading "No results".
  (c) `AnidbClient::new().ok()` — if all clients fail, user sees only "No
  results". (d) `fetch_mal_id` `Err` dropped silently (aniskip just skipped).
- **Why it matters:** Real failures masquerade as "no results" / degraded playback.
- **Solution:** Propagate errors (`.context(...)`/`bail!`), collect client-init
  failures and print a summary, log `fetch_mal_id` errors via `dbg_log!`.

### 3.6 HLS master-playlist parsing copy-pasted (and already diverged)
- **Files:** `src/providers/anidb.rs:331-395`, `animehub.rs:386-440`, `anineko.rs:345-395`
- **Problem:** ~65-line block (fetch playlist → parse `#EXT-X-STREAM-INF` →
  `Url::join` variants → quality label → headers → fallback to master → sort)
  duplicated in anidb/animehub and functionally again in anineko. Diverged:
  animehub labels the direct-master fallback `"1080p"/rank 1080` (a lie) while
  anidb/anineko use `"auto"/0`.
- **Why it matters:** Bug fixes must be applied in 3 places; the fallback label
  lie misleads quality selection.
- **Solution:** `fn parse_hls_master(master_url, body, referer, provider) ->
  Vec<StreamOption>` in `mod.rs`, plus a shared quality-label→rank mapping;
  anineko's NAME-based variant becomes a wrapper.

### 3.7 `write_http_response` duplicates proxy.rs and emits malformed reason phrases
- **Files:** `src/providers/anineko.rs:693-708`, `src/proxy.rs:154-215`
- **Problem:** anineko always writes `"HTTP/1.1 {status} OK"` — so 404/502/400
  responses are emitted as `404 OK` (malformed). `proxy.rs` has a proper
  reason-phrase map.
- **Why it matters:** Strict HTTP clients reject the malformed responses.
- **Solution:** Shared `write_http_response(status, reason, content_type, body)`
  with a correct reason-phrase map, used by both.

### 3.8 Debug-logging has 3 competing conventions
- **Files:** `src/providers/anidb.rs:14-20`, `animehub.rs:15-21`, `senshi.rs:34,83,210,284,326`,
  `anineko.rs`, `src/player.rs:90,132-134`, `src/aniskip.rs:137-151`
- **Problem:** `dbg_log!` macro copy-pasted in 2 files; Senshi repeats
  `std::env::var("ANV_DEBUG").is_ok()` + `eprintln!` ~7 times; player/aniskip
  inline-gate; anineko/mangadex/mangapill log nothing.
- **Why it matters:** AGENTS.md mandates `dbg_log!` everywhere; the style drift
  makes debugging harder.
- **Solution:** Single shared `dbg_log!("<provider>", ...)` in `mod.rs` (or a
  `pub(crate) fn dbg_log(prefix, args)`); migrate senshi/anineko/player/aniskip.

### 3.9 Fragile / unanchored regexes
- **Files:** `src/providers/anidb.rs:412`, `anineko.rs:346,408`, `animehub.rs:332`
- **Problem:** (a) `myanimelist\.net/anime/([0-9]+)` matches the first MAL link
  anywhere on the page (comment, related-anime block) → wrong MAL ID for
  aniskip/sync. (b) `#EXT-X-STREAM-INF.*NAME="..."` assumes attribute order;
  breaks silently → zero streams. (c) vibeplayer path regex hardcodes one URL
  shape; a path change breaks it. (d) `var zrpart2` regex unanchored to scope.
- **Why it matters:** Silent mis-identification / stream loss on page-layout changes.
- **Solution:** Anchor patterns to DOM context; prefer the `RESOLUTION=` HLS
  parse (3.6) over NAME-based; `regex::escape` any URL built from constants.

### 3.10 Retry logic exists in only 2 of 6 providers, incompatible
- **Files:** `src/providers/animehub.rs:38-70`, `mangadex.rs:45-70`
- **Problem:** animehub: 5 attempts/2s, retries server errors; mangadex: 3
  attempts, only on 429. Others have none.
- **Why it matters:** Flaky-provider behavior differs per provider; unify so
  fixes (e.g. 3.1) share one implementation.
- **Solution:** Generic `fetch_with_retry(client, builder, attempts, backoff,
  retryable_predicate)` in `mod.rs`.

### 3.11 player.rs header→mpv-arg mapping duplicated
- **Files:** `src/player.rs:114-123` vs `src/player.rs:241-252`
- **Problem:** Identical `user-agent`/`referer`/`http-header-fields` translation
  (incl. the subtle referer double-arg special case).
- **Why it matters:** Header fixes must be applied twice.
- **Solution:** `fn apply_header_args(cmd, headers)`.

### 3.12 Downloader: three functions 95% identical
- **Files:** `src/downloader.rs:228-280,282-314,316-350`
- **Problem:** `download_with_ffmpeg`/`download_with_ytdlp`/
  `download_with_ytdlp_aria2c` share the status-check → `NotFound` bail →
  cleanup epilogue; the ytdlp variants differ only in 2 args.
- **Why it matters:** Fixing the epilogue (e.g. partial-file cleanup) requires
  triple edits.
- **Solution:** One `run_downloader(bin, args, hint)` for spawn/status/cleanup;
  build the arg list per engine.

### 3.13 Regexes recompiled on every request
- **Files:** `src/providers/anidb.rs:99-102,147-154,300-301,335,412`,
  `animehub.rs:119,332,389`, `anineko.rs:133,217,227-228,294,301,328,346,408`
- **Problem:** `Regex::new` called inside per-request hot paths.
- **Why it matters:** Wasted CPU per search/stream fetch.
- **Solution:** `std::sync::LazyLock<Regex>` (rust-version 1.85 supports it).

### 3.14 Magic numbers / hardcoded literals
- **Files:** `src/providers/animehub.rs:300,341,97-105`, `senshi.rs:393`,
  `anineko.rs:356-362`, `src/player.rs:220,234,93-98,194-197,258-268`,
  `src/downloader.rs:244-257,322`, `src/cache.rs:26`, `src/sync/mal.rs:487,721,277,335`
- **Problem:** `server=0`, `?pl_usn=1`, `.min(2)` page cap, quality-rank `1000`
  with `"Auto"` (vs lowercase `"auto"` elsewhere), mpv exit code `2`, ffmpeg
  arg soup, aria2c `-x 16`, 30s cache timeout, MAL `limit=5`/`limit=100`,
  duplicated `Duration::from_secs(30)`.
- **Why it matters:** Undocumented values; quality-label/rank tables already
  inconsistent across providers.
- **Solution:** Named consts (`MPV_EXIT_QUIT`, `MAL_API_LIMIT`,
  `CACHE_TIMEOUT_SECS`, `AUTO_QUALITY_RANK`); unify the auto label/rank via 3.6.

### 3.15 String slicing / format-assumption parsing
- **Files:** `src/providers/animehub.rs:146-157,266-271`, `mangapill.rs:98`
- **Problem:** `truncate(len - 4)` / `- 6` byte-slicing on user-visible strings;
  `rsplit('/').next()` assumes `<slug>/<n>`; `text.replace("Chapter ", "")`
  assumes the label format verbatim.
- **Why it matters:** Breaks on non-ASCII slugs / label renames; silent misparse.
- **Solution:** Regex-based extraction (e.g. `chapter\s*([\d.]+)`), or a shared
  tested helper for suffix stripping.

### 3.16 `.expect()` selector panics vs error propagation
- **Files:** `src/providers/mangapill.rs:46-47,91,133` vs `animehub.rs:96-117`
- **Problem:** mangapill does `Selector::parse(...).expect("valid CSS selector")`
  (panic on a typo); animehub returns `anyhow!` errors for the same call.
- **Why it matters:** Two failure styles for the identical fallible operation;
  a typo'd selector crashes the process.
- **Solution:** Shared fallible `parse_selector(&str) -> Result<Selector>`.

### 3.17 StreamOption::provider values inconsistent
- **Files:** `src/providers/anidb.rs:366` (`"AniDB"`), `animehub.rs:415,430`
  (`"animehub"` lowercase), `senshi.rs:390` (`"Senshi"`), `anineko.rs:368,423`
  (`"bibiemb"`/`"vibeplayer"` — the embed host, not the provider).
- **Problem:** Displayed directly in `StreamOption::label()` (`types.rs:105-108`);
  users see a third-party embed host for AniNeko and mixed casing elsewhere.
- **Why it matters:** Inconsistent UX; AGENTS.md says it must match
  `Provider::display_name()`.
- **Solution:** Use `Provider::display_name()` everywhere; embed host can be
  appended to the URL/label if needed.

### 3.18 Intra-file duplication in animehub
- **Files:** `src/providers/animehub.rs:222-237` vs `283-298`
- **Problem:** Identical 15-line block (dub-slug suffix, URL build, slug
  extraction) in `fetch_episodes` and `fetch_streams`.
- **Why it matters:** Slug fixes must be applied twice.
- **Solution:** `build_show_url(identifier, translation)` + `slug_from_identifier`.

### 3.19 Manga chapter sorting duplicates utils.rs
- **Files:** `src/providers/mangadex.rs:158-165`, `mangapill.rs:105-112`,
  `src/utils.rs:6-19`
- **Problem:** Both providers reimplement parse-numeric-label + sort + dedup,
  while `utils.rs` already exports `parse_episode_key`/`sorted_episode_labels`.
- **Why it matters:** Sort semantics can drift between providers and the cmd layer.
- **Solution:** Reuse `crate::utils` helpers.

### 3.20 Remaining small duplications
- **Files:** `src/cmd/anime.rs:37-43` vs `src/cmd/sync.rs:52-58` (SkipOptions
  build); `src/cmd/anime.rs:425-443` vs `452-479` (label-by-index resolution);
  `src/cmd/anime.rs:196-206` vs `src/cmd/manga.rs:73-83` (timeout warning);
  `src/cmd/anime.rs:608-621` vs `623-636` (resolve_show/resolve_manga_info);
  `src/sync/mal.rs:392-423` vs `425-450` (exchange_code/refresh_token_inner)
- **Problem:** Each pair is a near-identical block differing in one detail.
- **Solution:** `impl From<&Cli> for SkipOptions`,
  `resolve_label_by_index(label, sorted)`, fold timeout-warning into 2.2, generic
  `resolve_by_title<T>`, and one `post_token_request(params)`.

### 3.21 Mutex `.unwrap()` in async playback path
- **Files:** `src/sync/mal.rs:777,785,804,807,816,824`
- **Problem:** `std::sync::Mutex` locks `.unwrap()`ed inside an async playback
  loop; a poisoned mutex panics.
- **Why it matters:** Panic mid-playback on any task panic.
- **Solution:** `unwrap_or_else(|e| e.into_inner())` helper or `parking_lot`.

### 3.22 Test & CI debt
- **Files:** 8 of 20 source files have `#[cfg(test)]` (`anidb`, `mangadex`,
  `mangapill`, `mal.rs`, `aniskip`, `cache`, `cmd/*`, `utils` have none);
  `.github/workflows/` contains only the cargo-dist release workflow
- **Problem:** AGENTS.md §13 requires unit tests for parsing logic; most
  providers have none. No CI job runs `cargo clippy -- -D warnings` or
  `cargo test`, so the existing 11 clippy warnings and regressions ship
  silently.
- **Why it matters:** Refactors in this doc have no safety net.
- **Solution:** Add a CI lint+test job; add parsing tests per provider
  (regex/JSON/slug helpers only — no live network).

### 3.23 Vestigial migration shims (intentional — document removal)
- **Files:** `src/sync/mal.rs:81-89` (legacy `entries` bucket with
  `#[allow(dead_code)]`), `src/cmd/sync.rs:374` (`let _token = ...` discard),
  `src/types.rs:122,125` (`serde(alias = "allanime")` / `"123animehub"`)
- **Problem:** Deliberate compatibility shims for old history/cache files.
- **Why it matters:** `#[allow(dead_code)]` is a standing dead-code permit;
  without a removal date they persist forever.
- **Solution:** Keep, but annotate each with the earliest version at which it can
  be deleted; replace `_token` with a `_` binding.
