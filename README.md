
<p align="center">
 <img src="./assets/logo.png" width=225 height=200>
</p>

anv is a terminal-native anime launcher for people who think tmux panes and watchlists belong together. Point it at a title, pick your episode, and drop straight into `mpv` without touching a browser tab.

## Why terminal otaku dig it
- Curated for AllAnime streams – fast GraphQL search with zero spoiler thumbnails.
- Sub or dub on demand via `--dub`; switches the query and history tagging automatically.
- Episode selector behaves like a shell picker: arrow keys, `Enter`, `Esc` to bail.
- Remembers what you watched last night, including translation choice – `anv history` drops you right back in.
- Reads manga too – `anv --manga` fetches chapters and pipes pages directly to your image viewer (mpv by default).
- Manga page cache supports custom location via `--cache-dir`.
- Jump directly to an episode with `-e` or `--episode` to skip the selection menu.
- Fires up `mpv` (or whatever you set as `player` in config) with the highest-quality stream it can negotiate.
- Syncs watch progress to MyAnimeList – sets start/finish dates, marks completed automatically.
- Browse your MAL **Plan to Watch** list with `anv watchlist` and start streaming in one step – no separate search needed.

## Install it

### Install prebuilt binaries via shell script

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/shravanasati/anv/releases/latest/download/anv-installer.sh | sh
```

### Install prebuilt binaries via powershell script

```sh
powershell -ExecutionPolicy Bypass -c "(irm https://github.com/shravanasati/anv/releases/latest/download/anv-installer.ps1) | iex"
```

### Install via GitHub Releases

Pre-built binaries for all platforms are available on [GitHub Releases](https://github.com/shravanasati/anv/releases).


### Cargo
```bash
cargo install anv
```

### Updating

The installer ships an `anv-update` binary alongside `anv`. Run it any time you want the latest release:

```sh
anv-update
```

> **Note:** If something breaks or streams stop working, run `anv-update` first before raising an issue — most provider-related breakages are fixed in patch releases.


## Quick start quests

Search and stream:
```bash
anv "bocchi the rock"
```

Prefer the dub:
```bash
anv --dub "demon slayer"
```

Read manga chapters:
```bash
anv --manga "one punch man"
```

Read manga with a custom cache directory:
```bash
anv --manga --cache-dir "/tmp/anv-cache" "one punch man"
```

Jump back to last night's cliffhanger:
```bash
anv --history
```

Jump directly to an episode:
```bash
anv -e 12 "bocchi the rock"
```

Watch in binge mode (automatically play subsequent episodes):
```bash
anv -e 1 -b "bocchi the rock"
```

Pick from your MAL Plan to Watch list and stream:
```bash
anv watchlist
```

Same, but start dubbed and jump to episode 1:
```bash
anv watchlist -d -e 1
```

Set a custom player (e.g. tuned mpv build):
```bash
# via environment variable
export ANV_PLAYER="/usr/bin/mpv --ytdl-format=best"
anv "naruto"

# or permanently in ~/.config/anv/config.toml
# player = "/usr/bin/mpv --ytdl-format=best"
```

## MAL sync

anv can automatically sync your watch progress to [MyAnimeList](https://myanimelist.net).

### Setup

**1. Create a MAL API application**

Go to [myanimelist.net/apiconfig](https://myanimelist.net/apiconfig), create a new app, and set:
- **App type:** `other`
- **Redirect URI:** `http://localhost:11422/callback`

Copy the **Client ID**.

**2. Add it to your config**

The config file lives at `~/.config/anv/config.toml` (Linux/macOS) or `%APPDATA%\anv\config.toml` (Windows).

```toml
[mal]
client_id = "<your-client-id>"

[sync]
enabled = true
```

**3. Authenticate**

```bash
anv sync enable mal
```

This opens your browser to the MAL authorisation page. After you approve, the token is saved to your data directory and you're done.

### Sync commands

| Command | What it does |
|---|---|
| `anv sync enable mal` | Authenticate with MAL (runs OAuth flow if no token stored) |
| `anv sync status` | Show whether sync is enabled, token validity, and expiry |
| `anv sync disable` | Disable sync (`sync.enabled = false` in config) |

### Watchlist

`anv watchlist` pulls your **Plan to Watch** list directly from MAL and lets you pick a title to stream — no search step required.

```bash
anv watchlist          # sub (default)
anv watchlist -d       # dubbed
anv watchlist -b       # binge mode
anv watchlist -e 5     # start at episode 5
```

**What it shows:** only titles that are currently airing or have finished airing. Anime that hasn't premiered yet (`not_yet_aired`) is automatically hidden so the list stays actionable. Each entry shows an episode count and a `· airing` or `· finished` tag.

### How sync works

After each episode finishes playing:

1. **First time seeing a show** — anv searches MAL for the title and shows you the English and Japanese names to confirm it found the right one. The match is cached locally so it never asks again for that series.
2. **Progress update** — if the status on MAL is already `watching` and only the episode count changes, anv updates silently with no prompt. If the status is changing (e.g. adding to list for the first time, or reaching the final episode), anv asks for confirmation first.
3. **Dates are set automatically:**
   - `start_date` is sent when you first start watching (not on list, or previously `plan_to_watch`).
   - `finish_date` is sent when anv marks the show as `completed`.

## How the flow feels

1. CLI asks AllAnime for matching series and shows you a clean list.
2. Pick a show; anv fetches available episode numbers for the chosen translation.
3. Episode picker highlights your last watched entry so Enter instantly resumes; Esc backs out like a prompt should.
4. Streams are resolved through AllAnime's clock API and piped to `mpv` with the right headers and subtitles.
5. History gets updated in `~/.local/share/anv/history.json` (Linux; platform-specific on others) so the next session remembers everything.
6. If MAL sync is enabled, watch progress is synced silently or with a brief confirmation depending on what changed.

## Tips and tweaks
- Keep `mpv` upgraded – some providers only serve DASH/HLS variants that older builds struggle with.
- If you want to experiment with custom players, set `player` in `~/.config/anv/config.toml` or use the `ANV_PLAYER` environment variable (env overrides config).
- Use `--cache-dir <DIR>` if you want manga page cache files somewhere specific (faster disk, larger partition, etc.).
- Use `-e <EP>` to skip the interactive episode selector and start playing a specific episode immediately.
- Run `anv-update` to pull the latest release whenever streams break or a new AllAnime quirk surfaces.
- Run `anv sync status` to quickly check if your MAL token is still valid before a long watch session.
- `anv watchlist` is the fastest path from "what should I watch?" to actually watching it — the MAL→AllAnime mapping is cached after the first run, so subsequent launches are instant.

## Troubleshooting

> **First step for any breakage:** run `anv-update` to make sure you are on the latest release before raising an issue. Most provider and stream failures are already fixed in the newest version.

- `mpv` not found: install it or set `player` in your config (or `ANV_PLAYER` env var).
- Streams empty: AllAnime occasionally throttles or shuffles providers; run `anv-update`, then try again.
- History file corrupted: delete the JSON under your data dir and anv recreates it on launch.
- MAL sync not working: run `anv sync status` to check token state, then `anv sync enable mal` to re-authenticate if needed.

## License

Released under the [MIT License](LICENSE). Have fun, stay hydrated, and don't skip the ending songs.
