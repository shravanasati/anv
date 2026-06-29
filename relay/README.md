# AllAnime API Relay (Cloudflare Worker)

A lightweight reverse proxy that forwards AllAnime API requests from geo-blocked
regions. Only the small JSON API calls go through the relay — video streams are
fetched directly from the CDN, so there's **zero impact on playback performance**.

## Why?

AllAnime uses Cloudflare to geo-block API requests from certain regions (e.g. India),
returning a `NEED_CAPTCHA` error. This relay runs on Cloudflare Workers (free tier:
100k requests/day) in a non-blocked region and forwards your API calls.

## Setup

### 1. Install Wrangler (Cloudflare CLI)

```bash
npm install -g wrangler
```

### 2. Authenticate

```bash
wrangler login
```

### 3. Deploy

```bash
cd relay/
wrangler deploy
```

This will output a URL like `https://anv-relay.<your-subdomain>.workers.dev`.

### 4. Configure anv

Add the worker URL to your anv config (`~/.config/anv/config.toml`):

```toml
api_proxy = "https://anv-relay.<your-subdomain>.workers.dev"
```

Or set it via environment variable:

```bash
export ANV__API_PROXY="https://anv-relay.<your-subdomain>.workers.dev"
```

## How it Works

```
anv (your machine)
  │
  ├── API calls ──→ Cloudflare Worker (US/EU) ──→ api.allanime.day ──→ response back
  │
  └── Video streams ──→ CDN (direct, no relay) ──→ plays in mpv
```

## Free Tier Limits

Cloudflare Workers free tier gives you **100,000 requests/day**. Each episode
typically needs ~2-5 API calls, so you'd need to watch ~20,000 episodes/day to
hit the limit.
