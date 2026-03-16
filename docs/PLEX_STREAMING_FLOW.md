# Plex streaming flow (same network vs proxy)

## Current behaviour

When you play a track, the app can stream in two ways:

### 1. Proxy path (default when direct is disabled)

- **Client** builds: `http://localhost:4000/api/plex/stream?token=...&path=...&container=...`
- **Client** sets `audio.src = that URL` (no extra request for stream URL).
- **Browser** → **Sonic server** (localhost): one round-trip to start the request.
- **Sonic server**:
  - Calls `getServerUri(token)` (cached after first use; else: plex.tv/devices.xml + optional local discovery + probe).
  - Fetches from Plex: `GET {plexBase}/library/parts/{id}?X-Plex-Token=...`
  - Pipes (or buffers) the response back to the client.
- **Browser** receives stream from Sonic and plays.

**Time adders:**

| Step | What adds time |
|------|----------------|
| Browser → Sonic | One RTT (local, small). |
| getServerUri (cache miss) | plex.tv round-trip + optional discovery + probe (seconds on first use). |
| Sonic → Plex | One RTT to Plex (same LAN or internet). |
| Data path | **Plex → Sonic → Browser**: extra copy and buffering through Sonic. |
| Proxy work | Server does fetch + pipe/buffer; more CPU and memory. |

### 2. Direct path (when same network and server URI is cached)

- **Client** already has `plexServerUri` (e.g. `http://192.168.1.x:32400`) from `/api/plex/server-uri` (called on sign-in or Settings refresh).
- **Client** builds URL in the browser: `{plexServerUri}/library/parts/{partKey}?X-Plex-Token=...`
- **Client** sets `audio.src = that URL` (no request to Sonic for the stream).
- **Browser** → **Plex server** directly (same LAN).
- **Plex** responds with the audio stream.
- **Browser** receives and plays.

**Time saved:**

- No stream request to Sonic (no proxy round-trip for the stream).
- No proxy copy: data goes **Plex → Browser** only.
- Playback can start as soon as Plex sends the first bytes (and range requests work directly with Plex).

## When we use which path

- **Direct** is used when `plexServerUri` is set and we have a token (same network as Plex is typical; server-uri is fetched and cached on sign-in).
- **Proxy** is used when we don’t have `plexServerUri` yet, or when direct is not enabled (e.g. remote or first load before server-uri returns).

## Reducing time in the process

1. **Use direct when possible**  
   Enable direct Plex URLs when `plexServerUri` is available so the browser talks to Plex directly. No Sonic proxy for the stream → fewer hops and less latency.

2. **Cache server URI**  
   We already fetch and cache `plexServerUri` (and optionally connection/local) on sign-in so the first play doesn’t need an extra round-trip for the stream URL.

3. **Preload next track**  
   We already preload the next track (with 0 ms delay) so the next track is buffered; direct URLs make this a direct Browser ↔ Plex flow.

4. **Avoid extra round-trips**  
   With direct: no `/api/plex/stream` or `/api/plex/stream-url` call for playback. With proxy: every play goes through Sonic once.

5. **Server-side**  
   - `getServerUri` is cached so repeated stream requests don’t hit plex.tv every time.
   - Stream proxy uses streaming pipe (no full buffering) when possible.
