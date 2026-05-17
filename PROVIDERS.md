# Project Knowledge: Provider & Stream Handling

## Supported Providers
- **AllAnime**: Primary anime provider, supports both sub and dub. Uses an AES-encrypted GraphQL API and obfuscated "clock" URLs for stream links.
- **MangaDex**: Manga provider.
- **Mangapill**: Manga provider.

## AllAnime Stream Fetching Strategy
As of May 2026, the `AllAnimeClient` uses a **Concurrent Racing** and **Optimized Query** strategy:

1. **Automatic Persisted Queries (APQ)**: To minimize API latency, `anv` first attempts a GET request using a SHA256 hash (`d405d0...`) of the GraphQL query. If the server lacks the cached query, it automatically falls back to a full POST request.
2. **Parallel Requests**: `anv` fires requests to all `PREFERRED_PROVIDERS` simultaneously using `FuturesUnordered`.
3. **Fastest-Wins**: The first provider to return a valid set of stream options is returned immediately; losing branches are cancelled.
4. **Proactive HLS Detection**: External CDN links (e.g., `fast4speed.rsvp`) are treated as direct HLS streams, skipping `clock.json` parsing.
5. **Advanced Provider Support (In Progress)**:
    - **Filemoon (`Fm-mp4`)**: Uses a specialized AES-256-CTR decryption logic to extract links from an encrypted JSON payload (`iv`, `payload`, `key_parts`).
    - **Mp4Upload (`Mp4`)**: Requires a regex-based scrape of the embed iframe with a specific referer (`https://www.mp4upload.com/`).

## Technical Details
- **Dependency**: Uses `futures` for `FuturesUnordered`, and `aes`/`ctr` for API and Filemoon decryption.
- **Hashes**: `EPISODE_SOURCES_HASH` is `d405d0edd690624b66baba3068e0edc3ac90f1597d898a1ec8db4e5c43c00fec`.
- **Referers**: 
    - API (GET/APQ): `https://youtu-chan.com`
    - API (POST): `https://allmanga.to`
    - Streams: Typically `https://youtu-chan.com` or provider-specific (e.g., `mp4upload`).
