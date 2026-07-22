# Project Knowledge: Provider & Stream Handling

## Supported Providers
- **AllAnime**: Primary anime provider, supports both sub and dub. Uses an AES-encrypted GraphQL API and obfuscated "clock" URLs for stream links.
- **AniNeko**: Anime provider (`anineko.to`), supports sub and dub playback via Bibiemb master m3u8 streams and VibePlayer streams (handled with a local HTTP proxy that strips PNG wrappers from video segments).
- **MangaDex**: Manga provider.
- **Mangapill**: Manga provider.

## Multi-Provider Fallback Strategy (`-p all`)
`anv` defaults to `--provider all` (`Provider::All`):
1. **Anime Mode**:
   - Queries **AllAnime** first.
   - If AllAnime returns 0 search results or stream extraction fails during playback, `anv` automatically falls back to **AniNeko**.
2. **Manga Mode**:
   - Queries **AllAnime** (manga mode) first, falling back to **MangaDex** and **Mangapill** if necessary.

## AllAnime Stream Fetching Strategy
As of May 2026, the `AllAnimeClient` uses a **Concurrent Racing** and **Optimized Query** strategy:

1. **Automatic Persisted Queries (APQ)**: To minimize API latency, `anv` first attempts a GET request using a SHA256 hash (`d405d0...`) of the GraphQL query. If the server lacks the cached query, it automatically falls back to a full POST request.
2. **Parallel Requests**: `anv` fires requests to all `PREFERRED_PROVIDERS` simultaneously using `FuturesUnordered`.
3. **Fastest-Wins**: The first provider to return a valid set of stream options is returned immediately; losing branches are cancelled.
4. **Proactive HLS Detection**: External CDN links (e.g., `fast4speed.rsvp`) are treated as direct HLS streams, skipping `clock.json` parsing.
5. **Advanced Provider Support (In Progress)**:
    - **Filemoon (`Fm-mp4`)**: Uses a specialized AES-256-CTR decryption logic to extract links from an encrypted JSON payload (`iv`, `payload`, `key_parts`).
    - **Mp4Upload (`Mp4`)**: Requires a regex-based scrape of the embed iframe with a specific referer (`https://www.mp4upload.com/`).

## AniNeko Stream Resolution & Local VibeProxy
- **Bibiemb Embeds**: `anv` parses HLS master playlists and presents quality variants (1080p, 720p, etc.) with original subtitles.
- **VibePlayer Embeds**: Video segments have PNG headers prepended. `anv` uses an in-memory local HLS proxy server on `127.0.0.1` (`VibeProxy`) to rewrite HLS playlists and dynamically strip PNG `IEND` chunk headers (`0x49 0x45 0x4E 0x44 0xAE 0x42 0x60 0x82`) before serving MPEG-TS (`video/mp2t`) to `mpv`.

