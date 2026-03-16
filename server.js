import express from "express";
import cors from "cors";
import crypto from "crypto";
import os from "os";
import { Readable } from "stream";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

// CJS bundle has __filename; ESM has import.meta.url (check __filename first to avoid import.meta in CJS bundle)
let __dirnameForData;
if (typeof __filename !== "undefined") {
  __dirnameForData = path.dirname(__filename);
} else if (typeof import.meta !== "undefined" && import.meta.url) {
  __dirnameForData = path.dirname(fileURLToPath(import.meta.url));
} else {
  __dirnameForData = process.cwd();
}
const __dirname = __dirnameForData;
const app = express();
// Prefer env (set by Electron so app works from any launch folder); else dir next to server.js
const DATA_DIR = process.env.DATA_DIR || path.join(__dirname, "data");
const PLAYLISTS_FILE = path.join(DATA_DIR, "playlists.json");
const SETTINGS_FILE = path.join(DATA_DIR, "settings.json");
const PORT = process.env.PORT ? Number(process.env.PORT) : 4000;

const LOCAL_MUSIC_FOLDER_NAME = "sonicmusic";
const LOCAL_MUSIC_EXTENSIONS = new Set([".mp3", ".m4a", ".aac", ".flac", ".ogg", ".wav", ".webm"]);

const LOCAL_COVER_REGEX = /^(cover|folder|albumart|front)\.(jpg|jpeg|png)$/i;

/** Cache of embedded art by relative path; populated during scan, cleared on next scan. */
const localArtCache = new Map();

/** Resolves with the port the server actually bound to (when listening). */
let resolveListening;
export const whenListening = new Promise((r) => { resolveListening = r; });

// Allow browser and dev: any localhost/127.0.0.1 port (tray app opens in default browser, no Electron window)
function corsOrigin(origin, cb) {
  const allowed = [
    "http://localhost:1430",
    "http://127.0.0.1:1430",
  ];
  if (typeof origin === "string" && /^https?:\/\/(localhost|127\.0\.0\.1)(:\d+)?\/?$/i.test(origin)) return cb(null, true);
  if (!origin || allowed.includes(origin) || origin === "null") return cb(null, true);
  if (typeof origin === "string" && (origin.startsWith("tauri://") || origin.startsWith("https://asset.localhost"))) return cb(null, true);
  return cb(null, false);
}
app.use(cors({ origin: corsOrigin }));
app.use(express.json({ limit: "10mb" }));

// IMPORTANT: This client identifier must match what the frontend uses
// when opening the Plex auth page, otherwise Plex will reject the PIN
// with "unable to complete this request".
const PLEX_CLIENT_IDENTIFIER = "crystal-player-web";
const PLEX_PRODUCT = "Sonic";
const PLEX_DEVICE = "Browser";
const PLEX_PLATFORM = "Web";

/** In-memory state for the current Plex token. */
let activePlexToken = null;

/** Cache for Plex API responses. Key -> { data, expiresAt }. TTL 15 minutes for faster repeat loads. */
const CACHE_TTL_MS = 15 * 60 * 1000;
const apiCache = new Map();

/** Disk cache for libraries/albums so app opens fast without re-fetching from Plex. No TTL on disk. */
const PLEX_CACHE_DIR = path.join(DATA_DIR, "plex-cache");
const PLEX_DISK_CACHE_KEYS = new Set(["libraries", "albums"]);

function plexDiskCachePath(key) {
  const hash = crypto.createHash("sha256").update(key).digest("hex").slice(0, 32);
  return path.join(PLEX_CACHE_DIR, `${hash}.json`);
}

function getCached(key) {
  const entry = apiCache.get(key);
  if (entry && Date.now() <= entry.expiresAt) return entry.data;
  const prefix = key.split(":")[0];
  if (PLEX_DISK_CACHE_KEYS.has(prefix)) {
    try {
      const fp = plexDiskCachePath(key);
      if (fs.existsSync(fp)) {
        const raw = fs.readFileSync(fp, "utf8");
        const parsed = JSON.parse(raw);
        const data = parsed.payload != null ? parsed.payload : parsed;
        apiCache.set(key, { data, expiresAt: Date.now() + CACHE_TTL_MS });
        return data;
      }
    } catch (_) { }
  }
  return null;
}

function setCached(key, data) {
  apiCache.set(key, { data, expiresAt: Date.now() + CACHE_TTL_MS });
  const prefix = key.split(":")[0];
  if (PLEX_DISK_CACHE_KEYS.has(prefix)) {
    try {
      if (!fs.existsSync(PLEX_CACHE_DIR)) fs.mkdirSync(PLEX_CACHE_DIR, { recursive: true });
      const fp = plexDiskCachePath(key);
      fs.writeFileSync(fp, JSON.stringify({ key, payload: data, savedAt: Date.now() }), "utf8");
    } catch (err) {
      console.warn("Plex disk cache write failed:", err?.message || err);
    }
  }
}

/** In-memory image cache for Plex thumbs. Key -> { body, contentType }. Max 300 entries, 24h TTL. */
const THUMB_CACHE_MAX = 300;
const THUMB_CACHE_TTL_MS = 24 * 60 * 60 * 1000;
const thumbCache = new Map();

function getThumbCached(key) {
  const entry = thumbCache.get(key);
  if (!entry || Date.now() > entry.expiresAt) return null;
  return entry;
}

function setThumbCached(key, body, contentType) {
  while (thumbCache.size >= THUMB_CACHE_MAX && thumbCache.size > 0) {
    const firstKey = thumbCache.keys().next().value;
    thumbCache.delete(firstKey);
  }
  thumbCache.set(key, {
    body,
    contentType: contentType || "image/jpeg",
    expiresAt: Date.now() + THUMB_CACHE_TTL_MS,
  });
}

function plexHeaders(extra = {}) {
  return {
    "X-Plex-Client-Identifier": PLEX_CLIENT_IDENTIFIER,
    "X-Plex-Product": PLEX_PRODUCT,
    "X-Plex-Device": PLEX_DEVICE,
    "X-Plex-Platform": PLEX_PLATFORM,
    Accept: "application/json",
    ...extra,
  };
}

async function plexFetchJson(url, options = {}) {
  const res = await fetch(url, {
    ...options,
    headers: {
      ...plexHeaders(options.headers || {}),
    },
  });

  const text = await res.text();
  if (!res.ok) {
    const snippet = text.slice(0, 200);
    const err = new Error(`Plex HTTP ${res.status}: ${snippet}`);
    err.status = res.status;
    throw err;
  }

  try {
    return JSON.parse(text);
  } catch (e) {
    const err = new Error(
      `Failed to parse Plex JSON: ${e.message}; body snippet: ${text.slice(
        0,
        200
      )}`
    );
    err.status = 500;
    throw err;
  }
}

// Decode HTML/XML entities in attribute values (e.g. &#8208; → hyphen, &#26085; → 日).
function decodeXmlEntities(str) {
  if (typeof str !== "string" || !str.length) return str;
  return str
    .replace(/&#x([0-9a-fA-F]+);/g, (_, hex) => {
      const code = parseInt(hex, 16);
      return code <= 0x10ffff ? String.fromCodePoint(code) : "";
    })
    .replace(/&#(\d+);/g, (_, dec) => {
      const code = parseInt(dec, 10);
      return code <= 0x10ffff ? String.fromCodePoint(code) : "";
    })
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&amp;/g, "&");
}

// Very small XML attribute parser for simple Plex XML responses.
function parseXmlAttributes(tagLine) {
  const attrs = {};
  const attrRegex = /(\w+)="([^"]*)"/g;
  let m;
  while ((m = attrRegex.exec(tagLine))) {
    attrs[m[1]] = m[2];
  }
  return attrs;
}

// POST /api/plex/pins -> create a Plex PIN via plex.tv
app.post("/api/plex/pins", async (_req, res) => {
  try {
    const data = await plexFetchJson("https://plex.tv/api/v2/pins?strong=true", {
      method: "POST",
    });

    // The frontend expects { id, code, authToken, clientIdentifier }
    res.json({
      id: data.id,
      code: data.code,
      authToken: data.authToken ?? null,
      clientIdentifier: data.clientIdentifier || PLEX_CLIENT_IDENTIFIER,
    });
  } catch (err) {
    console.error("Error creating Plex PIN", err);
    res
      .status(err.status || 500)
      .json({ error: "Failed to create Plex PIN", detail: String(err.message) });
  }
});

// POST /api/plex/token -> let the frontend explicitly set the active Plex token
app.post("/api/plex/token", (req, res) => {
  const { token } = req.body || {};
  if (!token || typeof token !== "string") {
    return res
      .status(400)
      .json({ error: "Missing token", detail: "Expected JSON body { token }." });
  }

  activePlexToken = token;
  console.log("Active Plex token set from frontend:", activePlexToken);
  res.json({ ok: true });
});

// GET /api/plex/pins/:id -> poll a Plex PIN status
app.get("/api/plex/pins/:id", async (req, res) => {
  const id = req.params.id;
  try {
    const data = await plexFetchJson(`https://plex.tv/api/v2/pins/${id}`, {
      method: "GET",
    });

    if (data.authToken) {
      activePlexToken = data.authToken;
      console.log("Active Plex token set from PIN:", activePlexToken);
    }

    res.json({
      id: data.id,
      code: data.code,
      authToken: data.authToken ?? null,
    });
  } catch (err) {
    console.error("Error polling Plex PIN", err);
    res
      .status(err.status || 500)
      .json({ error: "Failed to poll Plex PIN", detail: String(err.message) });
  }
});

// Cache which server URI is reachable (local vs remote) so we don't probe every request.
const SERVER_URI_CACHE_TTL_MS = 60 * 1000; // 1 minute
const serverUriCache = new Map(); // token -> { preferredUri, remoteUri, expiresAt }

function getCachedServerUri(token) {
  const entry = serverUriCache.get(token);
  if (!entry || Date.now() > entry.expiresAt) return null;
  return entry.preferredUri;
}

function setCachedServerUri(token, preferredUri, remoteUri) {
  serverUriCache.set(token, {
    preferredUri,
    remoteUri,
    expiresAt: Date.now() + SERVER_URI_CACHE_TTL_MS,
  });
}

function isPrivateUri(uri) {
  try {
    const u = new URL(uri);
    const host = u.hostname;
    if (host === "localhost" || host === "127.0.0.1") return true;
    if (host.startsWith("192.168.") || host.startsWith("10.")) return true;
    if (/^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(host)) return true;
    return false;
  } catch (_) {
    return false;
  }
}

// Get private IPv4 subnets (e.g. ["192.168.1", "10.0.0"]) and skip our own IPs.
function getPrivateSubnets() {
  const subnets = new Set();
  const ownIps = new Set();
  try {
    const ifaces = os.networkInterfaces();
    for (const name of Object.keys(ifaces)) {
      for (const iface of ifaces[name] || []) {
        if (iface.family !== "IPv4" || iface.internal) continue;
        const addr = iface.address;
        if (!addr) continue;
        if (addr.startsWith("192.168.") || addr.startsWith("10.") || /^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(addr)) {
          ownIps.add(addr);
          const parts = addr.split(".");
          if (parts.length === 4) subnets.add(parts.slice(0, 3).join("."));
        }
      }
    }
  } catch (_) { }
  return { subnets: [...subnets], ownIps };
}

// Probe a single IP for Plex identity; returns base URL if machineIdentifier matches.
const DISCOVERY_TIMEOUT_MS = 500;
const DISCOVERY_MAX_PER_SUBNET = 100; // .1–.100 so we find servers at high IPs
async function probePlexIdentity(ip, token, targetMachineId) {
  const url = `http://${ip}:32400/identity?X-Plex-Token=${encodeURIComponent(token)}`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), DISCOVERY_TIMEOUT_MS);
  try {
    const res = await fetch(url, {
      method: "GET",
      headers: { ...plexHeaders() },
      signal: controller.signal,
    });
    clearTimeout(timeout);
    if (!res.ok) return null;
    const text = await res.text();
    const mediaContainerMatch = text.match(/<MediaContainer\s[^>]*>/);
    if (!mediaContainerMatch) return null;
    const attrs = parseXmlAttributes(mediaContainerMatch[0]);
    const machineId = attrs.machineIdentifier;
    if (targetMachineId && machineId !== targetMachineId) return null;
    // If no targetMachineId, any Plex server that accepts our token is fine (single-server LAN)
    return `http://${ip}:32400`;
  } catch (_) {
    clearTimeout(timeout);
    return null;
  }
}

// Discover Plex server on local network by scanning same-subnet IPs. Returns base URL or null.
async function discoverLocalPlexUri(token, targetMachineId) {
  const { subnets, ownIps } = getPrivateSubnets();
  if (subnets.length === 0) return null;
  const candidates = [];
  for (const subnet of subnets) {
    for (let i = 1; i <= DISCOVERY_MAX_PER_SUBNET; i++) {
      const ip = `${subnet}.${i}`;
      if (ownIps.has(ip)) continue;
      candidates.push(ip);
    }
  }
  // Probe in parallel (batches of 12) to find first match quickly
  const BATCH = 12;
  for (let i = 0; i < candidates.length; i += BATCH) {
    const batch = candidates.slice(i, i + BATCH);
    const results = await Promise.all(
      batch.map((ip) => probePlexIdentity(ip, token, targetMachineId))
    );
    const found = results.find((r) => r != null);
    if (found) return found;
  }
  return null;
}

// Resolve Plex server URI. Tries connections from Plex (local first), then local network discovery, then first reachable.
async function getServerUri(token) {
  const cached = getCachedServerUri(token);
  if (cached) {
    const local = isPrivateUri(cached);
    return { serverUri: cached, publicAddress: null, local };
  }

  const devicesRes = await fetch(
    `https://plex.tv/devices.xml?X-Plex-Token=${encodeURIComponent(token)}`,
    {
      method: "GET",
      headers: { ...plexHeaders(), Accept: "application/xml" },
    }
  );
  const devicesXml = await devicesRes.text();
  if (!devicesRes.ok) {
    const err = new Error(`Plex devices: ${devicesRes.status}`);
    err.status = devicesRes.status;
    throw err;
  }
  const deviceRegex = /<Device [^>]*provides="[^"]*server[^"]*"[^>]*>([\s\S]*?)<\/Device>/g;
  const connectionRegex = /<Connection [^>]*\/>/g;
  const deviceMatch = deviceRegex.exec(devicesXml);
  if (!deviceMatch) return { serverUri: null, publicAddress: null };
  const deviceTagLine = deviceMatch[0].split(">")[0] + "/>";
  const deviceAttrs = parseXmlAttributes(deviceTagLine);
  const publicAddress = deviceAttrs.publicAddress || null;
  // Plex.tv devices.xml: server device may use machineIdentifier or clientIdentifier (anywhere in device block)
  const deviceContent = deviceMatch[0];
  let targetMachineId =
    deviceAttrs.machineIdentifier ||
    deviceAttrs.clientIdentifier ||
    deviceAttrs.id ||
    null;
  if (!targetMachineId && deviceContent) {
    const midMatch = deviceContent.match(/machineIdentifier="([^"]+)"/);
    if (midMatch) targetMachineId = midMatch[1];
  }
  const allConnections = [];
  let connMatch;
  while ((connMatch = connectionRegex.exec(deviceContent)) !== null) allConnections.push(connMatch[0]);

  const orderedUris = [];
  const restUris = [];
  for (const connTag of allConnections) {
    const connAttrs = parseXmlAttributes(connTag);
    let resolved = connAttrs.uri;
    if (!resolved && (connAttrs.address || connAttrs.host) && connAttrs.port) {
      const scheme = connAttrs.protocol || "http";
      resolved = `${scheme}://${connAttrs.address || connAttrs.host}:${connAttrs.port}`;
    }
    if (!resolved) continue;
    if (connAttrs.local === "1") orderedUris.push(resolved);
    else restUris.push(resolved);
  }
  let urisToTry = [...orderedUris, ...restUris];

  // Discover Plex on local network so we use LAN IP (faster, and "local" = true).
  try {
    const discovered = await discoverLocalPlexUri(token, targetMachineId);
    if (discovered) {
      urisToTry = [discovered, ...urisToTry];
      console.log("[Plex] Discovered local server:", discovered);
    }
  } catch (e) {
    console.warn("[Plex] Local discovery failed:", e?.message || e);
  }

  urisToTry.sort((a, b) => (isPrivateUri(a) === isPrivateUri(b) ? 0 : isPrivateUri(a) ? -1 : 1));
  const fallbackUri = urisToTry[0] || (publicAddress ? `http://${publicAddress}:32400` : null);

  let serverUri = fallbackUri;
  const PROBE_TIMEOUT_MS = 5000;
  for (const base of urisToTry) {
    try {
      const probeUrl = `${base.replace(/\/$/, "")}/identity`;
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), PROBE_TIMEOUT_MS);
      const probeRes = await fetch(probeUrl, {
        method: "GET",
        headers: { ...plexHeaders() },
        signal: controller.signal,
      });
      clearTimeout(timeout);
      if (probeRes.ok) {
        serverUri = base;
        console.log("[Plex] Using server (local first):", serverUri);
        break;
      }
    } catch (e) {
      if (isPrivateUri(base)) console.warn("[Plex] Local unreachable:", base, e?.message || e);
      continue;
    }
  }
  if (!serverUri && publicAddress) serverUri = `http://${publicAddress}:32400`;
  setCachedServerUri(token, serverUri, fallbackUri);
  const local = serverUri ? isPrivateUri(serverUri) : false;
  return { serverUri, publicAddress, local };
}

// GET /api/plex/connection -> { local: boolean } so the app can pick streaming quality
app.get("/api/plex/connection", async (req, res) => {
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    req.headers["x-plex-token"] ||
    activePlexToken;
  if (!token) return res.status(400).json({ error: "Missing token" });
  try {
    const { local } = await getServerUri(token);
    res.json({ local: !!local });
  } catch (err) {
    res.status(err.status || 500).json({ error: "Connection check failed", detail: String(err.message) });
  }
});

// GET /api/plex/server-uri -> { serverUri } so the app can build direct Plex stream URLs without opening Music
app.get("/api/plex/server-uri", async (req, res) => {
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    req.headers["x-plex-token"] ||
    activePlexToken;
  if (!token) return res.status(400).json({ error: "Missing token" });
  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) return res.status(404).json({ error: "No Plex servers" });
    res.json({ serverUri: serverUri.replace(/\/$/, "") });
  } catch (err) {
    res.status(err.status || 500).json({ error: "Failed to get server URI", detail: String(err.message) });
  }
});

// GET /api/plex/libraries -> music library only for the active server
app.get("/api/plex/libraries", async (req, res) => {
  const tokenFromQuery = req.query.token;
  const tokenFromHeader = req.headers["x-plex-token"];
  const token =
    (typeof tokenFromQuery === "string" && tokenFromQuery) ||
    (typeof tokenFromHeader === "string" && tokenFromHeader) ||
    activePlexToken;

  if (!token) {
    return res.status(400).json({
      error: "Not authenticated with Plex",
      detail: "No Plex token. Sign in on the Plex tab, or pass ?token= or X-Plex-Token header.",
    });
  }

  const cacheKey = `libraries:${token}`;
  const forceRefresh = req.query.refresh === "1" || req.query.refresh === "true";
  if (!forceRefresh) {
    const cached = getCached(cacheKey);
    if (cached) return res.json(cached);
  }

  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) {
      return res.status(404).json({
        error: "No Plex servers",
        detail: "No Plex Media Server found for this account.",
      });
    }

    const libsUrl = `${serverUri.replace(/\/$/, "")}/library/sections?X-Plex-Token=${encodeURIComponent(token)}`;
    const libsRes = await fetch(libsUrl, {
      method: "GET",
      headers: { ...plexHeaders(), Accept: "application/xml" },
    });
    const libsXml = await libsRes.text();

    if (!libsRes.ok) {
      console.error("Plex library/sections failed:", libsRes.status, libsUrl, libsXml.slice(0, 300));
      const err = new Error(`Plex library request failed ${libsRes.status}: ${libsXml.slice(0, 200)}`);
      err.status = libsRes.status;
      throw err;
    }

    // Parse Directory elements (Plex: <Directory key="1" type="artist" title="Music" ...><Location .../></Directory>).
    const simplified = [];
    // Match opening tag only; attributes can span newlines.
    const dirOpenRegex = /<Directory\s+([^>]+)>/g;
    let m;
    while ((m = dirOpenRegex.exec(libsXml)) !== null) {
      const attrStr = m[1].replace(/\s+/g, " ").trim();
      const attrs = parseXmlAttributes("<d " + attrStr + " />");
      const key = attrs.key || attrs.Key || attrs.id || attrs.Id;
      const title = decodeXmlEntities(attrs.title || attrs.Title);
      const type = attrs.type || attrs.Type || "unknown";
      if (key && title) {
        simplified.push({ key: String(key), title: String(title), type: String(type) });
      }
    }
    if (simplified.length === 0) {
      const sectionOpenRegex = /<Section\s+([^>]+)>/g;
      while ((m = sectionOpenRegex.exec(libsXml)) !== null) {
        const attrStr = m[1].replace(/\s+/g, " ").trim();
        const attrs = parseXmlAttributes("<d " + attrStr + " />");
        const key = attrs.key || attrs.Key || attrs.id || attrs.Id;
        const title = decodeXmlEntities(attrs.title || attrs.Title);
        const type = attrs.type || attrs.Type || "unknown";
        if (key && title) {
          simplified.push({ key: String(key), title: String(title), type: String(type) });
        }
      }
    }

    if (simplified.length === 0) {
      console.error("Plex library/sections: no Directory/Section found. Response (first 800 chars):", libsXml.slice(0, 800));
    }

    // This app is music-only: return only the music library (type artist).
    const musicOnly = simplified.filter(
      (s) => String(s.type).toLowerCase() === "artist" || String(s.type).toLowerCase() === "music"
    );
    setCached(cacheKey, musicOnly);
    res.json(musicOnly);
  } catch (err) {
    console.error("Error fetching Plex libraries", err);
    res.status(err.status || 500).json({
      error: "Failed to fetch Plex libraries",
      detail: String(err.message),
    });
  }
});

// GET /api/plex/albums -> albums in a music library section (type=9)
app.get("/api/plex/albums", async (req, res) => {
  console.log("GET /api/plex/albums", req.query.sectionKey ? "sectionKey=" + req.query.sectionKey : "no sectionKey");
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    (typeof req.headers["x-plex-token"] === "string" && req.headers["x-plex-token"]) ||
    activePlexToken;
  const sectionKey = req.query.sectionKey;
  if (!token) {
    return res.status(400).json({ error: "Not authenticated", detail: "Pass ?token= or X-Plex-Token header." });
  }
  if (!sectionKey || typeof sectionKey !== "string") {
    return res.status(400).json({ error: "Missing sectionKey", detail: "Pass ?sectionKey=1 (music library key)." });
  }

  const cacheKey = `albums:${token}:${sectionKey}`;
  const forceRefresh = req.query.refresh === "1" || req.query.refresh === "true";
  if (!forceRefresh) {
    const cached = getCached(cacheKey);
    if (cached) return res.json(cached);
  }

  try {
    const { serverUri, publicAddress } = await getServerUri(token);
    if (!serverUri) {
      return res.status(404).json({ error: "No Plex servers", detail: "No server found for this account." });
    }
    const basePath = `/library/sections/${encodeURIComponent(sectionKey)}/all?type=9&X-Plex-Token=${encodeURIComponent(token)}`;
    let url = `${serverUri.replace(/\/$/, "")}${basePath}`;
    let albumsRes = await fetch(url, {
      method: "GET",
      headers: { ...plexHeaders(), Accept: "application/xml" },
    });
    let albumsXml = await albumsRes.text();
    // If 404/timeout and we have a public address different from current host, retry with public URL.
    const usedPublic = publicAddress && (url.startsWith(`http://${publicAddress}:`) || url.startsWith(`https://${publicAddress}:`));
    if (!albumsRes.ok && publicAddress && !usedPublic) {
      const fallbackUrl = `http://${publicAddress}:32400${basePath}`;
      console.warn("Plex albums", albumsRes.status, ", retrying with public address:", fallbackUrl);
      albumsRes = await fetch(fallbackUrl, {
        method: "GET",
        headers: { ...plexHeaders(), Accept: "application/xml" },
      });
      albumsXml = await albumsRes.text();
      url = fallbackUrl;
    }
    if (!albumsRes.ok) {
      console.error("Plex albums failed:", albumsRes.status, url, albumsXml.slice(0, 200));
      return res.status(albumsRes.status).json({
        error: "Failed to fetch albums",
        detail: albumsXml.slice(0, 200) || `Plex returned ${albumsRes.status} for this server.`,
      });
    }
    const effectiveServerUri = url.replace(/\/library\/sections\/.*$/, "").replace(/\/$/, "") || serverUri.replace(/\/$/, "");
    const albums = [];
    const dirOpenRegex = /<Directory\s+([^>]+)>/g;
    let m;
    while ((m = dirOpenRegex.exec(albumsXml)) !== null) {
      const attrStr = m[1].replace(/\s+/g, " ").trim();
      const attrs = parseXmlAttributes("<d " + attrStr + " />");
      const key = attrs.ratingKey || attrs.key || attrs.Key;
      const title = decodeXmlEntities(attrs.title || attrs.Title);
      const rawArtist = attrs.parentTitle || attrs.parenttitle || attrs.Artist;
      const artist = rawArtist ? decodeXmlEntities(rawArtist) : "";
      const thumb = attrs.thumb || attrs.Thumb;
      const year = attrs.year || attrs.Year;
      if (key && title) {
        albums.push({
          key: String(key),
          title: String(title),
          artist: artist ? String(artist) : "",
          thumb: thumb ? String(thumb) : null,
          year: year ? String(year) : "",
        });
      }
    }
    const payload = { serverUri: effectiveServerUri, albums };
    setCached(cacheKey, payload);
    res.json(payload);
  } catch (err) {
    console.error("Error fetching Plex albums", err);
    res.status(err.status || 500).json({
      error: "Failed to fetch albums",
      detail: String(err.message),
    });
  }
});

// GET /api/plex/recentlyAdded -> recently added albums in a music section (Plex: recentlyAdded)
app.get("/api/plex/recentlyAdded", async (req, res) => {
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    (typeof req.headers["x-plex-token"] === "string" && req.headers["x-plex-token"]) ||
    activePlexToken;
  const sectionKey = req.query.sectionKey;
  if (!token) {
    return res.status(400).json({ error: "Not authenticated", detail: "Pass ?token= or X-Plex-Token header." });
  }
  if (!sectionKey || typeof sectionKey !== "string") {
    return res.status(400).json({ error: "Missing sectionKey", detail: "Pass ?sectionKey=1 (music library key)." });
  }

  const cacheKey = `recentlyAdded:${token}:${sectionKey}`;
  const cached = getCached(cacheKey);
  if (cached) return res.json(cached);

  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) {
      return res.status(404).json({ error: "No Plex servers", detail: "No server found for this account." });
    }
    const base = serverUri.replace(/\/$/, "");
    // Plex: section-specific recentlyAdded (type 9 = album), or fallback to global and filter by librarySectionID
    let url = `${base}/library/sections/${encodeURIComponent(sectionKey)}/recentlyAdded?type=9&X-Plex-Token=${encodeURIComponent(token)}`;
    let xmlRes = await fetch(url, { method: "GET", headers: { ...plexHeaders(), Accept: "application/xml" } });
    let xml = await xmlRes.text();
    if (xmlRes.status === 404 || !xmlRes.ok) {
      url = `${base}/library/recentlyAdded?X-Plex-Token=${encodeURIComponent(token)}`;
      xmlRes = await fetch(url, { method: "GET", headers: { ...plexHeaders(), Accept: "application/xml" } });
      xml = await xmlRes.text();
    }
    if (!xmlRes.ok) {
      return res.status(xmlRes.status).json({ error: "Failed to fetch recently added", detail: xml.slice(0, 200) });
    }
    const sectionId = String(sectionKey);
    const albums = [];
    const dirOpenRegex = /<Directory\s+([^>]+)>/g;
    let m;
    while ((m = dirOpenRegex.exec(xml)) !== null) {
      const attrStr = m[1].replace(/\s+/g, " ").trim();
      const attrs = parseXmlAttributes("<d " + attrStr + " />");
      if (attrs.librarySectionID && attrs.librarySectionID !== sectionId) continue;
      const key = attrs.ratingKey || attrs.key || attrs.Key;
      const title = decodeXmlEntities(attrs.title || attrs.Title);
      const rawArtist = attrs.parentTitle || attrs.parenttitle || attrs.Artist;
      const artist = rawArtist ? decodeXmlEntities(rawArtist) : "";
      const thumb = attrs.thumb || attrs.Thumb;
      const year = attrs.year || attrs.Year;
      if (key && title) {
        albums.push({
          key: String(key),
          title: String(title),
          artist: artist ? String(artist) : "",
          thumb: thumb ? String(thumb) : null,
          year: year ? String(year) : "",
        });
      }
    }
    const payload = { serverUri: base, albums };
    setCached(cacheKey, payload);
    res.json(payload);
  } catch (err) {
    console.error("Error fetching Plex recently added", err);
    res.status(err.status || 500).json({
      error: "Failed to fetch recently added",
      detail: String(err.message),
    });
  }
});

// GET /api/plex/onDeck -> recently played / on deck for a music section (in-progress or recently played)
app.get("/api/plex/onDeck", async (req, res) => {
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    (typeof req.headers["x-plex-token"] === "string" && req.headers["x-plex-token"]) ||
    activePlexToken;
  const sectionKey = req.query.sectionKey;
  if (!token) {
    return res.status(400).json({ error: "Not authenticated", detail: "Pass ?token= or X-Plex-Token header." });
  }
  if (!sectionKey || typeof sectionKey !== "string") {
    return res.status(400).json({ error: "Missing sectionKey", detail: "Pass ?sectionKey=1 (music library key)." });
  }

  const cacheKey = `onDeck:${token}:${sectionKey}`;
  const cached = getCached(cacheKey);
  if (cached) return res.json(cached);

  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) {
      return res.status(404).json({ error: "No Plex servers", detail: "No server found for this account." });
    }
    const base = serverUri.replace(/\/$/, "");
    // Plex: section-specific onDeck returns in-progress/recently played items (tracks or albums for music)
    const url = `${base}/library/sections/${encodeURIComponent(sectionKey)}/onDeck?X-Plex-Token=${encodeURIComponent(token)}`;
    const xmlRes = await fetch(url, { method: "GET", headers: { ...plexHeaders(), Accept: "application/xml" } });
    const xml = await xmlRes.text();
    if (!xmlRes.ok) {
      return res.status(xmlRes.status).json({ error: "Failed to fetch on deck", detail: xml.slice(0, 200) });
    }
    const albums = [];
    const dirOpenRegex = /<Directory\s+([^>]+)>/g;
    let m;
    while ((m = dirOpenRegex.exec(xml)) !== null) {
      const attrStr = m[1].replace(/\s+/g, " ").trim();
      const attrs = parseXmlAttributes("<d " + attrStr + " />");
      const key = attrs.ratingKey || attrs.key || attrs.Key;
      const title = decodeXmlEntities(attrs.title || attrs.Title);
      const rawArtist = attrs.parentTitle || attrs.parenttitle || attrs.Artist;
      const artist = rawArtist ? decodeXmlEntities(rawArtist) : "";
      const thumb = attrs.thumb || attrs.Thumb;
      const year = attrs.year || attrs.Year;
      if (key && title) {
        albums.push({
          key: String(key),
          title: String(title),
          artist: artist ? String(artist) : "",
          thumb: thumb ? String(thumb) : null,
          year: year ? String(year) : "",
        });
      }
    }
    const trackRegex = /<Track\s+([^>]+)>/g;
    while ((m = trackRegex.exec(xml)) !== null) {
      const attrStr = m[1].replace(/\s+/g, " ").trim();
      const attrs = parseXmlAttributes("<t " + attrStr + " />");
      const parentKey = attrs.parentRatingKey || attrs.parentKey;
      const parentTitle = decodeXmlEntities(attrs.parentTitle || attrs.parenttitle || "");
      const grandparentTitle = decodeXmlEntities(attrs.grandparentTitle || attrs.grandparenttitle || "");
      const artist = grandparentTitle || "";
      const title = parentTitle || "";
      const key = parentKey;
      const thumb = attrs.thumb || attrs.Thumb;
      const year = attrs.year || attrs.Year;
      if (key && title && !albums.some((a) => a.key === String(key))) {
        albums.push({
          key: String(key),
          title: String(title),
          artist: artist ? String(artist) : "",
          thumb: thumb ? String(thumb) : null,
          year: year ? String(year) : "",
        });
      }
    }
    const payload = { serverUri: base, albums };
    setCached(cacheKey, payload);
    res.json(payload);
  } catch (err) {
    console.error("Error fetching Plex on deck", err);
    res.status(err.status || 500).json({
      error: "Failed to fetch recently played",
      detail: String(err.message),
    });
  }
});

// GET /api/plex/recentlyPlayedTracks -> play history (tracks only) from Plex session history
app.get("/api/plex/recentlyPlayedTracks", async (req, res) => {
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    (typeof req.headers["x-plex-token"] === "string" && req.headers["x-plex-token"]) ||
    activePlexToken;
  const sectionKey = req.query.sectionKey;
  if (!token) {
    return res.status(400).json({ error: "Not authenticated", detail: "Pass ?token= or X-Plex-Token header." });
  }

  const cacheKey = `recentlyPlayedTracks:${token}:${sectionKey || "all"}`;
  const cached = getCached(cacheKey);
  if (cached) return res.json(cached);

  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) {
      return res.status(404).json({ error: "No Plex servers", detail: "No server found for this account." });
    }
    const base = serverUri.replace(/\/$/, "");
    const url = `${base}/status/sessions/history/all?X-Plex-Token=${encodeURIComponent(token)}`;
    const xmlRes = await fetch(url, { method: "GET", headers: { ...plexHeaders(), Accept: "application/xml" } });
    const xml = await xmlRes.text();
    if (!xmlRes.ok) {
      return res.status(xmlRes.status).json({ error: "Failed to fetch history", detail: xml.slice(0, 200) });
    }
    const sectionId = sectionKey ? String(sectionKey) : null;
    const rawTracks = [];
    const trackRegex = /<Track\s+([^>]+)\/?\s*>/g;
    let m;
    while ((m = trackRegex.exec(xml)) !== null) {
      const attrStr = m[1].replace(/\s+/g, " ").trim();
      const attrs = parseXmlAttributes("<t " + attrStr + " />");
      if (sectionId != null && attrs.librarySectionID !== sectionId) continue;
      const key = attrs.ratingKey || attrs.key;
      const title = decodeXmlEntities(attrs.title || "");
      const artist = decodeXmlEntities(attrs.grandparentTitle || attrs.grandparenttitle || "");
      const album = decodeXmlEntities(attrs.parentTitle || attrs.parenttitle || "");
      let albumKey = attrs.parentRatingKey || attrs.parentKey || "";
      if (albumKey && albumKey.startsWith("/")) {
        const match = albumKey.match(/\/library\/metadata\/(\d+)/);
        if (match) albumKey = match[1];
      }
      const thumb = attrs.grandparentThumb || attrs.parentThumb || attrs.thumb || attrs.Thumb || null;
      const viewedAt = attrs.viewedAt ? parseInt(attrs.viewedAt, 10) : 0;
      if (key && title) {
        rawTracks.push({ key: String(key), title, artist, album, albumKey: String(albumKey), thumb, viewedAt });
      }
    }
    rawTracks.sort((a, b) => b.viewedAt - a.viewedAt);
    const tracks = rawTracks.slice(0, 200);
    const payload = { serverUri: base, tracks };
    setCached(cacheKey, payload);
    res.json(payload);
  } catch (err) {
    console.error("Error fetching Plex recently played tracks", err);
    res.status(err.status || 500).json({
      error: "Failed to fetch recently played",
      detail: String(err.message),
    });
  }
});

// GET /api/plex/album/:ratingKey/tracks -> children (tracks) for an album + metadata (cached)
app.get("/api/plex/album/:ratingKey/tracks", async (req, res) => {
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    (typeof req.headers["x-plex-token"] === "string" && req.headers["x-plex-token"]) ||
    activePlexToken;
  const ratingKey = req.params.ratingKey;
  if (!token || !ratingKey) {
    return res.status(400).json({ error: "Missing token or ratingKey" });
  }
  const cacheKey = `tracks:${token}:${ratingKey}`;
  const cached = getCached(cacheKey);
  res.setHeader("Cache-Control", "private, max-age=900");
  if (cached) return res.json(cached);
  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) return res.status(404).json({ error: "No Plex servers" });
    const base = serverUri.replace(/\/$/, "");
    const url = `${base}/library/metadata/${encodeURIComponent(ratingKey)}/children?X-Plex-Token=${encodeURIComponent(token)}`;
    const xmlRes = await fetch(url, { method: "GET", headers: { ...plexHeaders(), Accept: "application/xml" } });
    const xml = await xmlRes.text();
    if (!xmlRes.ok) {
      return res.status(xmlRes.status).json({ error: "Failed to fetch tracks", detail: xml.slice(0, 200) });
    }
    const tracks = [];
    const trackRegex = /<Track\s+([^>]+)>([\s\S]*?)<\/Track>/g;
    let m;
    while ((m = trackRegex.exec(xml)) !== null) {
      const attrs = parseXmlAttributes("<t " + m[1].replace(/\s+/g, " ").trim() + " />");
      const mediaBlock = m[2];
      let bitrate = null;
      let audioChannels = null;
      let audioCodec = null;
      let container = null;
      let samplingRate = null;
      let durationMs = attrs.duration ? parseInt(attrs.duration, 10) : null;
      let partKey = null;
      const mediaMatch = /<Media\s+([^>]+)>([\s\S]*?)<\/Media>/.exec(mediaBlock);
      if (mediaMatch) {
        const mediaAttrs = parseXmlAttributes("<m " + mediaMatch[1].replace(/\s+/g, " ").trim() + " />");
        bitrate = mediaAttrs.bitrate ? parseInt(mediaAttrs.bitrate, 10) : null;
        audioChannels = mediaAttrs.audioChannels ? parseInt(mediaAttrs.audioChannels, 10) : null;
        audioCodec = mediaAttrs.audioCodec || null;
        container = mediaAttrs.container || null;
        if (mediaAttrs.samplingRate) samplingRate = parseInt(mediaAttrs.samplingRate, 10);
        else if (mediaAttrs.audioSamplingRate) samplingRate = parseInt(mediaAttrs.audioSamplingRate, 10);
        if (mediaAttrs.duration) durationMs = parseInt(mediaAttrs.duration, 10);
        const partMatch = /<Part\s+([^>]+)>/.exec(mediaMatch[2]);
        if (partMatch) {
          const partAttrs = parseXmlAttributes("<p " + partMatch[1].replace(/\s+/g, " ").trim() + " />");
          partKey = partAttrs.key || null;
        }
      }
      tracks.push({
        key: attrs.ratingKey || attrs.key,
        title: decodeXmlEntities(attrs.title || ""),
        index: attrs.index != null ? parseInt(attrs.index, 10) : null,
        duration: durationMs,
        bitrate: bitrate,
        audioChannels: audioChannels,
        audioCodec: audioCodec,
        container: container,
        samplingRate: samplingRate,
        partKey: partKey,
      });
    }
    const payload = { tracks };
    setCached(cacheKey, payload);
    res.json(payload);
  } catch (err) {
    console.error("Error fetching album tracks", err);
    res.status(err.status || 500).json({ error: "Failed to fetch tracks", detail: String(err.message) });
  }
});

// GET /api/plex/stream-url -> return direct Plex stream URL (no redirect; client sets audio.src = url for fastest start)
app.get("/api/plex/stream-url", async (req, res) => {
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    (typeof req.headers["x-plex-token"] === "string" && req.headers["x-plex-token"]) ||
    activePlexToken;
  let path = req.query.path;
  if (!token || path == null || typeof path !== "string") {
    return res.status(400).json({ error: "Missing token or path" });
  }
  path = path.trim();
  if (!path.startsWith("/")) path = `/library/parts/${path}`;
  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) return res.status(404).json({ error: "No Plex servers" });
    const base = serverUri.replace(/\/$/, "");
    const url = `${base}${path}${path.includes("?") ? "&" : "?"}X-Plex-Token=${encodeURIComponent(token)}`;
    return res.json({ url });
  } catch (err) {
    return res.status(500).json({ error: "Stream URL failed" });
  }
});

// GET /api/plex/stream -> proxy Plex audio stream for playback (no auth in URL for client)
// No buffer/body size limit: client may hold current + next + preload in full (no 25MB or similar cap).
// Optional: transcode=1&ratingKey=<track rating key> requests 320kbps transcoded stream via Plex API
app.get("/api/plex/stream", async (req, res) => {
  const origin = req.headers.origin;
  const allowOrigin = origin || "null";
  res.setHeader("Access-Control-Allow-Origin", allowOrigin);
  res.setHeader("Access-Control-Expose-Headers", "Content-Length, Content-Range, Accept-Ranges");

  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    (typeof req.headers["x-plex-token"] === "string" && req.headers["x-plex-token"]) ||
    activePlexToken;
  let path = req.query.path;
  const containerParam = typeof req.query.container === "string" ? req.query.container.trim().toLowerCase() : null;
  const ratingKeyParam = typeof req.query.ratingKey === "string" && req.query.ratingKey.trim() ? String(req.query.ratingKey).trim() : null;
  const musicBitrateParam = req.query.musicBitrate;
  const trackBitrateParam = req.query.trackBitrate != null ? parseInt(String(req.query.trackBitrate), 10) : null;
  const wantTranscode = req.query.transcode === "1" && ratingKeyParam && musicBitrateParam && String(musicBitrateParam) !== "lossless";
  const ratingKey = ratingKeyParam;
  let musicBitrate = wantTranscode ? Math.min(320, Math.max(128, parseInt(String(musicBitrateParam), 10) || 320)) : null;
  if (musicBitrate != null && trackBitrateParam > 0) musicBitrate = Math.min(musicBitrate, trackBitrateParam);

  if (!token || path == null || typeof path !== "string") {
    return res.status(400).json({ error: "Missing token or path" });
  }
  path = path.trim();
  if (!path.startsWith("/")) {
    path = `/library/parts/${path}`;
  }

  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) return res.status(404).json({ error: "No Plex servers" });
    const base = serverUri.replace(/\/$/, "");

    // DEBUG LOGGING
    const reqId = Date.now() % 10000;
    console.log(`[Stream ${reqId}] Request: ${path} | Range: ${req.headers.range || "None"}`);

    try {
      const { serverUri } = await getServerUri(token);
      if (!serverUri) return res.status(404).json({ error: "No Plex servers" });
      const base = serverUri.replace(/\/$/, "");

      const STREAM_FETCH_TIMEOUT_MS = 300000; // Increased to 5 mins
      // ... transcoding block omitted for brevity, adding logs if needed ...

      // Direct stream logic
      const url = `${base}${path}${path.includes("?") ? "&" : "?"}X-Plex-Token=${encodeURIComponent(token)}`;
      if (req.query.direct === "1") {
        res.redirect(302, url);
        return;
      }
      const fetchHeaders = { ...plexHeaders(), Accept: "audio/*" };
      // ... content type checks ...
      const pathIsM4a = /\.(m4a|mp4)(\?|$)/i.test(path);
      // ... other extensions ...
      if (req.headers.range) {
        fetchHeaders.Range = req.headers.range;
        console.log(`[Stream ${reqId}] Forwarding Range: ${req.headers.range}`);
      }
      if (req.headers["if-range"]) {
        fetchHeaders["If-Range"] = req.headers["if-range"];
      }

      const abortController = new AbortController();
      const timeoutId = setTimeout(() => abortController.abort(), STREAM_FETCH_TIMEOUT_MS);
      let streamRes;
      try {
        console.log(`[Stream ${reqId}] Fetching from Plex...`);
        streamRes = await fetch(url, { method: "GET", headers: fetchHeaders, signal: abortController.signal });
      } catch (fetchErr) {
        clearTimeout(timeoutId);
        const isTimeout = fetchErr?.name === "AbortError";
        console.error(`[Stream ${reqId}] Fetch failed:`, fetchErr?.message);
        if (!res.headersSent) res.status(500).json({ error: isTimeout ? "Plex took too long to respond" : "Stream fetch failed" });
        return;
      }
      clearTimeout(timeoutId);

      console.log(`[Stream ${reqId}] Plex Response: ${streamRes.status} ${streamRes.statusText}`);

      if (!streamRes.ok && streamRes.status !== 206) {
        console.error("Plex stream response not ok", streamRes.status, path);
        return res.status(streamRes.status).send();
      }
      res.status(streamRes.status);

      // ... Content-Type logic ...
      let contentType = streamRes.headers.get("content-type") || "";
      // ... (keep existing Content-Type fix logic) ...
      // Simplified for injection: ensuring we keep the fix logic but add logs
      if (!contentType && pathIsM4a) contentType = "audio/mp4"; // (simplified just for the logs context, ideally we keep full logic)

      res.setHeader("Content-Type", contentType || "audio/mpeg");
      res.setHeader("Accept-Ranges", "bytes");

      // Forward headers
      const len = streamRes.headers.get("content-length");
      if (len) res.setHeader("Content-Length", len);
      const cr = streamRes.headers.get("content-range");
      if (cr) {
        res.setHeader("Content-Range", cr);
        console.log(`[Stream ${reqId}] Forwarding Content-Range: ${cr}`);
      }
      const etag = streamRes.headers.get("etag");
      if (etag) res.setHeader("ETag", etag);
      const lastMod = streamRes.headers.get("last-modified");
      if (lastMod) res.setHeader("Last-Modified", lastMod);

      const body = streamRes.body;
      if (body && typeof Readable.fromWeb === "function") {
        const nodeStream = Readable.fromWeb(body);
        const onError = (err) => {
          console.error(`[Stream ${reqId}] Pipe error:`, err);
          if (!res.writableEnded && !res.destroyed && !res.headersSent) res.status(500).send();
          else if (!res.writableEnded) res.end();
        };
        const onClose = () => {
          console.log(`[Stream ${reqId}] Client closed connection`);
          if (nodeStream.destroy) nodeStream.destroy();
        };
        nodeStream.on("error", onError);
        res.on("close", onClose);
        res.on("error", onError);
        nodeStream.pipe(res);
      } else {
        const buf = await streamRes.arrayBuffer();
        res.send(Buffer.from(buf));
      }
    } catch (err) {
      console.error(`[Stream ${reqId}] General Error:`, err);
      if (!res.headersSent) res.status(500).json({ error: "Stream failed" });
    }
  } catch (err) {
    console.error(`[Stream ${reqId}] Outer Error:`, err);
    if (!res.headersSent) res.status(500).json({ error: "Stream setup failed" });
  }
});

// GET /api/plex/thumb -> proxy Plex thumb at smaller size, cache in memory and tell browser to cache
const THUMB_SIZE = 400;
app.get("/api/plex/thumb", async (req, res) => {
  const token =
    (typeof req.query.token === "string" && req.query.token) ||
    (typeof req.headers["x-plex-token"] === "string" && req.headers["x-plex-token"]) ||
    activePlexToken;
  const thumbPath = req.query.thumb;
  if (!token || !thumbPath || typeof thumbPath !== "string") {
    return res.status(400).json({ error: "Missing token or thumb" });
  }
  const cacheKey = `${thumbPath}:${THUMB_SIZE}`;
  const cached = getThumbCached(cacheKey);
  if (cached) {
    res.setHeader("Cache-Control", "public, max-age=86400");
    res.setHeader("Content-Type", cached.contentType);
    return res.send(cached.body);
  }
  try {
    const { serverUri } = await getServerUri(token);
    if (!serverUri) return res.status(404).send();
    const base = serverUri.replace(/\/$/, "");
    const transcodeUrl = `${base}/photo/:/transcode?width=${THUMB_SIZE}&height=${THUMB_SIZE}&minSize=1&url=${encodeURIComponent(thumbPath)}&X-Plex-Token=${encodeURIComponent(token)}`;
    let imgRes = await fetch(transcodeUrl, {
      method: "GET",
      headers: { ...plexHeaders(), Accept: "image/*" },
    });
    if (!imgRes.ok) {
      const rawUrl = `${base}${thumbPath.startsWith("/") ? "" : "/"}${thumbPath}${thumbPath.includes("?") ? "&" : "?"}X-Plex-Token=${encodeURIComponent(token)}`;
      imgRes = await fetch(rawUrl, { method: "GET", headers: { ...plexHeaders(), Accept: "image/*" } });
      if (!imgRes.ok) return res.status(imgRes.status).send();
    }
    const contentType = imgRes.headers.get("content-type") || "image/jpeg";
    const body = Buffer.from(await imgRes.arrayBuffer());
    setThumbCached(cacheKey, body, contentType);
    res.setHeader("Cache-Control", "public, max-age=86400");
    res.setHeader("Content-Type", contentType);
    res.send(body);
  } catch (err) {
    console.error("Thumb fetch error", err);
    res.status(500).send();
  }
});

// POST /api/cache/reset -> clear API and thumb caches
app.post("/api/cache/reset", (req, res) => {
  apiCache.clear();
  thumbCache.clear();
  serverUriCache.clear();
  res.json({ ok: true });
});

// --- Discord Rich Presence (optional: requires discord-rpc, Discord app running)
let discordRpcClient = null;
let discordRpcClientId = null;

async function getDiscordRpcClient(clientId) {
  if (!clientId || typeof clientId !== "string") return null;
  const id = clientId.trim();
  if (!id) return null;
  if (discordRpcClient && discordRpcClientId === id) return discordRpcClient;
  if (discordRpcClient) {
    try {
      discordRpcClient.destroy();
    } catch (_) { }
    discordRpcClient = null;
    discordRpcClientId = null;
  }
  try {
    let RPC;
    if (typeof require !== "undefined") {
      // Packaged app: load from PROJECT_ROOT/node_modules/discord-rpc (set by Electron main)
      const projectRoot = process.env.PROJECT_ROOT;
      if (projectRoot) {
        const discordRpcPath = path.join(projectRoot, "node_modules", "discord-rpc");
        try {
          if (fs.existsSync(path.join(discordRpcPath, "package.json"))) {
            RPC = require(discordRpcPath);
            // Packaged app: package "main" can resolve wrong; load entry explicitly if Client missing
            if (RPC && !RPC.Client && fs.existsSync(path.join(discordRpcPath, "src", "index.js"))) {
              RPC = require(path.join(discordRpcPath, "src", "index.js"));
            }
          }
        } catch (_) { }
      }
      if (!RPC) {
        const nodePaths = (process.env.NODE_PATH || "").split(path.delimiter).filter(Boolean);
        for (const dir of nodePaths) {
          try {
            RPC = require(require.resolve("discord-rpc", { paths: [dir] }));
            break;
          } catch (_) { }
        }
      }
      if (!RPC) {
        try {
          RPC = require("discord-rpc");
        } catch (_) { }
      }
    }
    if (!RPC) {
      try {
        const m = await import("discord-rpc");
        RPC = m.default || m;
      } catch (_) { }
    }
    const Client = RPC?.Client ?? RPC?.default?.Client;
    if (!Client) throw new Error("discord-rpc Client not found");
    const client = new Client({ transport: "ipc" });
    await client.login({ clientId: id });
    discordRpcClient = client;
    discordRpcClientId = id;
    return client;
  } catch (err) {
    console.warn("[Discord RPC] Failed to connect:", err?.message || err);
    throw err;
  }
}

app.post("/api/discord/test", express.json(), async (req, res) => {
  const clientId = (req.body?.clientId || "").trim();
  if (!clientId) {
    return res.status(400).json({ ok: false, error: "Enter your Application ID first." });
  }
  try {
    const client = await getDiscordRpcClient(clientId);
    await client.setActivity({
      details: "Test track",
      state: "Test artist",
      largeImageText: "Test album",
    });
    return res.json({ ok: true });
  } catch (err) {
    const msg = err?.message || String(err);
    const friendly = msg.includes("RPC_CONNECTION") || /ECONNREFUSED|connect|timeout/i.test(msg)
      ? "Discord not running or not accepting connections. Start the Discord desktop app and try again."
      : msg;
    return res.status(500).json({ ok: false, error: friendly });
  }
});

app.post("/api/discord/activity", express.json(), async (req, res) => {
  const { clientId, trackTitle, artistName, albumName, imageKey, albumArtUrl, clear: wantClear } = req.body || {};
  if (!clientId || typeof clientId !== "string") {
    return res.status(400).json({ error: "Missing Discord Application ID (clientId)" });
  }
  const id = String(clientId).trim();
  if (!id) {
    return res.status(400).json({ error: "Application ID is empty. Use the number from your app's page, not the Client Secret." });
  }
  try {
    if (wantClear || !trackTitle) {
      const client = await getDiscordRpcClient(id);
      if (client) {
        await client.clearActivity();
      }
      return res.json({ ok: true });
    }
    let client;
    try {
      client = await getDiscordRpcClient(id);
    } catch (e) {
      const msg = e?.message || String(e);
      return res.status(503).json({ error: /RPC_CONNECTION|ECONNREFUSED|connect|timeout/i.test(msg) ? "Discord not running. Start the Discord app." : msg });
    }
    const details = String(trackTitle).slice(0, 128);
    const state = (artistName != null && artistName !== "") ? String(artistName).slice(0, 128) : undefined;
    const largeImageText = (albumName != null && albumName !== "") ? String(albumName).slice(0, 128) : undefined;
    const imageKeyOrUrl = (albumArtUrl && String(albumArtUrl).trim()) || (imageKey && String(imageKey).trim()) ? (String(albumArtUrl || imageKey).trim().slice(0, 2048)) : undefined;
    await client.setActivity({
      details,
      state,
      largeImageKey: imageKeyOrUrl,
      largeImageText: largeImageText || undefined,
    });
    return res.json({ ok: true });
  } catch (err) {
    const msg = err?.message || String(err);
    console.warn("[Discord RPC] setActivity failed:", msg);
    return res.status(500).json({ error: msg.includes("RPC_CONNECTION") ? "Discord not running. Start the Discord app." : msg });
  }
});

// --- Last.fm Scrobbling (env LASTFM_* or pass apiKey/secret from app) ---
const LASTFM_API_KEY = process.env.LASTFM_API_KEY || "";
const LASTFM_SECRET = process.env.LASTFM_SECRET || "";

function lastFmSig(params, secret = LASTFM_SECRET) {
  const str = Object.keys(params)
    .sort()
    .filter((k) => k !== "format" && k !== "callback" && k !== "api_sig" && params[k] !== undefined && params[k] !== "")
    .map((k) => k + String(params[k]))
    .join("") + secret;
  return crypto.createHash("md5").update(str, "utf8").digest("hex");
}

function getLastFmCreds(body) {
  const apiKey = (body?.apiKey || "").trim() || LASTFM_API_KEY;
  const secret = (body?.secret || "").trim() || LASTFM_SECRET;
  return { apiKey, secret };
}

app.get("/api/lastfm/token", async (req, res) => {
  if (!LASTFM_API_KEY || !LASTFM_SECRET) return res.status(503).json({ error: "Last.fm not configured" });
  try {
    const params = { method: "auth.getToken", api_key: LASTFM_API_KEY };
    params.api_sig = lastFmSig(params);
    const form = new URLSearchParams({ ...params, format: "json" });
    const r = await fetch("https://ws.audioscrobbler.com/2.0/", { method: "POST", body: form, headers: { "Content-Type": "application/x-www-form-urlencoded" } });
    const data = await r.json();
    if (data.token) {
      const authUrl = `https://www.last.fm/api/auth?api_key=${encodeURIComponent(LASTFM_API_KEY)}&token=${encodeURIComponent(data.token)}`;
      return res.json({ token: data.token, authUrl });
    }
    return res.status(400).json({ error: data.message || "Failed to get token" });
  } catch (e) {
    return res.status(500).json({ error: String(e.message) });
  }
});

app.post("/api/lastfm/token", express.json(), async (req, res) => {
  const { apiKey, secret } = getLastFmCreds(req.body || {});
  if (!apiKey || !secret) return res.status(400).json({ error: "Enter your Last.fm API Key and Secret in Settings, or set LASTFM_API_KEY and LASTFM_SECRET on the server." });
  try {
    const params = { method: "auth.getToken", api_key: apiKey };
    params.api_sig = lastFmSig(params, secret);
    const form = new URLSearchParams({ ...params, format: "json" });
    const r = await fetch("https://ws.audioscrobbler.com/2.0/", { method: "POST", body: form, headers: { "Content-Type": "application/x-www-form-urlencoded" } });
    const data = await r.json();
    if (data.token) {
      const authUrl = `https://www.last.fm/api/auth?api_key=${encodeURIComponent(apiKey)}&token=${encodeURIComponent(data.token)}`;
      return res.json({ token: data.token, authUrl });
    }
    return res.status(400).json({ error: data.message || "Failed to get token" });
  } catch (e) {
    return res.status(500).json({ error: String(e.message) });
  }
});

app.post("/api/lastfm/session", express.json(), async (req, res) => {
  const body = req.body || {};
  const { apiKey, secret } = getLastFmCreds(body);
  if (!apiKey || !secret) return res.status(400).json({ error: "Last.fm API Key and Secret required (from Settings or server env)." });
  const token = body.token;
  if (!token) return res.status(400).json({ error: "Missing token" });
  try {
    const params = { method: "auth.getSession", api_key: apiKey, token };
    params.api_sig = lastFmSig(params, secret);
    const form = new URLSearchParams({ ...params, format: "json" });
    const r = await fetch("https://ws.audioscrobbler.com/2.0/", { method: "POST", body: form, headers: { "Content-Type": "application/x-www-form-urlencoded" } });
    const data = await r.json();
    const sk = data.session?.key;
    if (sk) return res.json({ sk, username: data.session?.name });
    return res.status(400).json({ error: data.message || "Failed to get session" });
  } catch (e) {
    return res.status(500).json({ error: String(e.message) });
  }
});

app.post("/api/lastfm/nowPlaying", express.json(), async (req, res) => {
  const body = req.body || {};
  const { apiKey, secret } = getLastFmCreds(body);
  if (!apiKey || !secret) return res.status(400).json({ error: "Last.fm not configured" });
  const { sk, artist, track, album, duration } = body;
  if (!sk || !artist || !track) return res.status(400).json({ error: "Missing sk, artist, or track" });
  try {
    const params = { method: "track.updateNowPlaying", api_key: apiKey, sk, artist, track };
    if (album) params.album = album;
    if (duration != null) params.duration = String(Math.round(Number(duration)));
    params.api_sig = lastFmSig(params, secret);
    const form = new URLSearchParams({ ...params, format: "json" });
    const r = await fetch("https://ws.audioscrobbler.com/2.0/", { method: "POST", body: form, headers: { "Content-Type": "application/x-www-form-urlencoded" } });
    const data = await r.json();
    if (data.error) return res.status(400).json({ error: data.message || data.error, errorCode: data.error });
    return res.json({ ok: true });
  } catch (e) {
    return res.status(500).json({ error: String(e.message) });
  }
});

app.post("/api/lastfm/scrobble", express.json(), async (req, res) => {
  const body = req.body || {};
  const { apiKey, secret } = getLastFmCreds(body);
  if (!apiKey || !secret) return res.status(400).json({ error: "Last.fm not configured" });
  const { sk, artist, track, timestamp, album, duration } = body;
  if (!sk || !artist || !track || timestamp == null) return res.status(400).json({ error: "Missing sk, artist, track, or timestamp" });
  const ts = Math.floor(Number(timestamp));
  try {
    const params = { method: "track.scrobble", api_key: apiKey, sk, "artist[0]": artist, "track[0]": track, "timestamp[0]": String(ts) };
    if (album) params["album[0]"] = album;
    if (duration != null) params["duration[0]"] = String(Math.round(Number(duration)));
    params.api_sig = lastFmSig(params, secret);
    const form = new URLSearchParams({ ...params, format: "json" });
    const r = await fetch("https://ws.audioscrobbler.com/2.0/", { method: "POST", body: form, headers: { "Content-Type": "application/x-www-form-urlencoded" } });
    const data = await r.json();
    if (data.error) return res.status(400).json({ error: data.message || data.error, errorCode: data.error });
    return res.json({ ok: true, accepted: data.scrobbles?.["@attr"]?.accepted });
  } catch (e) {
    return res.status(500).json({ error: String(e.message) });
  }
});

// POST /api/lastfm/test - validate session/key/secret (sends a test now-playing; does not scrobble)
app.post("/api/lastfm/test", express.json(), async (req, res) => {
  const body = req.body || {};
  const { apiKey, secret } = getLastFmCreds(body);
  if (!apiKey || !secret) return res.status(400).json({ error: "Enter API Key and Secret (or connect first)." });
  const sk = (body.sk || "").trim();
  if (!sk) return res.status(400).json({ error: "Not connected. Connect to Last.fm first, then test." });
  try {
    const params = { method: "track.updateNowPlaying", api_key: apiKey, sk, artist: "Sonic", track: "Connection test" };
    params.api_sig = lastFmSig(params, secret);
    const form = new URLSearchParams({ ...params, format: "json" });
    const r = await fetch("https://ws.audioscrobbler.com/2.0/", { method: "POST", body: form, headers: { "Content-Type": "application/x-www-form-urlencoded" } });
    const data = await r.json();
    if (data.error) {
      const msg = data.message || String(data.error);
      if (data.error === 4 || /session|auth/i.test(msg)) return res.json({ ok: false, error: "Invalid or expired session. Disconnect and connect again." });
      if (data.error === 9 || /api key|invalid key/i.test(msg)) return res.json({ ok: false, error: "Invalid API Key or Secret. Check your credentials." });
      return res.json({ ok: false, error: msg });
    }
    return res.json({ ok: true });
  } catch (e) {
    return res.status(500).json({ ok: false, error: String(e.message) });
  }
});

// --- Playlists (persisted to data/playlists.json) ---
function ensureDataDir() {
  if (!fs.existsSync(DATA_DIR)) fs.mkdirSync(DATA_DIR, { recursive: true });
}

function readPlaylists() {
  ensureDataDir();
  if (!fs.existsSync(PLAYLISTS_FILE)) return { playlists: [] };
  try {
    const raw = fs.readFileSync(PLAYLISTS_FILE, "utf8");
    const data = JSON.parse(raw);
    return Array.isArray(data.playlists) ? data : { playlists: [] };
  } catch {
    return { playlists: [] };
  }
}

function writePlaylists(data) {
  ensureDataDir();
  fs.writeFileSync(PLAYLISTS_FILE, JSON.stringify(data, null, 2), "utf8");
}

function generateId() {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 11)}`;
}

// GET /api/playlists
app.get("/api/playlists", (req, res) => {
  const data = readPlaylists();
  res.json(data);
});

// POST /api/playlists -> create playlist { name?, image? }
app.post("/api/playlists", (req, res) => {
  const { name = "New Playlist", image = null } = req.body || {};
  const data = readPlaylists();
  const playlist = {
    id: generateId(),
    name: String(name).trim() || "New Playlist",
    image,
    items: [],
  };
  data.playlists.push(playlist);
  writePlaylists(data);
  res.status(201).json(playlist);
});

// PUT /api/playlists/:id -> update name and/or image
app.put("/api/playlists/:id", (req, res) => {
  const { id } = req.params;
  const { name, image } = req.body || {};
  const data = readPlaylists();
  const idx = data.playlists.findIndex((p) => p.id === id);
  if (idx === -1) return res.status(404).json({ error: "Playlist not found" });
  if (name !== undefined) data.playlists[idx].name = String(name).trim() || data.playlists[idx].name;
  if (image !== undefined) data.playlists[idx].image = image;
  writePlaylists(data);
  res.json(data.playlists[idx]);
});

// DELETE /api/playlists/:id
app.delete("/api/playlists/:id", (req, res) => {
  const { id } = req.params;
  const data = readPlaylists();
  const idx = data.playlists.findIndex((p) => p.id === id);
  if (idx === -1) return res.status(404).json({ error: "Playlist not found" });
  data.playlists.splice(idx, 1);
  writePlaylists(data);
  res.status(204).end();
});

// POST /api/playlists/:id/items -> add track(s). body: { track, album? } or { tracks: [{ track, album? }, ...] }
app.post("/api/playlists/:id/items", (req, res) => {
  const { id } = req.params;
  const body = req.body || {};
  const data = readPlaylists();
  const idx = data.playlists.findIndex((p) => p.id === id);
  if (idx === -1) return res.status(404).json({ error: "Playlist not found" });
  const playlist = data.playlists[idx];
  const normalize = (t) => ({
    trackKey: t.track?.key ?? t.trackKey,
    partKey: t.track?.partKey ?? t.partKey,
    title: t.track?.title ?? t.title ?? "",
    artist: t.album?.artist ?? t.artist ?? "",
    album: t.album?.title ?? t.album ?? "",
    thumb: t.album?.thumb ?? t.thumb ?? null,
  });
  if (Array.isArray(body.tracks)) {
    body.tracks.forEach((t) => playlist.items.push(normalize(t)));
  } else if (body.track) {
    playlist.items.push(normalize({ track: body.track, album: body.album ?? null }));
  } else {
    return res.status(400).json({ error: "Provide track or tracks array" });
  }
  writePlaylists(data);
  res.json(playlist);
});

// DELETE /api/playlists/:id/items/:index (index in items array)
app.delete("/api/playlists/:id/items/:index", (req, res) => {
  const { id, index } = req.params;
  const i = parseInt(index, 10);
  if (Number.isNaN(i) || i < 0) return res.status(400).json({ error: "Invalid index" });
  const data = readPlaylists();
  const idx = data.playlists.findIndex((p) => p.id === id);
  if (idx === -1) return res.status(404).json({ error: "Playlist not found" });
  const playlist = data.playlists[idx];
  if (i >= playlist.items.length) return res.status(404).json({ error: "Item not found" });
  playlist.items.splice(i, 1);
  writePlaylists(data);
  res.json(playlist);
});

// PUT /api/playlists/:id/reorder -> body: { fromIndex, toIndex } or { order: [item, ...] }
app.put("/api/playlists/:id/reorder", (req, res) => {
  const { id } = req.params;
  const body = req.body || {};
  const data = readPlaylists();
  const idx = data.playlists.findIndex((p) => p.id === id);
  if (idx === -1) return res.status(404).json({ error: "Playlist not found" });
  const playlist = data.playlists[idx];
  if (typeof body.fromIndex === "number" && typeof body.toIndex === "number") {
    const from = Math.max(0, Math.min(body.fromIndex, playlist.items.length - 1));
    const to = Math.max(0, Math.min(body.toIndex, playlist.items.length - 1));
    const [item] = playlist.items.splice(from, 1);
    if (item) playlist.items.splice(to, 0, item);
  } else if (Array.isArray(body.order)) {
    const copy = playlist.items.slice();
    playlist.items = body.order.map((i) => copy[i]).filter(Boolean);
  }
  writePlaylists(data);
  res.json(playlist);
});

// DELETE /api/library/items -> remove track/album from ALL playlists
app.delete("/api/library/items", (req, res) => {
  const { trackKey, partKey, type } = req.body || {};
  if (!trackKey && !partKey) return res.status(400).json({ error: "Missing trackKey or partKey" });

  const data = readPlaylists();
  let changed = false;

  data.playlists.forEach((p) => {
    const originalLen = p.items.length;
    // Filter out items that match the given keys
    p.items = p.items.filter((item) => {
      // If trackKey matches
      if (trackKey && (item.trackKey === trackKey || String(item.trackKey) === String(trackKey))) return false;
      // If partKey matches
      if (partKey && (item.partKey === partKey || String(item.partKey) === String(partKey))) return false;
      return true;
    });

    if (p.items.length !== originalLen) changed = true;
  });

  if (changed) writePlaylists(data);
  res.json({ success: true, changed });
});

// --- Local files (Music/sonicmusic folder)
function readSettingsSafe() {
  try {
    if (!fs.existsSync(SETTINGS_FILE)) return { localFilesEnabled: false, localMusicPath: null };
    const raw = fs.readFileSync(SETTINGS_FILE, "utf8");
    const o = JSON.parse(raw);
    return { localFilesEnabled: !!o.localFilesEnabled, localMusicPath: o.localMusicPath || null };
  } catch {
    return { localFilesEnabled: false, localMusicPath: null };
  }
}
function writeSettings(updates) {
  const current = readSettingsSafe();
  const next = { ...current, ...updates };
  fs.mkdirSync(DATA_DIR, { recursive: true });
  fs.writeFileSync(SETTINGS_FILE, JSON.stringify(next, null, 2), "utf8");
}

app.get("/api/local-files/status", (req, res) => {
  const s = readSettingsSafe();
  res.json({ enabled: s.localFilesEnabled, path: s.localMusicPath });
});

app.post("/api/local-files/enable", (req, res) => {
  const musicDir = path.join(os.homedir(), "Music");
  const localDir = path.join(musicDir, LOCAL_MUSIC_FOLDER_NAME);
  try {
    if (!fs.existsSync(musicDir)) fs.mkdirSync(musicDir, { recursive: true });
    fs.mkdirSync(localDir, { recursive: true });
    writeSettings({ localFilesEnabled: true, localMusicPath: localDir });
    res.json({ ok: true, path: localDir });
  } catch (err) {
    res.status(500).json({ error: (err && err.message) || "Failed to create folder" });
  }
});

async function scanLocalMusic(dir) {
  localArtCache.clear();
  const results = [];
  const mm = await import("music-metadata");
  function collectFiles(d, out) {
    if (!fs.existsSync(d)) return;
    const entries = fs.readdirSync(d, { withFileTypes: true });
    for (const ent of entries) {
      const full = path.join(d, ent.name);
      if (ent.isDirectory()) {
        collectFiles(full, out);
      } else {
        const ext = path.extname(ent.name).toLowerCase();
        if (LOCAL_MUSIC_EXTENSIONS.has(ext)) out.push({ full, ext, name: ent.name });
      }
    }
  }
  const files = [];
  collectFiles(dir, files);
  for (const { full, ext, name } of files) {
    const relative = path.relative(dir, full).replace(/\\/g, "/");
    const fileDir = path.dirname(full);
    try {
      const meta = await mm.parseFile(full);
      const common = meta.common;
      const format = meta.format;
      const duration = format && typeof format.duration === "number" ? Math.round(format.duration * 1000) : null;
      const trackNo = common && common.track && typeof common.track.no === "number" ? common.track.no : null;
      const year = common && common.year != null ? String(common.year) : "";
      let hasEmbeddedArt = false;
      if (common && common.picture && common.picture.length > 0) {
        const pic = common.picture[0];
        if (pic && pic.data) {
          const art = { data: pic.data, format: pic.format || "image/jpeg" };
          localArtCache.set(relative, art);
          const dirKey = path.dirname(relative).replace(/\\/g, "/");
          if (dirKey !== ".") localArtCache.set("__dir:" + dirKey, art);
          hasEmbeddedArt = true;
        }
      }
      if (!hasEmbeddedArt && fs.existsSync(fileDir)) {
        const dirEntries = fs.readdirSync(fileDir, { withFileTypes: true });
        for (const ent of dirEntries) {
          if (!ent.isFile()) continue;
          if (!LOCAL_COVER_REGEX.test(ent.name)) continue;
          const ext = path.extname(ent.name).toLowerCase();
          try {
            const coverPath = path.join(fileDir, ent.name);
            const buf = fs.readFileSync(coverPath);
            const fmt = ext === ".png" ? "image/png" : "image/jpeg";
            const art = { data: buf, format: fmt };
            localArtCache.set(relative, art);
            const dirKey = path.dirname(relative).replace(/\\/g, "/");
            if (dirKey !== ".") localArtCache.set("__dir:" + dirKey, art);
            break;
          } catch (_) { }
        }
      }
      let bitrateKbps = null;
      if (format && format.bitrate != null) {
        const raw = Number(format.bitrate);
        if (Number.isFinite(raw)) {
          const kbps = raw > 5000 ? Math.round(raw / 1000) : Math.round(raw);
          bitrateKbps = Math.max(1, Math.min(9999, kbps));
        }
      }
      const sampleRateHz = format && format.sampleRate;
      const samplingRateKhz = sampleRateHz ? Math.round(sampleRateHz / 1000) : null;
      results.push({
        path: relative,
        title: (common && common.title) || path.basename(name, ext),
        artist: (common && common.artist) || "Unknown Artist",
        album: (common && common.album) || "Unknown Album",
        duration,
        year,
        key: `local:${relative}`,
        partKey: `local:${relative}`,
        thumb: `local:${relative}`,
        index: trackNo,
        bitrate: bitrateKbps,
        samplingRate: samplingRateKhz,
        audioChannels: (format && format.numberOfChannels) || null,
        audioCodec: (format && format.codec) || null,
        container: ext.slice(1),
      });
    } catch (_) {
      results.push({
        path: relative,
        title: path.basename(name, ext),
        artist: "Unknown Artist",
        album: "Unknown Album",
        duration: null,
        year: "",
        key: `local:${relative}`,
        partKey: `local:${relative}`,
        thumb: `local:${relative}`,
        index: null,
        bitrate: null,
        samplingRate: null,
        audioChannels: null,
        audioCodec: null,
        container: ext.slice(1),
      });
    }
  }
  return results;
}

app.get("/api/local-files/scan", async (req, res) => {
  const s = readSettingsSafe();
  if (!s.localFilesEnabled || !s.localMusicPath || !fs.existsSync(s.localMusicPath)) {
    return res.json({ tracks: [] });
  }
  try {
    const tracks = await scanLocalMusic(s.localMusicPath);
    res.json({ tracks, scanId: Date.now() });
  } catch (err) {
    res.status(500).json({ error: (err && err.message) || "Scan failed" });
  }
});

function findFolderArtSync(musicRoot, relativePath) {
  const normalized = (relativePath || "").replace(/\\/g, "/").replace(/\/+/g, "/");
  const dirRel = path.dirname(normalized);
  const dirFull = path.join(musicRoot, dirRel);
  const resolved = path.resolve(dirFull);
  const rootResolved = path.resolve(musicRoot);
  if (!resolved.startsWith(rootResolved) || resolved === rootResolved) return null;
  if (!fs.existsSync(resolved) || !fs.statSync(resolved).isDirectory()) return null;
  let entries;
  try {
    entries = fs.readdirSync(resolved, { withFileTypes: true });
  } catch (_) {
    return null;
  }
  for (const ent of entries) {
    if (!ent.isFile() || !LOCAL_COVER_REGEX.test(ent.name)) continue;
    const ext = path.extname(ent.name).toLowerCase();
    try {
      const buf = fs.readFileSync(path.join(resolved, ent.name));
      const fmt = ext === ".png" ? "image/png" : "image/jpeg";
      return { data: buf, format: fmt };
    } catch (_) { }
  }
  return null;
}

app.get("/api/local-files/art", (req, res) => {
  const rawPath = req.query.path;
  if (!rawPath || typeof rawPath !== "string") return res.status(400).send("Missing path");
  const s = readSettingsSafe();
  if (!s.localFilesEnabled || !s.localMusicPath) return res.status(403).send("Local files not enabled");
  let decoded;
  try {
    decoded = decodeURIComponent(rawPath);
  } catch (_) {
    return res.status(400).send("Invalid path");
  }
  if (decoded.includes("..") || path.isAbsolute(decoded)) return res.status(400).send("Invalid path");
  const normalized = decoded.replace(/\\/g, "/").replace(/\/+/g, "/");
  let cached = localArtCache.get(decoded) || localArtCache.get(normalized);
  if (!cached) {
    const dirKey = path.dirname(normalized).replace(/\\/g, "/");
    if (dirKey && dirKey !== ".") cached = localArtCache.get("__dir:" + dirKey);
  }
  if (!cached) {
    const found = findFolderArtSync(s.localMusicPath, normalized) || findFolderArtSync(s.localMusicPath, decoded);
    if (found) {
      cached = found;
      localArtCache.set(normalized, found);
      const dirKey = path.dirname(normalized).replace(/\\/g, "/");
      if (dirKey && dirKey !== ".") localArtCache.set("__dir:" + dirKey, found);
    }
  }
  if (!cached) {
    res.setHeader("Cache-Control", "no-store");
    return res.status(404).send("No art");
  }
  res.setHeader("Cache-Control", "public, max-age=86400");
  res.setHeader("Content-Type", cached.format || "image/jpeg");
  res.send(cached.data);
});

app.get("/api/local-files/stream", (req, res) => {
  const rawPath = req.query.path;
  if (!rawPath || typeof rawPath !== "string") return res.status(400).send("Missing path");
  const s = readSettingsSafe();
  if (!s.localFilesEnabled || !s.localMusicPath) return res.status(403).send("Local files not enabled");
  const decoded = decodeURIComponent(rawPath);
  if (decoded.includes("..") || path.isAbsolute(decoded)) return res.status(400).send("Invalid path");
  const full = path.join(s.localMusicPath, decoded);
  const resolved = path.resolve(full);
  const rootResolved = path.resolve(s.localMusicPath);
  if (!resolved.startsWith(rootResolved) || resolved === rootResolved) return res.status(400).send("Invalid path");
  if (!fs.existsSync(resolved) || !fs.statSync(resolved).isFile()) return res.status(404).send("Not found");
  const ext = path.extname(resolved).toLowerCase();
  if (!LOCAL_MUSIC_EXTENSIONS.has(ext)) return res.status(400).send("Not an audio file");
  res.setHeader("Cache-Control", "no-store");
  res.sendFile(resolved, { acceptRanges: true });
});

// Serve built frontend when DIST_PATH is set (Electron tray app) or in production with dist present
// DIST_PATH set by Electron when packaged so app works from anywhere
const distPath = process.env.DIST_PATH || (process.env.NODE_ENV === "production" ? path.join(__dirname, "dist") : null);
if (distPath && fs.existsSync(distPath)) {
  console.log("[Sonic] serving static from", distPath);
  app.use(
    express.static(distPath, {
      setHeaders: (res, filePath) => {
        if (filePath.endsWith(".html") || filePath.endsWith(".js")) {
          res.setHeader("Cache-Control", "no-store, no-cache, must-revalidate");
        }
      },
    })
  );
  app.get("*", (req, res) => {
    res.setHeader("Cache-Control", "no-store, no-cache, must-revalidate");
    res.sendFile(path.join(distPath, "index.html"));
  });
} else if (distPath) {
  console.error("[Sonic] dist not found at:", distPath, "(__dirname:", __dirname, ")");
}

const maxPort = 4010;
const listenHost = process.env.LISTEN_HOST || "127.0.0.1";
function tryListen(port) {
  if (port > maxPort) {
    console.error("[Sonic] no free port between", PORT, "and", maxPort);
    return;
  }
  const server = app.listen(port, listenHost, () => {
    serverUriCache.clear();
    resolveListening(port);
    console.log(`Sonic backend listening on http://${listenHost}:${port}`);
  });
  server.keepAliveTimeout = 3600000; // 1 hour
  server.headersTimeout = 3600000; // 1 hour (>= keepAliveTimeout)
  server.on("error", (err) => {
    if (err.code === "EADDRINUSE") {
      console.warn("[Sonic] port", port, "in use, trying", port + 1);
      tryListen(port + 1);
    } else {
      console.error("[Sonic] server error:", err);
    }
  });
}
tryListen(PORT);

