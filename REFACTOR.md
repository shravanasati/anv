# REFACTOR.md

Technical debt audit of the `anv` codebase, categorized by type. Each item lists
the problem, why it matters, and a proposed solution. Work through sections in
order (docs → arch → code). Most items are independent and can be done alongside
feature work.


## 1. Architecture Debt

### 1.1 [PARTIALLY RESOLVED] No cache expiry / invalidation anywhere
- **Files:** `src/sync/mal.rs:84-96`, `src/aniskip.rs`
- **Status:** `SkipCache` in `src/aniskip.rs` resolved (added `inserted_at` timestamp, 30-day TTL pruning on load, max 1000 entry cap on save, and compact unindented JSON serialization). `MalIdCache` pending.

### 1.2 VibeProxy singleton and unbounded session map
- **Files:** `src/providers/anineko.rs:449,488-500`
- **Problem:** `VIBE_PROXY: OnceLock<Arc<VibeProxy>>` can't be torn down, and
  `register()` inserts into `sessions: Mutex<HashMap<...>>` with no removal path.
- **Why it matters:** Unbounded memory growth on long sessions.
- **Solution:** Store an `Instant` per session and prune on `register`/`handle_connection`.


## 2. Code Debt

### 2.4 `Translation::Raw`/Dub semantics diverge across providers
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

### 2.5 Error swallowing
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

### 2.6 HLS master-playlist parsing copy-pasted (and already diverged)
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

### 2.7 `write_http_response` duplicates proxy.rs and emits malformed reason phrases
- **Files:** `src/providers/anineko.rs:693-708`, `src/proxy.rs:154-215`
- **Problem:** anineko always writes `"HTTP/1.1 {status} OK"` — so 404/502/400
  responses are emitted as `404 OK` (malformed). `proxy.rs` has a proper
  reason-phrase map.
- **Why it matters:** Strict HTTP clients reject the malformed responses.
- **Solution:** Shared `write_http_response(status, reason, content_type, body)`
  with a correct reason-phrase map, used by both.

### 2.9 Fragile / unanchored regexes
- **Files:** `src/providers/anidb.rs:412`, `anineko.rs:346,408`, `animehub.rs:332`
- **Problem:** (a) `myanimelist\.net/anime/([0-9]+)` matches the first MAL link
  anywhere on the page (comment, related-anime block) → wrong MAL ID for
  aniskip/sync. (b) `#EXT-X-STREAM-INF.*NAME="..."` assumes attribute order;
  breaks silently → zero streams. (c) vibeplayer path regex hardcodes one URL
  shape; a path change breaks it. (d) `var zrpart2` regex unanchored to scope.
- **Why it matters:** Silent mis-identification / stream loss on page-layout changes.
- **Solution:** Anchor patterns to DOM context; prefer the `RESOLUTION=` HLS
  parse (2.6) over NAME-based; `regex::escape` any URL built from constants.

### 2.10 Retry logic exists in only 1 of 4 providers, incompatible
- **Files:** `src/providers/animehub.rs:38-70`
- **Problem:** animehub: 5 attempts/2s, retries server errors; others have none.
- **Why it matters:** Flaky-provider behavior differs per provider; unify so
  fixes (e.g. 2.1) share one implementation.
- **Solution:** Generic `fetch_with_retry(client, builder, attempts, backoff,
  retryable_predicate)` in `mod.rs`.

### 2.13 Regexes recompiled on every request
- **Files:** `src/providers/anidb.rs:99-102,147-154,300-301,335,412`,
  `animehub.rs:119,332,389`, `anineko.rs:133,217,227-228,294,301,328,346,408`
- **Problem:** `Regex::new` called inside per-request hot paths.
- **Why it matters:** Wasted CPU per search/stream fetch.
- **Solution:** `std::sync::LazyLock<Regex>` (rust-version 1.85 supports it).

### 2.14 Magic numbers / hardcoded literals
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
  `CACHE_TIMEOUT_SECS`, `AUTO_QUALITY_RANK`); unify the auto label/rank via 2.6.

### 2.15 String slicing / format-assumption parsing
- **Files:** `src/providers/animehub.rs:146-157,266-271`, `mangapill.rs:98`
- **Problem:** `truncate(len - 4)` / `- 6` byte-slicing on user-visible strings;
  `rsplit('/').next()` assumes `<slug>/<n>`; `text.replace("Chapter ", "")`
  assumes the label format verbatim.
- **Why it matters:** Breaks on non-ASCII slugs / label renames; silent misparse.
- **Solution:** Regex-based extraction (e.g. `chapter\s*([\d.]+)`), or a shared
  tested helper for suffix stripping.

### 2.16 `.expect()` selector panics vs error propagation
- **Files:** `src/providers/mangapill.rs:46-47,91,133` vs `animehub.rs:96-117`
- **Problem:** mangapill does `Selector::parse(...).expect("valid CSS selector")`
  (panic on a typo); animehub returns `anyhow!` errors for the same call.
- **Why it matters:** Two failure styles for the identical fallible operation;
  a typo'd selector crashes the process.
- **Solution:** Shared fallible `parse_selector(&str) -> Result<Selector>`.

### 2.17 StreamOption::provider values inconsistent
- **Files:** `src/providers/anidb.rs:366` (`"AniDB"`), `animehub.rs:415,430`
  (`"animehub"` lowercase), `senshi.rs:390` (`"Senshi"`), `anineko.rs:368,423`
  (`"bibiemb"`/`"vibeplayer"` — the embed host, not the provider).
- **Problem:** Displayed directly in `StreamOption::label()` (`types.rs:105-108`);
  users see a third-party embed host for AniNeko and mixed casing elsewhere.
- **Why it matters:** Inconsistent UX; AGENTS.md says it must match
  `Provider::display_name()`.
- **Solution:** Use `Provider::display_name()` everywhere; embed host can be
  appended to the URL/label if needed.

### 2.20 Remaining small duplications
- **Files:** `src/cmd/anime.rs:37-43` vs `src/cmd/sync.rs:52-58` (SkipOptions
  build); `src/cmd/anime.rs:425-443` vs `452-479` (label-by-index resolution);
  `src/cmd/anime.rs:196-206` vs `src/cmd/manga.rs:73-83` (timeout warning);
  `src/cmd/anime.rs:608-621` vs `623-636` (resolve_show/resolve_manga_info);
  `src/sync/mal.rs:392-423` vs `425-450` (exchange_code/refresh_token_inner)
- **Problem:** Each pair is a near-identical block differing in one detail.
- **Solution:** `impl From<&Cli> for SkipOptions`,
  `resolve_label_by_index(label, sorted)`, fold timeout-warning into 2.2, generic
  `resolve_by_title<T>`, and one `post_token_request(params)`.

### 2.21 Mutex `.unwrap()` in async playback path
- **Files:** `src/sync/mal.rs:777,785,804,807,816,824`
- **Problem:** `std::sync::Mutex` locks `.unwrap()`ed inside an async playback
  loop; a poisoned mutex panics.
- **Why it matters:** Panic mid-playback on any task panic.
- **Solution:** `unwrap_or_else(|e| e.into_inner())` helper or `parking_lot`.

### 2.22 Test & CI debt
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

### 2.23 Vestigial migration shims (intentional — document removal)
- **Files:** `src/sync/mal.rs:81-89` (legacy `entries` bucket with
  `#[allow(dead_code)]`), `src/cmd/sync.rs:374` (`let _token = ...` discard),
  `src/types.rs:122,125` (`serde(alias = "allanime")` / `"123animehub"`)
- **Problem:** Deliberate compatibility shims for old history/cache files.
- **Why it matters:** `#[allow(dead_code)]` is a standing dead-code permit;
  without a removal date they persist forever.
- **Solution:** Keep, but annotate each with the earliest version at which it can
  be deleted; replace `_token` with a `_` binding.
