/**
 * anv API Relay — Cloudflare Worker
 *
 * Forwards AllAnime GraphQL API requests from geo-blocked regions.
 * Deploy with: wrangler deploy
 */

const UPSTREAM = "https://api.allanime.day";

export default {
  async fetch(request) {
    const url = new URL(request.url);

    // Health check
    if (url.pathname === "/" || url.pathname === "/health") {
      return new Response(JSON.stringify({ status: "ok", relay: "anv" }), {
        headers: { "Content-Type": "application/json" },
      });
    }

    // Build upstream URL: forward path + query string to api.allanime.day
    const upstream = new URL(url.pathname + url.search, UPSTREAM);

    // Clone relevant headers, replacing Host
    const headers = new Headers(request.headers);
    headers.set("Host", new URL(UPSTREAM).host);

    // Forward the request
    const response = await fetch(upstream.toString(), {
      method: request.method,
      headers,
      body: request.method !== "GET" ? request.body : undefined,
    });

    // Return response with CORS headers so it works from anywhere
    const respHeaders = new Headers(response.headers);
    respHeaders.set("Access-Control-Allow-Origin", "*");
    respHeaders.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    respHeaders.set("Access-Control-Allow-Headers", "*");

    // Handle CORS preflight
    if (request.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: respHeaders });
    }

    return new Response(response.body, {
      status: response.status,
      headers: respHeaders,
    });
  },
};
