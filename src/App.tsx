import { useCallback, useEffect, useMemo, useRef, useState } from "react";

declare global {
  interface Window {
    SonicMedia?: {
      setNowPlaying(meta: {
        title: string;
        artist?: string;
        album?: string;
        albumArt?: string;
        state: "playing" | "paused" | "stopped";
        currentTime?: number;
        duration?: number;
      }): void;
      onCommand?(callback: (command: { type: string; position?: number }) => void): void;
    };
  }
}

type PlexPin = {
  id: number;
  code: string;
  authToken?: string | null;
};

type PlexStatus =
  | { state: "unknown"; token?: undefined }
  | { state: "signedOut"; token?: undefined }
  | { state: "signedIn"; token: string };

type MainView = "library" | "nowPlaying" | "queue" | "playlists" | "settings";

export type PlaylistItem = {
  trackKey: string;
  partKey: string;
  title: string;
  artist: string;
  album: string;
  thumb: string | null;
};

export type Playlist = {
  id: string;
  name: string;
  image: string | null;
  items: PlaylistItem[];
};

export type StreamingQuality = "128" | "256" | "320" | "lossless";

const STREAMING_QUALITY_OPTIONS: { value: StreamingQuality; label: string }[] = [
  { value: "128", label: "128 kbps" },
  { value: "256", label: "256 kbps" },
  { value: "320", label: "320 kbps" },
  { value: "lossless", label: "Lossless" },
];

const APP_FONT_OPTIONS: { value: string; label: string }[] = [
  { value: "", label: "System default" },
  { value: "Georgia", label: "Georgia" },
  { value: "Inter", label: "Inter" },
  { value: "Courier New", label: "Courier New" },
  { value: "Trebuchet MS", label: "Trebuchet MS" },
  { value: "Palatino Linotype", label: "Palatino" },
];

function hexToHSL(hex: string): { h: number; s: number; l: number } {
  const n = parseInt(hex.slice(1), 16);
  const r = (n >> 16) / 255;
  const g = ((n >> 8) & 0xff) / 255;
  const b = (n & 0xff) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  let h = 0;
  let s = 0;
  const l = (max + min) / 2;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    switch (max) {
      case r: h = ((g - b) / d + (g < b ? 6 : 0)) / 6; break;
      case g: h = ((b - r) / d + 2) / 6; break;
      default: h = ((r - g) / d + 4) / 6; break;
    }
  }
  return { h: h * 360, s: s * 100, l: l * 100 };
}

function hslToHex(h: number, s: number, l: number): string {
  s /= 100;
  l /= 100;
  const a = s * Math.min(l, 1 - l);
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    return l - a * Math.max(Math.min(k - 3, 9 - k, 1), -1);
  };
  const r = Math.round(f(0) * 255);
  const g = Math.round(f(8) * 255);
  const b = Math.round(f(4) * 255);
  return `#${(1 << 24 | r << 16 | g << 8 | b).toString(16).slice(1)}`;
}

/** Parse hex input to #RRGGBB or null if invalid. Accepts #RRGGBB, RRGGBB, #RGB, RGB. */
function parseHexInput(raw: string): string | null {
  const s = raw.trim().replace(/^#/, "");
  if (/^[0-9a-fA-F]{6}$/.test(s)) return "#" + s.toLowerCase();
  if (/^[0-9a-fA-F]{3}$/.test(s))
    return "#" + s.toLowerCase().replace(/(.)/g, "$1$1");
  return null;
}

function hexToRgba(hex: string, a: number): string {
  const n = parseInt(hex.slice(1), 16);
  const r = (n >> 16) & 0xff;
  const g = (n >> 8) & 0xff;
  const b = n & 0xff;
  return `rgba(${r},${g},${b},${a})`;
}

/** Relative luminance 0–1 (dark to light). Used for text contrast. */
function hexLuminance(hex: string): number {
  const n = parseInt(hex.slice(1), 16);
  const r = (n >> 16) & 0xff;
  const g = (n >> 8) & 0xff;
  const b = n & 0xff;
  const [rs, gs, bs] = [r, g, b].map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * rs + 0.7152 * gs + 0.0722 * bs;
}

export type ThemeTextMode = "auto" | "light" | "dark";

const LIGHT_TEXT = "#e8eef4";
const LIGHT_MUTED = "#9ca8b8";
const DARK_TEXT = "#1a2234";
const DARK_MUTED = "#4a5568";

const DEFAULT_THEME_SEED = "#0f172a";

function isValidHex(hex: string): boolean {
  if (!hex || typeof hex !== "string") return false;
  const s = hex.replace(/^#/, "");
  return /^[0-9a-fA-F]{6}$/.test(s) || /^[0-9a-fA-F]{3}$/.test(s);
}

/** Theme: the seed color is the main background (most prominent). Surface/border/accent derived from same hue. */
function applyThemeFromSeed(seedHex: string, textMode: ThemeTextMode) {
  const hex = isValidHex(seedHex) ? (seedHex.startsWith("#") ? seedHex : "#" + seedHex) : DEFAULT_THEME_SEED;
  const { h, s, l } = hexToHSL(hex);
  const root = document.documentElement;
  // Use the exact seed as main background so the dominant color is most prominent
  const bg = hex;
  const surface = hslToHex(h, s, Math.max(8, Math.min(92, l + 10)));
  const border = hslToHex(h, s, Math.max(12, Math.min(88, l + 16)));
  const accent = hslToHex(h, Math.min(80, s + 15), Math.max(25, Math.min(85, l + 28)));
  root.style.setProperty("--app-bg", bg);
  root.style.setProperty("--app-surface", surface);
  root.style.setProperty("--app-border", border);
  root.style.setProperty("--app-accent", accent);
  root.style.setProperty("--app-accent-dim", hexToRgba(accent, 0.28));
  const lum = hexLuminance(bg);
  if (textMode === "light") {
    root.style.setProperty("--app-text", LIGHT_TEXT);
    root.style.setProperty("--app-muted", LIGHT_MUTED);
  } else if (textMode === "dark") {
    root.style.setProperty("--app-text", DARK_TEXT);
    root.style.setProperty("--app-muted", DARK_MUTED);
  } else {
    const useLightText = lum < 0.45;
    root.style.setProperty("--app-text", useLightText ? LIGHT_TEXT : DARK_TEXT);
    root.style.setProperty("--app-muted", useLightText ? LIGHT_MUTED : DARK_MUTED);
  }
}

/** Get dominant color from an image URL (for theme-from-album). Returns hex e.g. #1a2b3c or null on failure. */
function getDominantColorFromImageUrl(url: string): Promise<string | null> {
  return new Promise((resolve) => {
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => {
      try {
        const size = 64;
        const canvas = document.createElement("canvas");
        canvas.width = size;
        canvas.height = size;
        const ctx = canvas.getContext("2d");
        if (!ctx) {
          resolve(null);
          return;
        }
        ctx.drawImage(img, 0, 0, size, size);
        const data = ctx.getImageData(0, 0, size, size).data;
        // Quantize to 4 bits per channel; track count and sum per bucket so we get exact average of most-used region
        const shift = 4;
        const buckets: Record<number, { count: number; sumR: number; sumG: number; sumB: number }> = {};
        for (let i = 0; i < data.length; i += 4) {
          const r = data[i];
          const g = data[i + 1];
          const b = data[i + 2];
          const a = data[i + 3];
          if (a < 128) continue;
          const key = (r >> shift << 8) | (g >> shift << 4) | (b >> shift);
          const cur = buckets[key];
          if (!cur) buckets[key] = { count: 1, sumR: r, sumG: g, sumB: b };
          else {
            cur.count++;
            cur.sumR += r;
            cur.sumG += g;
            cur.sumB += b;
          }
        }
        let maxCount = 0;
        let best: { sumR: number; sumG: number; sumB: number; count: number } | null = null;
        for (const cur of Object.values(buckets)) {
          if (cur.count > maxCount) {
            maxCount = cur.count;
            best = cur;
          }
        }
        if (!best) {
          resolve(null);
          return;
        }
        const r = Math.round(best.sumR / best.count);
        const g = Math.round(best.sumG / best.count);
        const b = Math.round(best.sumB / best.count);
        const hex = `#${(1 << 24 | r << 16 | g << 8 | b).toString(16).slice(1)}`;
        resolve(hex);
      } catch {
        resolve(null);
      }
    };
    img.onerror = () => resolve(null);
    img.src = url;
  });
}

const EQ_MIN_DB = -12;
const EQ_MAX_DB = 12;
const PREAMP_MIN_DB = -12;
const PREAMP_MAX_DB = 12;

/** 20 bands, log-spaced 25 Hz to 16 kHz */
const EQ_BANDS = (() => {
  const freqs: number[] = [];
  const minF = Math.log10(25);
  const maxF = Math.log10(16000);
  for (let i = 0; i < 20; i++) {
    const f = Math.pow(10, minF + (maxF - minF) * (i / 19));
    freqs.push(Math.round(f));
  }
  return freqs.map((freq) => ({
    freq,
    label: freq >= 1000 ? (freq / 1000).toFixed(1) + "k" : String(freq),
  }));
})();

/** Draw frequency (log) -> gain (dB) for parametric EQ curve. */
function EqCurve({ gains, width = 400, height = 88 }: { gains: number[]; width?: number; height?: number }) {
  const pad = { left: 32, right: 16, top: 8, bottom: 22 };
  const w = width - pad.left - pad.right;
  const h = height - pad.top - pad.bottom;
  const minF = Math.log10(25);
  const maxF = Math.log10(16000);
  const toX = (freq: number) => pad.left + (Math.log10(freq) - minF) / (maxF - minF) * w;
  const toY = (db: number) => pad.top + h - (db - EQ_MIN_DB) / (EQ_MAX_DB - EQ_MIN_DB) * h;
  const points = EQ_BANDS.map((band, i) => {
    const g = gains[i] ?? 0;
    return `${toX(band.freq)},${toY(g)}`;
  }).join(" ");
  const zeroY = toY(0);
  return (
    <div style={{ marginBottom: 16 }}>
      <svg width={width} height={height} style={{ display: "block", overflow: "visible" }} aria-hidden>
        <defs>
          <linearGradient id="eqFill" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="var(--app-accent)" stopOpacity="0.35" />
            <stop offset="100%" stopColor="var(--app-accent)" stopOpacity="0.05" />
          </linearGradient>
        </defs>
        <line x1={pad.left} y1={zeroY} x2={width - pad.right} y2={zeroY} stroke="var(--app-border)" strokeWidth="1" strokeDasharray="4 2" opacity={0.8} />
        <path
          fill="url(#eqFill)"
          d={`M ${toX(EQ_BANDS[0].freq)},${zeroY} L ${points.split(" ").join(" L ")} L ${toX(EQ_BANDS[EQ_BANDS.length - 1].freq)},${zeroY} Z`}
        />
        <polyline
          fill="none"
          stroke="var(--app-accent)"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          points={points}
        />
        {EQ_BANDS.map((b) => (
          <text key={b.freq} x={toX(b.freq)} y={height - 4} textAnchor="middle" fontSize="8" fill="var(--app-muted)">{b.label}</text>
        ))}
      </svg>
    </div>
  );
}

const FLAT_20 = Array(20).fill(0);
const EQ_PRESETS: { id: string; label: string; gains: number[] }[] = [
  { id: "flat", label: "Flat", gains: [...FLAT_20] },
  { id: "bass", label: "Bass boost", gains: [8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
  { id: "treble", label: "Treble boost", gains: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7] },
  { id: "rock", label: "Rock", gains: [5, 4, 3, 2, 1, 0, -1, -1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 2, 2] },
  { id: "jazz", label: "Jazz", gains: [-1, 0, 1, 2, 2, 2, 2, 2, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
  { id: "classical", label: "Classical", gains: [2, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 3, 3, 4, 4, 4, 4, 4] },
];

const PLEX_CLIENT_IDENTIFIER = "crystal-player-web";
const PLEX_PRODUCT = "Sonic";
const PLEX_DEVICE = "Browser";
const PLEX_PLATFORM = "Web";

// Base URL for the backend. In production (packaged app) use relative URLs so we work no matter which port the server bound to (4000–4010).
// In dev (Vite) use explicit localhost:4000 so the dev server and backend can differ.
const API_ORIGIN =
  import.meta.env.VITE_API_BASE
    ? import.meta.env.VITE_API_BASE.replace(/\/api\/plex\/?$/, "") || import.meta.env.VITE_API_BASE
    : import.meta.env.DEV
      ? "http://localhost:4000"
      : "";
const API_BASE = (API_ORIGIN ? API_ORIGIN + "/api/plex" : "/api/plex");
const API_SERVER = API_ORIGIN;

let localArtScanId = 0;
function getAlbumThumbUrl(thumb: string | undefined | null, token?: string): string | null {
  if (!thumb) return null;
  if (thumb.startsWith("local:")) {
    const url = `${API_SERVER}/api/local-files/art?path=${encodeURIComponent(thumb.slice(6))}`;
    return `${url}&v=${localArtScanId}`;
  }
  const path = thumb.startsWith("http") ? new URL(thumb).pathname : thumb;
  return token ? `${API_BASE}/thumb?thumb=${encodeURIComponent(path)}&token=${encodeURIComponent(token)}` : null;
}

/** Renders album art or a blank placeholder when URL is missing or image fails to load. */
function AlbumCover({ thumbUrl, style = {} }: { thumbUrl: string | null; style?: React.CSSProperties }) {
  const [errored, setErrored] = useState(false);
  const showPlaceholder = !thumbUrl || errored;
  const containerStyle: React.CSSProperties = { position: "relative", overflow: "hidden", ...style };
  return (
    <div style={containerStyle}>
      {showPlaceholder ? (
        <div style={{ position: "absolute", inset: 0, background: "var(--app-surface)" }} />
      ) : (
        <img src={thumbUrl} alt="" onError={() => setErrored(true)} style={{ width: "100%", height: "100%", objectFit: "cover", display: "block" }} />
      )}
    </div>
  );
}

const BACKEND_NOT_RUNNING_MSG =
  "The backend service isn't running. In the project folder run: npm run server";

function isNetworkOrBackendError(err: unknown): boolean {
  const msg = String((err as Error).message ?? err).toLowerCase();
  return (
    msg.includes("failed to fetch") ||
    msg.includes("network error") ||
    msg.includes("networkrequestfailed") ||
    msg.includes("connection refused") ||
    msg.includes("err_connection_refused") ||
    msg.includes("load failed") ||
    msg.includes("service") && (msg.includes("running") || msg.includes("unavailable"))
  );
}

/** Fetch from backend; on network/connection failure throws with BACKEND_NOT_RUNNING_MSG. */
async function backendFetch(url: string, options?: RequestInit): Promise<Response> {
  try {
    const res = await fetch(url, options);
    if (res.status === 404 || res.status >= 502) throw new Error(BACKEND_NOT_RUNNING_MSG);
    return res;
  } catch (err) {
    if (isNetworkOrBackendError(err)) throw new Error(BACKEND_NOT_RUNNING_MSG);
    throw err;
  }
}

// Helper to talk directly to Plex for the PIN flow only. Everything
// else (servers, libraries, playback) goes through the backend.
async function plexPinFetch<T>(
  path: string,
  options: RequestInit & { method: "GET" | "POST" }
): Promise<T> {
  const url = `https://plex.tv/api/v2${path}`;
  const res = await fetch(url, {
    ...options,
    headers: {
      "X-Plex-Client-Identifier": PLEX_CLIENT_IDENTIFIER,
      "X-Plex-Product": PLEX_PRODUCT,
      "X-Plex-Device": PLEX_DEVICE,
      "X-Plex-Platform": PLEX_PLATFORM,
      Accept: "application/json",
      ...(options.headers || {}),
    },
  });

  const text = await res.text();
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}: ${text.slice(0, 200)}`);
  }

    return JSON.parse(text) as T;
}

type PlexLibrary = {
  key: string;
  title: string;
  type: string;
};

type PlexAlbum = {
  key: string;
  title: string;
  artist: string;
  thumb: string | null;
  year: string;
};

type PlexTrack = {
  key: string;
  title: string;
  index: number | null;
  duration: number | null;
  bitrate: number | null;
  audioChannels: number | null;
  audioCodec: string | null;
  container: string | null;
  samplingRate?: number | null;
  partKey?: string | null;
};

function PlexAuthCard({
  status,
  setStatus,
}: {
  status: PlexStatus;
  setStatus: (next: PlexStatus) => void;
}) {
  const [isCreatingPin, setIsCreatingPin] = useState(false);
  const [hasOpenedPlex, setHasOpenedPlex] = useState(false);
  const [pin, setPin] = useState<PlexPin | null>(null);
  const [, setIsPolling] = useState(false);
  const [plexLinkCopied, setPlexLinkCopied] = useState(false);
  const [plexLinkError, setPlexLinkError] = useState<string | null>(null);

  useEffect(() => {
    if (!pin || status.state === "signedIn") return;

    let mounted = true;
    let attempts = 0;

    const poll = async () => {
      if (!mounted || !pin) return;
      attempts += 1;
      try {
        setIsPolling(true);
        const updated = await plexPinFetch<any>(`/pins/${pin.id}`, {
          method: "GET",
        });
        const token: string | null =
          updated.authToken ??
          updated.auth_token ??
          updated.auth_token ??
          null;
        if (token) {
          window.localStorage.setItem("plexAuthToken", token);
          if (mounted) {
            setStatus({ state: "signedIn", token });
          }
          return;
        }
      } catch (err) {
        console.error("Error polling Plex PIN", err);
      } finally {
        if (mounted) {
          setIsPolling(false);
        }
      }

      if (mounted && attempts < 60 && !document.hidden) {
        window.setTimeout(poll, 5000);
      }
    };

    poll();

    return () => {
      mounted = false;
    };
  }, [pin, status.state, setStatus]);

  async function handleCreatePin() {
    setPlexLinkError(null);
    setPlexLinkCopied(false);
    try {
      setIsCreatingPin(true);

      // Ask Plex directly to create a PIN. Using the same
      // client identifier here and in the auth URL keeps
      // Plex happy.
      const created = await plexPinFetch<PlexPin>("/pins?strong=true", {
        method: "POST",
      });
      setPin(created);

      const url = `https://app.plex.tv/auth/#!?clientID=${encodeURIComponent(
        PLEX_CLIENT_IDENTIFIER
      )}&code=${encodeURIComponent(created.code)}`;
      try {
        window.open(url, "_blank", "noopener,noreferrer");
    } catch (err) {
        console.error("Failed to open Plex auth URL", err);
        setPlexLinkError(url);
        try {
          await navigator.clipboard.writeText(url);
          setPlexLinkCopied(true);
          setTimeout(() => setPlexLinkCopied(false), 8000);
        } catch {
          // Clipboard may fail without user gesture; "Copy link" button will work
        }
      }
      setHasOpenedPlex(true);
    } catch (err) {
      console.error("Error starting Plex auth", err);
    } finally {
      setIsCreatingPin(false);
    }
  }

  function handleSignOut() {
    window.localStorage.removeItem("plexAuthToken");
    setStatus({ state: "signedOut" });
  }

  const statusLabel =
    status.state === "unknown"
      ? "Checking…"
      : status.state === "signedOut"
      ? "Not connected"
      : "Connected";

  const tokenSnippet =
    status.state === "signedIn"
      ? `${status.token.slice(0, 8)}…${status.token.slice(-4)}`
      : null;

  return (
    <div
      style={{
        padding: 16,
        borderRadius: 16,
        backgroundColor: "var(--app-surface)",
        border: "1px solid var(--app-border)",
        display: "flex",
        flexDirection: "column",
        gap: 12,
      }}
    >
      <div>
        <div
          style={{
            fontSize: 12,
            textTransform: "uppercase",
            letterSpacing: 0.08,
            opacity: 0.8,
          }}
        >
          Plex
        </div>
        <div style={{ fontSize: 14, marginTop: 2 }}>{statusLabel}</div>
        {tokenSnippet && (
          <div
            style={{
              fontSize: 11,
              opacity: 0.7,
              marginTop: 2,
            }}
          >
            Connected (token ending {tokenSnippet.slice(-4)})
          </div>
        )}
      </div>

      {status.state !== "signedIn" && (
        <>
          <button
            type="button"
            onClick={handleCreatePin}
            disabled={isCreatingPin}
            style={{
              padding: "10px 16px",
              borderRadius: 999,
              border: "none",
              backgroundColor: "#f97316",
              color: "#111827",
              fontSize: 14,
              fontWeight: 600,
              cursor: isCreatingPin ? "default" : "pointer",
            }}
          >
            {isCreatingPin
              ? "Contacting Plex…"
              : "Sign in with Plex"}
          </button>

          <p style={{ fontSize: 12, opacity: 0.8 }}>
            A browser window will open. Sign in to Plex there, then come back
            here; this preview will show you as connected so you can explore the
            player UI.
          </p>

          {plexLinkCopied && (
            <p style={{ fontSize: 13, color: "var(--app-accent)", marginTop: 8 }}>
              Link copied to clipboard. Paste it into your browser to sign in.
            </p>
          )}
          {plexLinkError && (
            <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 8 }}>
              <p style={{ fontSize: 12, opacity: 0.9 }}>
                {plexLinkCopied ? "Link copied to clipboard. Paste it into your browser." : "Could not open browser. Copy the link below:"}
              </p>
              <input
                type="text"
                readOnly
                value={plexLinkError}
                onClick={(e) => (e.target as HTMLInputElement).select()}
                style={{
                  width: "100%",
                  padding: "8px 10px",
                  fontSize: 11,
                  fontFamily: "monospace",
                  borderRadius: 8,
                  border: "1px solid var(--app-border)",
                  background: "var(--app-bg)",
                  color: "var(--app-text)",
                  boxSizing: "border-box",
                }}
                aria-label="Plex sign-in link"
              />
              <button
                type="button"
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(plexLinkError);
                    setPlexLinkCopied(true);
                    setTimeout(() => setPlexLinkCopied(false), 8000);
                  } catch {
                    try {
                      const ta = document.createElement("textarea");
                      ta.value = plexLinkError;
                      ta.setAttribute("readonly", "");
                      ta.style.position = "fixed";
                      ta.style.left = "-9999px";
                      document.body.appendChild(ta);
                      ta.select();
                      document.execCommand("copy");
                      document.body.removeChild(ta);
                      setPlexLinkCopied(true);
                      setTimeout(() => setPlexLinkCopied(false), 8000);
                    } catch {}
                  }
                }}
                style={{
                  alignSelf: "flex-start",
                  padding: "6px 12px",
                  fontSize: 12,
                  borderRadius: 8,
                  border: "1px solid var(--app-border)",
                  background: "var(--app-accent)",
                  color: "var(--app-text)",
                  cursor: "pointer",
                }}
              >
                Copy link
              </button>
            </div>
          )}

          {hasOpenedPlex && !plexLinkError && !plexLinkCopied && (
            <p style={{ fontSize: 11, opacity: 0.7 }}>
              In a full install, a background helper would turn your real Plex
              login into a saved token for this app.
            </p>
          )}
        </>
      )}

      {status.state === "signedIn" && (
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            gap: 8,
          }}
        >
          <p style={{ fontSize: 12, opacity: 0.8, margin: 0 }}>
            You’re connected to Plex. You can now browse your servers and
            libraries.
          </p>
          <button
            type="button"
            onClick={handleSignOut}
            style={{
              padding: "6px 12px",
              borderRadius: 999,
              border: "1px solid var(--app-border)",
              backgroundColor: "transparent",
              color: "var(--app-text)",
              fontSize: 12,
              cursor: "pointer",
              opacity: 0.9,
            }}
          >
            Disconnect
          </button>
        </div>
      )}
    </div>
  );
}

function normalizeForSearch(s: string): string {
  return s
    .toLowerCase()
    .replace(/[\s\-_.,'"]+/g, " ")
    .replace(/\s+/g, "")
    .trim();
}

/** Split query into words and normalize each; match if every word appears in the normalized text (e.g. "blink 182" matches "blink-182"). */
function searchQueryParts(query: string): string[] {
  const trimmed = query.trim().toLowerCase();
  if (!trimmed) return [];
  return trimmed
    .split(/[\s\-_.,'"]+/)
    .map((w) => w.replace(/\s+/g, ""))
    .filter(Boolean);
}

function textMatchesSearchParts(normalizedText: string, parts: string[]): boolean {
  if (parts.length === 0) return true;
  return parts.every((part) => normalizedText.includes(part));
}

type SortBy = "title" | "artist" | "year";
type ViewMode = "grid" | "list";
type GroupBy = "none" | "artist" | "year";

const ALBUM_HOVER_PREFETCH_DELAY_MS = 80;
const TRACK_HOVER_PRELOAD_MS = 100; // Start loading track on hover so click plays in ~200ms (Plexamp-style)

function LibraryView({
  status,
  selectedAlbum,
  onSelectAlbum,
  onPlayAlbum,
  onAlbumsLoaded,
  onServerUri,
  onAlbumContextMenu,
  preloadedTracks,
  onPlayTrack,
  onAlbumHover,
  onTrackHover,
  onTrackHoverEnd,
  localTracks = [],
  libraryFilter = "all",
  onLibraryFilterChange,
  onRefreshLocalFiles,
  onGoToSettings,
}: {
  status: PlexStatus;
  selectedAlbum?: PlexAlbum | null;
  onSelectAlbum?: (album: PlexAlbum) => void;
  onPlayAlbum?: (album: PlexAlbum) => void;
  onAlbumsLoaded?: (albums: PlexAlbum[]) => void;
  onServerUri?: (uri: string | null) => void;
  onAlbumContextMenu?: (album: PlexAlbum, e: React.MouseEvent) => void;
  preloadedTracks?: Record<string, PlexTrack[]>;
  onPlayTrack?: (trackList: PlexTrack[], index: number, album: PlexAlbum | null) => void;
  onAlbumHover?: (album: PlexAlbum) => void;
  onTrackHover?: (track: PlexTrack, immediate?: boolean) => void;
  onTrackHoverEnd?: () => void;
  localTracks?: PlexTrack[];
  libraryFilter?: "all" | "local";
  onLibraryFilterChange?: (v: "all" | "local") => void;
  onRefreshLocalFiles?: () => void;
  onGoToSettings?: () => void;
}) {
  const hoverPrefetchTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hoveredAlbumKeyRef = useRef<string | null>(null);
  const handleAlbumMouseEnter = useCallback((album: PlexAlbum) => {
    if (!onAlbumHover) return;
    if (hoverPrefetchTimeoutRef.current) clearTimeout(hoverPrefetchTimeoutRef.current);
    hoveredAlbumKeyRef.current = album.key;
    hoverPrefetchTimeoutRef.current = setTimeout(() => {
      hoverPrefetchTimeoutRef.current = null;
      onAlbumHover(album);
    }, ALBUM_HOVER_PREFETCH_DELAY_MS);
  }, [onAlbumHover]);
  const handleAlbumMouseLeave = useCallback(() => {
    if (hoverPrefetchTimeoutRef.current) {
      clearTimeout(hoverPrefetchTimeoutRef.current);
      hoverPrefetchTimeoutRef.current = null;
    }
    hoveredAlbumKeyRef.current = null;
  }, []);
  const [musicLibrary, setMusicLibrary] = useState<PlexLibrary | null>(null);
  const [albums, setAlbums] = useState<PlexAlbum[]>([]);
  const [, setServerUri] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [libraryRefreshTrigger, setLibraryRefreshTrigger] = useState(0);
  const wantRefreshRef = useRef(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [sortBy, setSortBy] = useState<SortBy>("artist");
  const [sortAsc, setSortAsc] = useState(true);
  const [groupBy, setGroupBy] = useState<GroupBy>("none");
  const [viewMode, setViewMode] = useState<ViewMode>("grid");

  const localAlbums = useMemo((): PlexAlbum[] => {
    const byKey = new Map<string, { artist: string; album: string; year: string; tracks: PlexTrack[] }>();
    for (const t of localTracks) {
      const artist = (t as PlexTrack & { artist?: string }).artist ?? "Unknown Artist";
      const album = (t as PlexTrack & { album?: string }).album ?? "Unknown Album";
      const year = (t as PlexTrack & { year?: string }).year ?? "";
      const key = `local:album:${artist}\0${album}`;
      if (!byKey.has(key)) byKey.set(key, { artist, album, year, tracks: [] });
      byKey.get(key)!.tracks.push(t);
    }
    return [...byKey.entries()].map(([, { artist, album, year, tracks }]) => {
      const key = `local:album:${artist}\0${album}`;
      const sorted = [...tracks].sort((a, b) => (a.index ?? 9999) - (b.index ?? 9999));
      const firstPath = (sorted[0] as PlexTrack & { path?: string })?.path;
      return {
        key,
        title: album,
        artist,
        thumb: firstPath ? `local:${firstPath}` : null,
        year,
      };
    });
  }, [localTracks]);

  useEffect(() => {
    if (status.state !== "signedIn") return;

    let mounted = true;
    const refreshParam = wantRefreshRef.current ? "&refresh=1" : "";

    (async () => {
      try {
        setError(null);
        setIsLoading(true);
        if (wantRefreshRef.current) wantRefreshRef.current = false;

        const libsRes = await backendFetch(
          `${API_BASE}/libraries?token=${encodeURIComponent(status.token)}${refreshParam}`,
          { method: "GET", headers: { Accept: "application/json" } }
        );
        if (!libsRes.ok) {
          const body = await libsRes.json().catch(() => ({}));
          throw new Error((body as { error?: string }).error || `HTTP ${libsRes.status}`);
        }
        const libs = (await libsRes.json()) as PlexLibrary[];
        if (!mounted) return;
        const music =
          libs.find(
            (l) =>
              String(l.type).toLowerCase() === "artist" ||
              String(l.type).toLowerCase() === "music"
          ) ?? null;
        setMusicLibrary(music);

        if (!music) {
          setIsLoading(false);
          return;
        }

        const albumsRes = await backendFetch(
          `${API_BASE}/albums?token=${encodeURIComponent(status.token)}&sectionKey=${encodeURIComponent(music.key)}${refreshParam}`,
          { method: "GET", headers: { Accept: "application/json" } }
        );
        if (!albumsRes.ok) {
          const body = (await albumsRes.json().catch(() => ({}))) as { error?: string; detail?: string };
          const msg = body?.error || `Albums: ${albumsRes.status}`;
          const detail = body?.detail;
          throw new Error(detail ? `${msg} — ${detail}` : msg);
        }
        const data = (await albumsRes.json()) as { serverUri: string; albums: PlexAlbum[] };
        if (!mounted) return;
        setServerUri(data.serverUri ?? null);
        onServerUri?.(data.serverUri ?? null);
        const list = data.albums ?? [];
        setAlbums(list);
        onAlbumsLoaded?.(list);
    } catch (err) {
        if (mounted)
          setError(
            isNetworkOrBackendError(err) ? BACKEND_NOT_RUNNING_MSG : (err as Error).message ?? String(err)
          );
    } finally {
        if (mounted) setIsLoading(false);
      }
    })();

    return () => {
      mounted = false;
    };
  }, [status.state, status.token, libraryRefreshTrigger]);

  const searchParts = useMemo(() => searchQueryParts(searchQuery), [searchQuery]);
  const filteredAlbums = useMemo(() => {
    if (!searchParts.length) return albums;
    return albums.filter((a) => {
      const combined =
        normalizeForSearch(a.title) +
        (a.artist ? normalizeForSearch(a.artist) : "") +
        (a.year ? normalizeForSearch(a.year) : "");
      return textMatchesSearchParts(combined, searchParts);
    });
  }, [albums, searchParts]);
  type SearchTrackHit = { album: PlexAlbum; track: PlexTrack; trackIndex: number };
  const filteredTracks = useMemo((): SearchTrackHit[] => {
    if (!searchParts.length || !preloadedTracks) return [];
    const out: SearchTrackHit[] = [];
    for (const album of albums) {
      const tracks = preloadedTracks[album.key];
      if (!tracks?.length) continue;
      const albumTitleNorm = normalizeForSearch(album.title);
      const artistNorm = album.artist ? normalizeForSearch(album.artist) : "";
      for (let i = 0; i < tracks.length; i++) {
        const t = tracks[i];
        const titleNorm = normalizeForSearch(t.title);
        const combined = titleNorm + artistNorm + albumTitleNorm;
        if (textMatchesSearchParts(combined, searchParts)) {
          out.push({ album, track: t, trackIndex: i });
        }
      }
    }
    return out;
  }, [albums, searchParts, preloadedTracks]);
  const filteredLocalTracks = useMemo((): SearchTrackHit[] => {
    if (!searchParts.length || !preloadedTracks) return [];
    const out: SearchTrackHit[] = [];
    for (const album of localAlbums) {
      const tracks = preloadedTracks[album.key];
      if (!tracks?.length) continue;
      const albumTitleNorm = normalizeForSearch(album.title);
      const artistNorm = normalizeForSearch(album.artist || "");
      for (let i = 0; i < tracks.length; i++) {
        const t = tracks[i];
        const titleNorm = normalizeForSearch(t.title);
        const combined = titleNorm + artistNorm + albumTitleNorm;
        if (textMatchesSearchParts(combined, searchParts)) {
          out.push({ album, track: t, trackIndex: i });
        }
      }
    }
    return out;
  }, [localAlbums, searchParts, preloadedTracks]);

  const sortedAlbums = useMemo(() => {
    const list = [...filteredAlbums];
    const mult = sortAsc ? 1 : -1;
    list.sort((a, b) => {
      let va: string | number = a.title;
      let vb: string | number = b.title;
      if (sortBy === "artist") {
        va = a.artist || "";
        vb = b.artist || "";
      } else if (sortBy === "year") {
        va = a.year ? parseInt(a.year, 10) || 0 : 0;
        vb = b.year ? parseInt(b.year, 10) || 0 : 0;
      } else {
        va = (va as string).toLowerCase();
        vb = (vb as string).toLowerCase();
      }
      if (typeof va === "number" && typeof vb === "number") return mult * (va - vb);
      return mult * String(va).localeCompare(String(vb), undefined, { sensitivity: "base" });
    });
    return list;
  }, [filteredAlbums, sortBy, sortAsc]);

  const filteredLocalAlbums = useMemo(() => {
    if (!searchParts.length) return localAlbums;
    return localAlbums.filter((a) => {
      const combined = normalizeForSearch(a.title) + normalizeForSearch(a.artist) + normalizeForSearch(a.year);
      return textMatchesSearchParts(combined, searchParts);
    });
  }, [localAlbums, searchParts]);
  const sortedLocalAlbums = useMemo(() => {
    const list = [...filteredLocalAlbums];
    const mult = sortAsc ? 1 : -1;
    list.sort((a, b) => {
      const va = sortBy === "artist" ? (a.artist || "") : sortBy === "year" ? (a.year ? parseInt(a.year, 10) || 0 : 0) : (a.title || "").toLowerCase();
      const vb = sortBy === "artist" ? (b.artist || "") : sortBy === "year" ? (b.year ? parseInt(b.year, 10) || 0 : 0) : (b.title || "").toLowerCase();
      if (typeof va === "number" && typeof vb === "number") return mult * (va - vb);
      return mult * String(va).localeCompare(String(vb), undefined, { sensitivity: "base" });
    });
    return list;
  }, [filteredLocalAlbums, sortBy, sortAsc]);

  const mergedSortedAlbums = useMemo(() => {
    const combined = [...sortedAlbums, ...sortedLocalAlbums];
    const mult = sortAsc ? 1 : -1;
    combined.sort((a, b) => {
      let va: string | number = sortBy === "artist" ? (a.artist || "") : sortBy === "year" ? (a.year ? parseInt(a.year, 10) || 0 : 0) : (a.title || "").toLowerCase();
      let vb: string | number = sortBy === "artist" ? (b.artist || "") : sortBy === "year" ? (b.year ? parseInt(b.year, 10) || 0 : 0) : (b.title || "").toLowerCase();
      if (typeof va === "number" && typeof vb === "number") return mult * (va - vb);
      return mult * String(va).localeCompare(String(vb), undefined, { sensitivity: "base" });
    });
    return combined;
  }, [sortedAlbums, sortedLocalAlbums, sortBy, sortAsc]);

  type GroupSection = { label: string; key: string; albums: PlexAlbum[] };
  const groupedSections = useMemo((): GroupSection[] => {
    const source = libraryFilter === "local" ? sortedLocalAlbums : mergedSortedAlbums;
    if (groupBy === "none") return [{ label: "", key: "", albums: source }];
    const map = new Map<string, PlexAlbum[]>();
    for (const a of source) {
      const key = groupBy === "artist" ? (a.artist || "—") : groupBy === "year" ? (a.year || "Unknown") : "";
      if (!map.has(key)) map.set(key, []);
      map.get(key)!.push(a);
    }
    const keys = [...map.keys()].sort((x, y) => String(x).localeCompare(String(y), undefined, { sensitivity: "base" }));
    if (groupBy === "year") keys.sort((x, y) => (Number(y) || 0) - (Number(x) || 0));
    return keys.map((k) => ({ label: k, key: k, albums: map.get(k)! }));
  }, [libraryFilter, sortedLocalAlbums, mergedSortedAlbums, groupBy]);

  if (status.state !== "signedIn" && localTracks.length === 0) {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: 12, maxWidth: 420 }}>
        <div style={{ padding: 20, borderRadius: 18, background: "var(--app-bg)", border: "1px solid var(--app-border)" }}>
          <div style={{ fontSize: 18, fontWeight: 600, marginBottom: 8 }}>No music source connected</div>
          <p style={{ fontSize: 13, opacity: 0.85, margin: 0 }}>Go to <strong>Settings</strong> to connect Plex or turn on local files to browse and play music here.</p>
        </div>
        {typeof onGoToSettings === "function" && (
          <button
            type="button"
            onClick={onGoToSettings}
            style={{
              padding: "10px 16px",
              borderRadius: 10,
              border: "1px solid var(--app-border)",
              background: "var(--app-accent)",
              color: "var(--app-text)",
              fontSize: 14,
              fontWeight: 500,
              cursor: "pointer",
              alignSelf: "flex-start",
            }}
          >
            Open Settings
          </button>
        )}
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      {error && (
    <div
      style={{
            fontSize: 13,
            color: "#fecaca",
            padding: 12,
            borderRadius: 8,
            background: "rgba(0,0,0,0.2)",
            border: "1px solid rgba(248,113,113,0.4)",
          }}
        >
          {error}
          {error === BACKEND_NOT_RUNNING_MSG && (
            <div style={{ marginTop: 8, fontFamily: "monospace", fontSize: 12 }}>
              In a terminal: <strong>npm run server</strong>
            </div>
          )}
        </div>
      )}
      {isLoading && !musicLibrary && (
        <div style={{ fontSize: 13, opacity: 0.8 }}>Loading music library and albums…</div>
      )}
      {!isLoading && !musicLibrary && !error && localAlbums.length === 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          <p style={{ fontSize: 13, opacity: 0.85, margin: 0 }}>No music library on this server. Add a Music library in Plex, or enable Local files in Settings.</p>
          {typeof onGoToSettings === "function" && (
            <button type="button" onClick={onGoToSettings} style={{ padding: "8px 14px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-surface)", color: "var(--app-text)", fontSize: 13, cursor: "pointer", alignSelf: "flex-start" }}>
              Open Settings
            </button>
          )}
        </div>
      )}
      {(musicLibrary || localAlbums.length > 0) && (
        <>
          <div style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
            <input
              type="search"
              placeholder="Search songs and albums…"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              style={{
                flex: "1 1 200px",
                minWidth: 0,
                maxWidth: 400,
                padding: "8px 12px",
                borderRadius: 10,
                border: "1px solid var(--app-border)",
                background: "var(--app-surface)",
                color: "var(--app-text)",
                fontSize: 14,
                outline: "none",
              }}
            />
            {localTracks.length > 0 && onLibraryFilterChange && (
              <select
                value={libraryFilter}
                onChange={(e) => onLibraryFilterChange(e.target.value as "all" | "local")}
                style={{
                  padding: "8px 12px",
                  borderRadius: 10,
                  border: "1px solid var(--app-border)",
                  background: "var(--app-surface)",
                  color: "var(--app-text)",
                  fontSize: 13,
                  cursor: "pointer",
                }}
              >
                <option value="all">All music</option>
                <option value="local">Local files</option>
              </select>
            )}
            <div style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap", marginLeft: "auto" }}>
              <button
                type="button"
                onClick={() => {
                  if (status.state === "signedIn" && status.token && musicLibrary) {
                    wantRefreshRef.current = true;
                    setLibraryRefreshTrigger((n) => n + 1);
                  }
                  onRefreshLocalFiles?.();
                }}
                style={{
                  padding: "6px 10px",
                  borderRadius: 8,
                  border: "1px solid var(--app-border)",
                  background: "var(--app-surface)",
                  color: "var(--app-text)",
                  fontSize: 12,
                  cursor: "pointer",
                }}
                title="Re-fetch library from Plex (if connected) and scan local files"
              >
                Refresh library
              </button>
              <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, opacity: 0.9 }}>
                Sort by
                <select
                  value={`${sortBy}-${sortAsc ? "asc" : "desc"}`}
                  onChange={(e) => {
                    const v = e.target.value;
                    const [s, o] = v.split("-") as [SortBy, string];
                    setSortBy(s);
                    setSortAsc(o === "asc");
                  }}
                  style={{
                    padding: "6px 10px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: "var(--app-surface)",
                    color: "var(--app-text)",
                    fontSize: 12,
                    cursor: "pointer",
                  }}
                >
                  <option value="title-asc">Title (A–Z)</option>
                  <option value="title-desc">Title (Z–A)</option>
                  <option value="artist-asc">Artist (A–Z)</option>
                  <option value="artist-desc">Artist (Z–A)</option>
                  <option value="year-desc">Year (newest)</option>
                  <option value="year-asc">Year (oldest)</option>
                </select>
              </label>
              <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, opacity: 0.9 }}>
                Group by
                <select
                  value={groupBy}
                  onChange={(e) => setGroupBy(e.target.value as GroupBy)}
                  style={{
                    padding: "6px 10px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: "var(--app-surface)",
                    color: "var(--app-text)",
                    fontSize: 12,
                    cursor: "pointer",
                  }}
                >
                  <option value="none">None</option>
                  <option value="artist">Artist</option>
                  <option value="year">Year</option>
                </select>
              </label>
              <div style={{ display: "flex", gap: 4 }}>
                <button
                  type="button"
                  onClick={() => setViewMode("grid")}
                  title="Grid view"
                  style={{
                    padding: "6px 10px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: viewMode === "grid" ? "var(--app-border)" : "var(--app-surface)",
                    color: "var(--app-text)",
                    cursor: "pointer",
                    fontSize: 14,
                  }}
                >
                  ⊞
                </button>
                <button
                  type="button"
                  onClick={() => setViewMode("list")}
                  title="List view"
                  style={{
                    padding: "6px 10px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: viewMode === "list" ? "var(--app-border)" : "var(--app-surface)",
                    color: "var(--app-text)",
                    cursor: "pointer",
                    fontSize: 14,
                  }}
                >
                  ≡
                </button>
              </div>
            </div>
          </div>
          {searchParts.length > 0 ? (
            <>
            <div style={{ display: "flex", flex: 1, minHeight: 0, gap: 24 }}>
              <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 12 }}>
                <div style={{ fontSize: 11, textTransform: "uppercase", letterSpacing: 0.1, opacity: 0.85, fontWeight: 600 }}>Songs</div>
                <div style={{ display: "flex", flexDirection: "column", gap: 4, overflowY: "auto", minHeight: 0 }}>
                    {[...filteredTracks, ...filteredLocalTracks].map(({ album, track, trackIndex }) => {
                      const thumbUrl = getAlbumThumbUrl(album.thumb, status.state === "signedIn" ? status.token : undefined);
                      const trackList = preloadedTracks?.[album.key] ?? [];
                      return (
                        <div
                          key={`${album.key}-${track.key}`}
                          role="button"
                          tabIndex={0}
                          onClick={() => onPlayTrack?.(trackList, trackIndex, album)}
                          onMouseDown={() => onTrackHover?.(track, true)}
                          onMouseEnter={() => onTrackHover?.(track)}
                          onMouseLeave={() => onTrackHoverEnd?.()}
                          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPlayTrack?.(trackList, trackIndex, album); } }}
                          style={{
                            display: "flex",
                            alignItems: "center",
                            gap: 12,
                            padding: "10px 12px",
                            borderRadius: 10,
                            background: "var(--app-surface)",
                            cursor: onPlayTrack ? "pointer" : "default",
                            borderLeft: "3px solid transparent",
                          }}
                        >
                          <AlbumCover thumbUrl={thumbUrl} style={{ width: 40, height: 40, borderRadius: 8, flexShrink: 0 }} />
                          <span style={{ width: 20, flexShrink: 0, fontSize: 12, opacity: 0.8 }}>{track.index ?? trackIndex + 1}</span>
                          <div style={{ minWidth: 0, flex: 1 }}>
                            <div style={{ fontWeight: 500, fontSize: 14, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{track.title}</div>
                            <div style={{ fontSize: 12, opacity: 0.8, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{album.artist ?? "—"} · {album.title}</div>
                          </div>
                          {track.duration != null && (
                            <span style={{ fontSize: 12, opacity: 0.8, flexShrink: 0 }}>
                              {Math.floor(track.duration / 60000)}:{String(Math.floor((track.duration % 60000) / 1000)).padStart(2, "0")}
                            </span>
                          )}
                        </div>
                      );
                    })}
                </div>
              </div>
              <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 12 }}>
                <div style={{ fontSize: 11, textTransform: "uppercase", letterSpacing: 0.1, opacity: 0.85, fontWeight: 600 }}>Albums</div>
                {([...filteredAlbums, ...filteredLocalAlbums].length > 0) ? viewMode === "grid" ? (
                    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))", gap: 16 }}>
                      {[...filteredAlbums, ...filteredLocalAlbums].map((album) => {
                        const thumbUrl = getAlbumThumbUrl(album.thumb, status.state === "signedIn" ? status.token : undefined);
                        const isSelected = selectedAlbum?.key === album.key;
                        return (
                          <div
                            key={album.key}
                            style={{ minWidth: 0, cursor: onSelectAlbum ? "pointer" : undefined, userSelect: "none", WebkitUserSelect: "none" }}
                            onClick={() => onSelectAlbum?.(album)}
                            onMouseEnter={() => handleAlbumMouseEnter(album)}
                            onMouseLeave={handleAlbumMouseLeave}
                            onContextMenu={(e) => { e.preventDefault(); onAlbumContextMenu?.(album, e); }}
                            onKeyDown={(e) => e.key === "Enter" && onSelectAlbum?.(album)}
                            role={onSelectAlbum ? "button" : undefined}
                            tabIndex={onSelectAlbum ? 0 : undefined}
                          >
                            <div
                              style={{
                                borderRadius: 12,
                                overflow: "hidden",
                                aspectRatio: "1",
                                backgroundColor: isSelected ? "var(--app-accent-dim)" : "var(--app-surface)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
                                boxShadow: isSelected ? "0 0 0 3px var(--app-accent)" : undefined,
                              }}
                              onDoubleClick={(e) => { e.stopPropagation(); onPlayAlbum?.(album); }}
                            >
                              <AlbumCover thumbUrl={thumbUrl} style={{ width: "100%", height: "100%" }} />
                            </div>
                            <div style={{ paddingTop: 8, paddingBottom: 0, paddingLeft: 2, paddingRight: 2, minWidth: 0 }}>
                              <div style={{ fontWeight: 600, fontSize: 13, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{album.title}</div>
                              <div style={{ fontSize: 12, opacity: 0.8, marginTop: 2, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{album.artist || "—"}</div>
                              {album.year && <div style={{ fontSize: 11, opacity: 0.6, marginTop: 2 }}>{album.year}</div>}
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  ) : (
                    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                      {[...filteredAlbums, ...filteredLocalAlbums].map((album) => {
                        const thumbUrl = getAlbumThumbUrl(album.thumb, status.state === "signedIn" ? status.token : undefined);
                        const isSelected = selectedAlbum?.key === album.key;
                        return (
                          <div
                            key={album.key}
                            role={onSelectAlbum ? "button" : undefined}
                            tabIndex={onSelectAlbum ? 0 : undefined}
                            onClick={() => onSelectAlbum?.(album)}
                            onMouseEnter={() => handleAlbumMouseEnter(album)}
                            onMouseLeave={handleAlbumMouseLeave}
                            onContextMenu={(e) => { e.preventDefault(); onAlbumContextMenu?.(album, e); }}
                            onKeyDown={(e) => e.key === "Enter" && onSelectAlbum?.(album)}
        style={{
                              display: "flex",
                              alignItems: "center",
                              gap: 12,
                              padding: "8px 12px",
                              borderRadius: 10,
                              background: isSelected ? "var(--app-accent-dim)" : "var(--app-surface)",
                              borderLeft: isSelected ? "3px solid var(--app-accent)" : "3px solid transparent",
                              cursor: onSelectAlbum ? "pointer" : undefined,
                              userSelect: "none",
                              WebkitUserSelect: "none",
                            }}
                          >
                            <div style={{ width: 48, height: 48, flexShrink: 0, borderRadius: 8, overflow: "hidden" }} onDoubleClick={(e) => { e.stopPropagation(); onPlayAlbum?.(album); }}>
                              <AlbumCover thumbUrl={thumbUrl} style={{ width: "100%", height: "100%" }} />
                            </div>
                            <div style={{ minWidth: 0, flex: 1 }}>
                              <div style={{ fontWeight: 600, fontSize: 14 }}>{album.title}</div>
                              <div style={{ fontSize: 12, opacity: 0.8 }}>{album.artist || "—"}</div>
                              {album.year && <div style={{ fontSize: 11, opacity: 0.6, marginTop: 2 }}>{album.year}</div>}
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  ) : null}
                </div>
              </div>
              {filteredTracks.length === 0 && filteredLocalTracks.length === 0 && filteredAlbums.length === 0 && filteredLocalAlbums.length === 0 && (
                <div style={{ fontSize: 13, opacity: 0.85 }}>No songs or albums match “{searchQuery.trim()}”.</div>
              )}
            </>
          ) : (
            <>
            {groupedSections.map((section) => (
            <div key={section.key || "all"} style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              {section.label ? (
                <div
                  style={{
                    fontSize: 11,
                    textTransform: "uppercase",
                    letterSpacing: 0.1,
                    opacity: 0.85,
                    fontWeight: 600,
                    marginTop: section.key ? 8 : 0,
                  }}
                >
                  {section.label}
                </div>
              ) : null}
              {viewMode === "grid" ? (
                <div
                  style={{
                    display: "grid",
                    gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
                    gap: 16,
                  }}
                >
                  {section.albums.map((album) => {
                    const thumbUrl = getAlbumThumbUrl(album.thumb, status.state === "signedIn" ? status.token : undefined);
                    const isSelected = selectedAlbum?.key === album.key;
                    return (
                      <div
                        key={album.key}
                        style={{
                          minWidth: 0,
                          cursor: onSelectAlbum ? "pointer" : undefined,
                          userSelect: "none",
                          WebkitUserSelect: "none",
                        }}
                        onClick={() => onSelectAlbum?.(album)}
                        onMouseEnter={() => handleAlbumMouseEnter(album)}
                        onMouseLeave={handleAlbumMouseLeave}
                        onContextMenu={(e) => { e.preventDefault(); onAlbumContextMenu?.(album, e); }}
                        onKeyDown={(e) => e.key === "Enter" && onSelectAlbum?.(album)}
                        role={onSelectAlbum ? "button" : undefined}
                        tabIndex={onSelectAlbum ? 0 : undefined}
                      >
                        <div
                          style={{
                            borderRadius: 12,
                            overflow: "hidden",
                            aspectRatio: "1",
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "center",
                            boxShadow: isSelected ? "0 0 0 3px var(--app-accent)" : undefined,
                            backgroundColor: isSelected ? "var(--app-accent-dim)" : "var(--app-surface)",
                          }}
                          onDoubleClick={(e) => {
                            e.stopPropagation();
                            onPlayAlbum?.(album);
                          }}
                        >
                          <AlbumCover thumbUrl={thumbUrl} style={{ width: "100%", height: "100%" }} />
                        </div>
                        <div
                          style={{
                            paddingTop: 8,
                            paddingBottom: 0,
                            paddingLeft: 2,
                            paddingRight: 2,
                            minWidth: 0,
                          }}
                        >
                          <div
                            style={{
                              fontWeight: 600,
                              fontSize: 13,
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                              whiteSpace: "nowrap",
                            }}
                            title={album.title}
                          >
                            {album.title}
                          </div>
                          <div
                            style={{
                              fontSize: 12,
                              opacity: 0.8,
                              marginTop: 2,
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                              whiteSpace: "nowrap",
                            }}
                            title={album.artist || undefined}
                          >
                            {album.artist || "—"}
                          </div>
                          {album.year && (
                            <div style={{ fontSize: 11, opacity: 0.6, marginTop: 2 }}>{album.year}</div>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>
              ) : (
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  {section.albums.map((album) => {
                    const thumbUrl = getAlbumThumbUrl(album.thumb, status.state === "signedIn" ? status.token : undefined);
                    const isSelected = selectedAlbum?.key === album.key;
                    return (
                      <div
                        key={album.key}
                        role={onSelectAlbum ? "button" : undefined}
                        tabIndex={onSelectAlbum ? 0 : undefined}
                        onClick={() => onSelectAlbum?.(album)}
                        onMouseEnter={() => handleAlbumMouseEnter(album)}
                        onMouseLeave={handleAlbumMouseLeave}
                        onContextMenu={(e) => { e.preventDefault(); onAlbumContextMenu?.(album, e); }}
                        onKeyDown={(e) => e.key === "Enter" && onSelectAlbum?.(album)}
                        style={{
                          display: "flex",
                          alignItems: "center",
                          gap: 12,
                          padding: "8px 12px",
                          borderRadius: 10,
                          background: isSelected ? "var(--app-accent-dim)" : "var(--app-surface)",
                          borderLeft: isSelected ? "3px solid var(--app-accent)" : "3px solid transparent",
                          cursor: onSelectAlbum ? "pointer" : undefined,
                          userSelect: "none",
                          WebkitUserSelect: "none",
                        }}
                      >
                        <div
                          style={{ width: 48, height: 48, flexShrink: 0, borderRadius: 8, overflow: "hidden" }}
                          onDoubleClick={(e) => {
                            e.stopPropagation();
                            onPlayAlbum?.(album);
                          }}
                        >
                          <AlbumCover thumbUrl={thumbUrl} style={{ width: "100%", height: "100%" }} />
                        </div>
                        <div style={{ minWidth: 0, flex: 1 }}>
                          <div
                            style={{
                              fontWeight: 600,
                              fontSize: 14,
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                              whiteSpace: "nowrap",
                            }}
                            title={album.title}
                          >
                            {album.title}
                          </div>
                          <div
                            style={{
                              fontSize: 12,
                              opacity: 0.8,
                              marginTop: 2,
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                              whiteSpace: "nowrap",
                            }}
                            title={album.artist || undefined}
                          >
                            {album.artist || "—"}
                          </div>
                        </div>
                        {album.year && (
                          <div style={{ fontSize: 12, opacity: 0.6, flexShrink: 0 }}>{album.year}</div>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
            ))}
            </>
          )}
        </>
      )}
    </div>
  );
}

function AlbumDetailSidebar({
  album,
  token,
  preloadedTracks,
  onTracksLoaded,
  onPlayTrack,
  onTrackContextMenu,
  onClose,
  playingTrackKey,
  embedInScroll,
  parentFetchesTracks,
  onTrackHover,
  onTrackHoverEnd,
}: {
  album: PlexAlbum;
  token: string;
  preloadedTracks: Record<string, PlexTrack[]>;
  onTracksLoaded: (key: string, tracks: PlexTrack[]) => void;
  onPlayTrack?: (trackList: PlexTrack[], index: number) => void;
  onTrackContextMenu?: (track: PlexTrack, album: PlexAlbum | null, trackList: PlexTrack[], index: number, e: React.MouseEvent) => void;
  onClose: () => void;
  playingTrackKey?: string | null;
  embedInScroll?: boolean;
  /** When true, parent (App) fetches tracks for this album; sidebar only shows loading until preloadedTracks updates. Avoids duplicate request. */
  parentFetchesTracks?: boolean;
  onTrackHover?: (track: PlexTrack, immediate?: boolean) => void;
  onTrackHoverEnd?: () => void;
}) {
  const cachedTracks = preloadedTracks[album.key];
  const [localTracks, setLocalTracks] = useState<PlexTrack[]>(cachedTracks ?? []);
  const [loading, setLoading] = useState(!cachedTracks && !parentFetchesTracks);
  const [error, setError] = useState<string | null>(null);
  const trackListScrollRef = useRef<HTMLDivElement>(null);
  const [coverHovered, setCoverHovered] = useState(false);

  const tracks = cachedTracks ?? localTracks;

  useEffect(() => {
    trackListScrollRef.current?.scrollTo({ top: 0 });
  }, [album.key]);
  const showLoading = !cachedTracks && (parentFetchesTracks ? true : loading);

  useEffect(() => {
    if (cachedTracks) {
      setLocalTracks(cachedTracks);
      setLoading(false);
      setError(null);
      return;
    }
    if (parentFetchesTracks) return;
    setLocalTracks([]);
    if (!album?.key || !token) {
      setLoading(false);
      return;
    }
    let mounted = true;
    setLoading(true);
    setError(null);
    backendFetch(
      `${API_BASE}/album/${encodeURIComponent(album.key)}/tracks?token=${encodeURIComponent(token)}`
    )
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then((data: { tracks: PlexTrack[] }) => {
        const list = data.tracks ?? [];
        if (mounted) {
          setLocalTracks(list);
          onTracksLoaded(album.key, list);
        }
      })
      .catch((err) => {
        if (mounted)
          setError(
            isNetworkOrBackendError(err) ? BACKEND_NOT_RUNNING_MSG : (err as Error).message ?? "Failed to load tracks"
          );
      })
      .finally(() => {
        if (mounted) setLoading(false);
      });
    return () => {
      mounted = false;
    };
  }, [album?.key, token, preloadedTracks, onTracksLoaded, parentFetchesTracks]);

  const thumbUrl = getAlbumThumbUrl(album.thumb, token);
  const firstTrack = tracks[0];
  const formatDuration = (ms: number | null) =>
    ms != null ? `${Math.floor(ms / 60000)}:${String(Math.floor((ms % 60000) / 1000)).padStart(2, "0")}` : "—";

  const fileType =
    firstTrack?.container || firstTrack?.audioCodec
      ? (firstTrack?.container || firstTrack?.audioCodec || "").toUpperCase()
      : null;
  const showDetails =
    firstTrack &&
    (firstTrack.bitrate != null || fileType);

  return (
        <div
          style={{
        paddingBottom: 12,
            display: "flex",
            flexDirection: "column",
        gap: 8,
        ...(embedInScroll ? {} : { flex: 1, minHeight: 0, overflow: "hidden" }),
        position: "relative",
      }}
    >
      <div
        style={{
          width: "100%",
          aspectRatio: "1",
          flexShrink: 0,
          borderRadius: 8,
          overflow: "hidden",
          position: "relative",
        }}
        onMouseEnter={() => setCoverHovered(true)}
        onMouseLeave={() => setCoverHovered(false)}
      >
        <button
          type="button"
          onClick={onClose}
          title="Close album"
          style={{
            position: "absolute",
            top: 6,
            right: 6,
            padding: "2px 6px",
            fontSize: 11,
            borderRadius: 999,
            border: "none",
            background: "rgba(15,23,42,0.8)",
            color: "var(--app-text)",
            cursor: "pointer",
            zIndex: 1,
            opacity: coverHovered ? 1 : 0,
            pointerEvents: coverHovered ? "auto" : "none",
            transition: "opacity 120ms ease-out",
          }}
        >
          ✕
        </button>
        <AlbumCover thumbUrl={thumbUrl} style={{ width: "100%", height: "100%" }} />
      </div>
      <div style={{ minWidth: 0, flexShrink: 0 }}>
        <div style={{ fontWeight: 600, fontSize: 13 }}>{album.title}</div>
        <div style={{ fontSize: 11, opacity: 0.85 }}>{album.artist || "—"}</div>
        {album.year && <div style={{ fontSize: 10, opacity: 0.7 }}>{album.year}</div>}
      </div>
      <div
        ref={trackListScrollRef}
        className="hide-scrollbar"
        style={{
          ...(embedInScroll ? {} : { flex: 1, minHeight: 0, overflowY: "auto" }),
              display: "flex",
              flexDirection: "column",
          gap: 2,
          paddingRight: 4,
        }}
      >
        {showLoading && <div style={{ fontSize: 12, opacity: 0.8 }}>Loading…</div>}
        {error && <div style={{ fontSize: 12, color: "#fecaca" }}>{error}</div>}
        {!showLoading && !error && tracks.length === 0 && (
          <div style={{ fontSize: 12, opacity: 0.8 }}>No tracks</div>
        )}
        {!showLoading &&
          !error &&
          tracks.map((t, idx) => {
            const isCurrentTrack = playingTrackKey != null && playingTrackKey === t.key;
            return (
              <div
                key={t.key}
                role="button"
                tabIndex={0}
                onClick={(e) => {
                  e.stopPropagation();
                  onPlayTrack?.(tracks, idx);
                }}
                onMouseDown={() => onTrackHover?.(t, true)}
                onMouseEnter={() => onTrackHover?.(t)}
                onMouseLeave={() => onTrackHoverEnd?.()}
                onContextMenu={(e) => {
                  e.stopPropagation();
                  e.preventDefault();
                  onTrackContextMenu?.(t, album, tracks, idx, e);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    onPlayTrack?.(tracks, idx);
                  }
                }}
              style={{
                fontSize: 12,
                  padding: "6px 8px",
                  borderRadius: 6,
                  background: isCurrentTrack ? "var(--app-accent-dim)" : "var(--app-surface)",
                  borderLeft: isCurrentTrack ? "3px solid var(--app-accent)" : "3px solid transparent",
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  cursor: onPlayTrack ? "pointer" : "default",
                  color: isCurrentTrack ? "var(--app-text)" : undefined,
                  fontWeight: isCurrentTrack ? 600 : undefined,
                }}
              >
                <span style={{ opacity: isCurrentTrack ? 1 : 0.7, flexShrink: 0, width: 20 }}>
                  {isCurrentTrack ? "▶" : (t.index != null ? t.index : idx + 1)}
                </span>
                <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {t.title}
                </span>
                <span style={{ opacity: isCurrentTrack ? 0.95 : 0.7, flexShrink: 0 }}>{formatDuration(t.duration)}</span>
              </div>
            );
          })}
      </div>
      {showDetails && (
        <div
          style={{
            marginTop: 6,
            paddingTop: 8,
            borderTop: "1px solid var(--app-border)",
            fontSize: 10,
                opacity: 0.8,
            flexShrink: 0,
              }}
            >
          <div style={{ textTransform: "uppercase", letterSpacing: 0.08, marginBottom: 4 }}>
            Details
            </div>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
            {firstTrack.bitrate != null && Number(firstTrack.bitrate) >= 1 && Number(firstTrack.bitrate) <= 9999 && <span>{Math.round(Number(firstTrack.bitrate))} kbps</span>}
            {fileType && <span>{fileType}</span>}
          </div>
        </div>
      )}
    </div>
  );
}

export function QueueUpNext({
  queue,
  currentIndex,
  token,
  playingTrackKey,
  onPlayQueueIndex,
  onTrackContextMenu,
}: {
  queue: QueueItem[];
  currentIndex: number;
  token: string;
  playingTrackKey: string | null;
  onPlayQueueIndex: (index: number) => void;
  onTrackContextMenu?: (track: PlexTrack, album: PlexAlbum | null, trackList: PlexTrack[], index: number, e: React.MouseEvent) => void;
}) {
  const upNext = queue.slice(currentIndex + 1);
  if (upNext.length === 0) return null;
  const groups: { album: PlexAlbum | null; items: QueueItem[]; startQueueIndex: number }[] = [];
  let i = currentIndex + 1;
  while (i < queue.length) {
    const album = queue[i].album;
    const albumKey = album?.key ?? null;
    const groupItems: QueueItem[] = [];
    const start = i;
    while (i < queue.length && (queue[i].album?.key ?? null) === albumKey) {
      groupItems.push(queue[i]);
      i++;
    }
    groups.push({ album, items: groupItems, startQueueIndex: start });
  }
  const formatDuration = (ms: number | null) =>
    ms != null ? `${Math.floor(ms / 60000)}:${String(Math.floor((ms % 60000) / 1000)).padStart(2, "0")}` : "—";

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, paddingTop: 16, borderTop: "1px solid var(--app-border)" }}>
      <div style={{ fontSize: 10, textTransform: "uppercase", letterSpacing: 0.08, opacity: 0.85 }}>Next in queue</div>
      {groups.map((group) => {
        const thumbUrl = getAlbumThumbUrl(group.album?.thumb, token ?? undefined);
        if (group.items.length === 1) {
          const item = group.items[0];
          const isCurrent = playingTrackKey === item.track.key;
          return (
            <div
              key={`${item.track.key}-${group.startQueueIndex}`}
              role="button"
              tabIndex={0}
              onClick={() => onPlayQueueIndex(group.startQueueIndex)}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onTrackContextMenu?.(item.track, item.album, [item.track], 0, e);
              }}
              onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPlayQueueIndex(group.startQueueIndex); } }}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 10,
                padding: "6px 8px",
                borderRadius: 8,
                background: isCurrent ? "var(--app-accent-dim)" : "var(--app-surface)",
                borderLeft: isCurrent ? "3px solid var(--app-accent)" : "3px solid transparent",
                cursor: "pointer",
                fontSize: 12,
              }}
            >
              <AlbumCover thumbUrl={thumbUrl} style={{ width: 36, height: 36, borderRadius: 6, flexShrink: 0 }} />
              <div style={{ minWidth: 0, flex: 1 }}>
                <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontWeight: 500 }}>{item.track.title}</div>
                {group.album && (
                  <div style={{ fontSize: 11, opacity: 0.75, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{group.album.title} · {group.album.artist || "—"}</div>
                )}
              </div>
              <span style={{ fontSize: 11, opacity: 0.8, flexShrink: 0 }}>{formatDuration(item.track.duration)}</span>
            </div>
          );
        }
        return (
          <div key={group.startQueueIndex} style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <AlbumCover thumbUrl={thumbUrl} style={{ width: 40, height: 40, borderRadius: 8, flexShrink: 0 }} />
              <div style={{ minWidth: 0, flex: 1 }}>
                <div style={{ fontWeight: 600, fontSize: 12 }}>{group.album?.title ?? "—"}</div>
                <div style={{ fontSize: 11, opacity: 0.8 }}>{group.album?.artist ?? "—"}</div>
              </div>
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: 2, paddingLeft: 2 }}>
              {group.items.map((item, idx) => {
                const queueIdx = group.startQueueIndex + idx;
                const isCurrent = playingTrackKey === item.track.key;
                return (
                  <div
                    key={item.track.key}
                    role="button"
                    tabIndex={0}
                    onClick={() => onPlayQueueIndex(queueIdx)}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      onTrackContextMenu?.(item.track, item.album, group.items.map((i) => i.track), idx, e);
                    }}
                    onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPlayQueueIndex(queueIdx); } }}
                    style={{
                      fontSize: 12,
                      padding: "5px 8px",
                      borderRadius: 6,
                      background: isCurrent ? "var(--app-accent-dim)" : "var(--app-surface)",
                      borderLeft: isCurrent ? "3px solid var(--app-accent)" : "3px solid transparent",
                      cursor: "pointer",
                      display: "flex",
                      alignItems: "center",
                gap: 8,
                    }}
                  >
                    <span style={{ opacity: isCurrent ? 1 : 0.7, width: 18, flexShrink: 0 }}>{isCurrent ? "▶" : (item.track.index ?? idx + 1)}</span>
                    <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{item.track.title}</span>
                    <span style={{ fontSize: 11, opacity: 0.8, flexShrink: 0 }}>{formatDuration(item.track.duration)}</span>
                  </div>
                );
              })}
            </div>
          </div>
        );
      })}
    </div>
  );
}

function MiniQueueSidebar({
  queue,
  currentIndex,
  token,
  onPlayQueueIndex,
  onTrackContextMenu,
  onRemoveFromQueue,
}: {
  queue: QueueItem[];
  currentIndex: number;
  token: string;
  onPlayQueueIndex: (index: number) => void;
  onTrackContextMenu?: (track: PlexTrack, album: PlexAlbum | null, trackList: PlexTrack[], index: number, e: React.MouseEvent) => void;
  onRemoveFromQueue?: (queueIndex: number) => void;
}) {
  const [hoveredQueueIdx, setHoveredQueueIdx] = useState<number | null>(null);
  const queueFromCurrent = queue.slice(currentIndex);

  if (queueFromCurrent.length === 0) {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: 12, flex: 1, minHeight: 0 }}>
        <div style={{ fontSize: 14, textTransform: "uppercase", letterSpacing: 0.08, opacity: 0.9, fontWeight: 600, flexShrink: 0 }}>Queue · 0</div>
        <div style={{ flex: 1, minHeight: 0 }} />
      </div>
    );
  }

  const groups: { album: PlexAlbum | null; items: QueueItem[]; startQueueIndex: number }[] = [];
  let idx = 0;
  while (idx < queueFromCurrent.length) {
    const albumKey = queueFromCurrent[idx].album?.key ?? null;
    const groupItems: QueueItem[] = [];
    const startQueueIndex = currentIndex + idx;
    while (idx < queueFromCurrent.length && (queueFromCurrent[idx].album?.key ?? null) === albumKey) {
      groupItems.push(queueFromCurrent[idx]);
      idx++;
    }
    if (groupItems.length > 0) groups.push({ album: groupItems[0].album, items: groupItems, startQueueIndex });
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, overflow: "hidden", minHeight: 0, flex: 1 }}>
      <div style={{ fontSize: 14, textTransform: "uppercase", letterSpacing: 0.08, opacity: 0.9, fontWeight: 600, flexShrink: 0 }}>
        Queue · {queueFromCurrent.length}
      </div>
      <div className="hide-scrollbar" style={{ flex: 1, minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column", gap: 10 }}>
        {groups.map((group) => {
          const thumbUrl = getAlbumThumbUrl(group.album?.thumb, token ?? undefined);
          if (group.items.length === 1) {
            const item = group.items[0];
            const queueIdx = group.startQueueIndex;
            const isCurrent = currentIndex === queueIdx;
            return (
              <div
                key={`${item.track.key}-${queueIdx}`}
                role="button"
                tabIndex={0}
                onClick={() => onPlayQueueIndex(queueIdx)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  onTrackContextMenu?.(item.track, item.album, [item.track], 0, e);
                }}
                onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPlayQueueIndex(queueIdx); } }}
                onMouseEnter={() => setHoveredQueueIdx(queueIdx)}
                onMouseLeave={() => setHoveredQueueIdx(null)}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  padding: "6px 8px",
                  borderRadius: 8,
                  background: isCurrent ? "var(--app-accent-dim)" : "var(--app-surface)",
                  borderLeft: isCurrent ? "3px solid var(--app-accent)" : "3px solid transparent",
                  cursor: "pointer",
                  fontSize: 13,
                  flexShrink: 0,
                }}
              >
                <AlbumCover thumbUrl={thumbUrl} style={{ width: 36, height: 36, borderRadius: 6, flexShrink: 0 }} />
                <div style={{ minWidth: 0, flex: 1, overflow: "hidden" }}>
                  <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontWeight: isCurrent ? 600 : 500, fontSize: 13 }}>{group.items[0].track.title}</div>
                  {group.album && (
                    <div style={{ fontSize: 11, opacity: 0.8, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", marginTop: 1 }}>{group.album.artist ?? "—"}</div>
                  )}
                </div>
                {onRemoveFromQueue && (
                  <div style={{ width: 20, height: 20, flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center" }}>
                    {hoveredQueueIdx === queueIdx && (
                      <button
                        type="button"
                        title="Remove from queue"
                        onClick={(e) => { e.stopPropagation(); onRemoveFromQueue(queueIdx); }}
                        style={{ width: 20, height: 20, padding: 0, border: "none", borderRadius: 4, background: "transparent", color: "var(--app-muted)", fontSize: 14, lineHeight: 1, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center" }}
                      >
                        ×
                      </button>
                    )}
                  </div>
                )}
              </div>
            );
          }
          return (
            <div key={group.startQueueIndex} style={{ display: "flex", flexDirection: "column", gap: 6, flexShrink: 0 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                <AlbumCover thumbUrl={thumbUrl} style={{ width: 44, height: 44, borderRadius: 8, flexShrink: 0 }} />
                <div style={{ minWidth: 0, flex: 1, fontSize: 15, fontWeight: 600, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{group.album?.title ?? "—"}</div>
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 4, paddingLeft: 4 }}>
                {group.items.map((item, idx) => {
                  const queueIdx = group.startQueueIndex + idx;
                  const isCurrent = currentIndex === queueIdx;
                  return (
                    <div
                      key={item.track.key}
                      role="button"
                      tabIndex={0}
                      onClick={() => onPlayQueueIndex(queueIdx)}
                      onContextMenu={(e) => {
                        e.preventDefault();
                        onTrackContextMenu?.(item.track, item.album, group.items.map((i) => i.track), idx, e);
                      }}
                      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPlayQueueIndex(queueIdx); } }}
                      onMouseEnter={() => setHoveredQueueIdx(queueIdx)}
                      onMouseLeave={() => setHoveredQueueIdx(null)}
                      style={{
                        fontSize: 14,
                        padding: "8px 10px",
                        borderRadius: 8,
                        background: isCurrent ? "var(--app-accent-dim)" : "var(--app-surface)",
                        borderLeft: isCurrent ? "4px solid var(--app-accent)" : "4px solid transparent",
                        cursor: "pointer",
                        display: "flex",
                        alignItems: "center",
                        gap: 10,
                      }}
                    >
                      <span style={{ width: 22, flexShrink: 0, opacity: 0.9, fontSize: 15 }}>{isCurrent ? "▶" : (item.track.index ?? idx + 1)}</span>
                      <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: 15 }}>{item.track.title}</span>
                      {onRemoveFromQueue && (
                        <div style={{ width: 24, height: 24, flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center" }}>
                          {hoveredQueueIdx === queueIdx && (
                            <button
                              type="button"
                              title="Remove from queue"
                              onClick={(e) => { e.stopPropagation(); onRemoveFromQueue(queueIdx); }}
                              style={{ width: 24, height: 24, padding: 0, border: "none", borderRadius: 6, background: "transparent", color: "var(--app-muted)", fontSize: 16, lineHeight: 1, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center" }}
                            >
                              ×
                            </button>
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function NowPlayingTheatre({
  queue,
  currentIndex,
  token,
}: {
  queue: QueueItem[];
  currentIndex: number;
  token: string;
}) {
  const current = queue[currentIndex];
  const album = current?.album ?? null;
  const thumbUrl = getAlbumThumbUrl(album?.thumb, token ?? undefined);

  if (!current) {
    return (
      <div style={{ flex: 1, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", minHeight: 320, gap: 12 }}>
        <div style={{ fontSize: 48, opacity: 0.3 }}>♪</div>
        <p style={{ fontSize: 15, color: "var(--app-muted)" }}>Nothing playing. Pick something from Music or the Queue.</p>
      </div>
    );
  }

  return (
    <div
      style={{
        flex: 1,
        minHeight: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <div style={{ position: "relative", zIndex: 1, display: "flex", flexDirection: "column", alignItems: "center", gap: 24, padding: 32, maxWidth: 640 }}>
        <div
          style={{
            width: 480,
            height: 480,
            borderRadius: 16,
            overflow: "hidden",
            boxShadow: "0 24px 48px rgba(0,0,0,0.5), 0 0 0 1px rgba(255,255,255,0.06)",
            flexShrink: 0,
          }}
        >
          <AlbumCover thumbUrl={thumbUrl} style={{ width: "100%", height: "100%" }} />
        </div>
      </div>
    </div>
  );
}

function QueuePage({
  queue,
  currentIndex,
  token,
  onPlayQueueIndex,
  onTrackContextMenu,
  onRemoveFromQueue,
  onReorderQueue,
  onClearQueue,
}: {
  queue: QueueItem[];
  currentIndex: number;
  token: string;
  onPlayQueueIndex: (index: number) => void;
  onTrackContextMenu?: (track: PlexTrack, album: PlexAlbum | null, trackList: PlexTrack[], index: number, e: React.MouseEvent) => void;
  onRemoveFromQueue: (queueIndex: number) => void;
  onReorderQueue: (fromIndex: number, toIndex: number) => void;
  onClearQueue: () => void;
}) {
  const formatDuration = (ms: number | null) =>
    ms != null ? `${Math.floor(ms / 60000)}:${String(Math.floor((ms % 60000) / 1000)).padStart(2, "0")}` : "—";

  const queueFromCurrent = queue.slice(currentIndex);
  const count = queueFromCurrent.length;
  const totalMs = queueFromCurrent.reduce((sum, item) => sum + (item.track.duration ?? 0), 0);
  const totalHours = Math.floor(totalMs / 3600000);
  const totalMins = Math.floor((totalMs % 3600000) / 60000);
  const durationLabel = totalHours > 0 ? `${totalHours}h ${totalMins}m` : `${totalMins}m`;

  const [hoveredQueueIdx, setHoveredQueueIdx] = useState<number | null>(null);
  const [draggingQueueIdx, setDraggingQueueIdx] = useState<number | null>(null);
  const [dropTarget, setDropTarget] = useState<{ queueIdx: number; place: "before" | "after" } | null>(null);

  if (count === 0) {
    return (
      <div style={{ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", minHeight: 276, gap: 14 }}>
        <div style={{ fontSize: 55, opacity: 0.3 }}>♪</div>
        <p style={{ fontSize: 15, color: "var(--app-muted)" }}>Queue is empty. Add tracks from Music.</p>
      </div>
    );
  }

  const groups: { album: PlexAlbum | null; items: QueueItem[]; startQueueIndex: number }[] = [];
  let i = currentIndex;
  while (i < queue.length) {
    const album = queue[i].album;
    const albumKey = album?.key ?? null;
    const groupItems: QueueItem[] = [];
    const start = i;
    while (i < queue.length && (queue[i].album?.key ?? null) === albumKey) {
      groupItems.push(queue[i]);
      i++;
    }
    groups.push({ album, items: groupItems, startQueueIndex: start });
  }

  const albumKeyRunCount = new Map<string | null, number>();
  for (const g of groups) {
    const key = g.album?.key ?? null;
    albumKeyRunCount.set(key, (albumKeyRunCount.get(key) ?? 0) + 1);
  }
  const isAffected = Array.from(albumKeyRunCount.values()).some((c) => c > 1);

  const hamburgerSvg = (
    <svg width={14} height={14} viewBox="0 0 24 24" fill="currentColor" style={{ display: "block" }} aria-hidden>
      <rect x="3" y="5" width="18" height="2" rx="1" />
      <rect x="3" y="11" width="18" height="2" rx="1" />
      <rect x="3" y="17" width="18" height="2" rx="1" />
    </svg>
  );

  const dragHandle = (queueIdx: number) => (
    <div
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData("text/plain", String(queueIdx));
        e.dataTransfer.effectAllowed = "move";
        setDraggingQueueIdx(queueIdx);
      }}
      onDragEnd={() => { setDraggingQueueIdx(null); setDropTarget(null); }}
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => e.stopPropagation()}
      style={{
        width: 24,
        height: 24,
        flexShrink: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        cursor: draggingQueueIdx === queueIdx ? "grabbing" : "grab",
        color: "var(--app-muted)",
        opacity: hoveredQueueIdx === queueIdx || draggingQueueIdx === queueIdx ? 1 : 0,
        transition: "opacity 0.15s ease",
      }}
      title="Drag to reorder"
    >
      {hamburgerSvg}
    </div>
  );

  const renderSingleRow = (
    item: QueueItem,
    queueIdx: number,
    thumbUrl: string | null,
  ) => {
    const isCurrent = currentIndex === queueIdx;
    const showLineAbove = dropTarget?.queueIdx === queueIdx && dropTarget?.place === "before";
    const showLineBelow = dropTarget?.queueIdx === queueIdx && dropTarget?.place === "after";
    const handleDragOver = (e: React.DragEvent, idx: number) => {
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      const rect = e.currentTarget.getBoundingClientRect();
      const place = e.clientY < rect.top + rect.height / 2 ? "before" : "after";
      setDropTarget({ queueIdx: idx, place });
    };
    const handleDrop = (e: React.DragEvent, dropIdx: number) => {
      e.preventDefault();
      setDropTarget(null);
      const from = parseInt(e.dataTransfer.getData("text/plain"), 10);
      const to = dropTarget?.queueIdx === dropIdx ? (dropTarget.place === "before" ? dropIdx : dropIdx + 1) : dropIdx;
      if (!Number.isNaN(from) && from !== to) onReorderQueue(from, to);
    };
    return (
      <div key={`${item.track.key}-${queueIdx}`} style={{ position: "relative" }}>
        {showLineAbove && (
          <div style={{ height: 2, background: "var(--app-accent)", borderRadius: 1, marginBottom: 2, flexShrink: 0 }} aria-hidden />
        )}
        <div
          role="button"
          tabIndex={0}
          onClick={() => onPlayQueueIndex(queueIdx)}
          onContextMenu={(e) => {
            e.preventDefault();
            onTrackContextMenu?.(item.track, item.album, [item.track], 0, e);
          }}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPlayQueueIndex(queueIdx); } }}
          onMouseEnter={() => setHoveredQueueIdx(queueIdx)}
          onMouseLeave={() => setHoveredQueueIdx(null)}
          onDragLeave={() => setDropTarget(null)}
          onDragOver={(e) => handleDragOver(e, queueIdx)}
          onDrop={(e) => handleDrop(e, queueIdx)}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 8,
            padding: "7px 0",
            borderRadius: 0,
            background: isCurrent ? "var(--app-accent-dim)" : "transparent",
            borderLeft: isCurrent ? "3px solid var(--app-accent)" : "3px solid transparent",
            cursor: "pointer",
            fontSize: 14,
            opacity: draggingQueueIdx === queueIdx ? 0.6 : 1,
          }}
        >
          <AlbumCover thumbUrl={thumbUrl} style={{ width: 38, height: 38, borderRadius: 6, flexShrink: 0 }} />
          <div style={{ minWidth: 0, flex: 1, overflow: "hidden" }}>
            <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontWeight: isCurrent ? 600 : 500, fontSize: 14 }}>{item.track.title}</div>
            {item.album && (
              <div style={{ fontSize: 12, opacity: 0.8, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", marginTop: 1 }}>{item.album.artist ?? "—"}</div>
            )}
          </div>
          <div style={{ width: 22, height: 22, flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center" }}>
            {hoveredQueueIdx === queueIdx && (
              <button
                type="button"
                title="Remove from queue"
                onClick={(e) => { e.stopPropagation(); onRemoveFromQueue(queueIdx); }}
                style={{ width: 22, height: 22, padding: 0, border: "none", borderRadius: 4, background: "transparent", color: "var(--app-muted)", fontSize: 15, lineHeight: 1, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center" }}
              >
                ×
              </button>
            )}
          </div>
        </div>
        {showLineBelow && (
          <div style={{ height: 2, background: "var(--app-accent)", borderRadius: 1, marginTop: 2, flexShrink: 0 }} aria-hidden />
        )}
      </div>
    );
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 21, maxWidth: 776 }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 14, flexWrap: "wrap" }}>
        <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--app-muted)", margin: 0, textTransform: "uppercase", letterSpacing: 0.06 }}>
          Now & up next · {count} {count === 1 ? "track" : "tracks"} · {durationLabel}
        </h2>
        <button
          type="button"
          onClick={onClearQueue}
          disabled={count === 0}
          style={{
            padding: "9px 16px",
            fontSize: 13,
            fontWeight: 500,
            color: "var(--app-muted)",
            background: "var(--app-surface)",
            border: "1px solid var(--app-border)",
            borderRadius: 9,
            cursor: count === 0 ? "default" : "pointer",
            opacity: count === 0 ? 0.7 : 1,
          }}
        >
          Clear queue
        </button>
      </div>
      {isAffected ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {queueFromCurrent.map((item, i) => {
            const queueIdx = currentIndex + i;
            const thumbUrl = getAlbumThumbUrl(item.album?.thumb, token ?? undefined);
            return renderSingleRow(item, queueIdx, thumbUrl);
          })}
        </div>
      ) : (
      groups.map((group) => {
        const thumbUrl = getAlbumThumbUrl(group.album?.thumb, token ?? undefined);
        if (group.items.length === 1) {
          return renderSingleRow(group.items[0], group.startQueueIndex, thumbUrl);
        }
        return (
          <div key={group.startQueueIndex} style={{ display: "flex", flexDirection: "column", gap: 9 }}>
            <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
              <AlbumCover thumbUrl={thumbUrl} style={{ width: 48, height: 48, borderRadius: 9, flexShrink: 0 }} />
              <div style={{ minWidth: 0, flex: 1 }}>
                <div style={{ fontWeight: 600, fontSize: 15 }}>{group.album?.title ?? "—"}</div>
                <div style={{ fontSize: 13, opacity: 0.8 }}>{group.album?.artist ?? "—"}</div>
              </div>
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
              {group.items.map((item, idx) => {
                const queueIdx = group.startQueueIndex + idx;
                const isCurrent = currentIndex === queueIdx;
                const showLineAbove = dropTarget?.queueIdx === queueIdx && dropTarget?.place === "before";
                const showLineBelow = dropTarget?.queueIdx === queueIdx && dropTarget?.place === "after";
                const handleDragOverRow = (e: React.DragEvent, index: number) => {
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "move";
                  const rect = e.currentTarget.getBoundingClientRect();
                  const place = e.clientY < rect.top + rect.height / 2 ? "before" : "after";
                  setDropTarget({ queueIdx: index, place });
                };
                const handleDropRow = (e: React.DragEvent, dropIdx: number) => {
                  e.preventDefault();
                  setDropTarget(null);
                  const from = parseInt(e.dataTransfer.getData("text/plain"), 10);
                  const to = dropTarget?.queueIdx === dropIdx ? (dropTarget.place === "before" ? dropIdx : dropIdx + 1) : dropIdx;
                  if (!Number.isNaN(from) && from !== to) onReorderQueue(from, to);
                };
                return (
                  <div key={item.track.key} style={{ position: "relative" }}>
                    {showLineAbove && (
                      <div style={{ height: 2, background: "var(--app-accent)", borderRadius: 1, marginBottom: 2 }} aria-hidden />
                    )}
                    <div
                      role="button"
                      tabIndex={0}
                      onClick={() => onPlayQueueIndex(queueIdx)}
                      onContextMenu={(e) => {
                        e.preventDefault();
                        onTrackContextMenu?.(item.track, item.album, group.items.map((i) => i.track), idx, e);
                      }}
                      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPlayQueueIndex(queueIdx); } }}
                      onMouseEnter={() => setHoveredQueueIdx(queueIdx)}
                      onMouseLeave={() => setHoveredQueueIdx(null)}
                      onDragLeave={() => setDropTarget(null)}
                      onDragOver={(e) => handleDragOverRow(e, queueIdx)}
                      onDrop={(e) => handleDropRow(e, queueIdx)}
                      style={{
                        fontSize: 13,
                        padding: "6px 10px",
                        borderRadius: 8,
                        background: isCurrent ? "var(--app-accent-dim)" : "var(--app-surface)",
                        borderLeft: isCurrent ? "3px solid var(--app-accent)" : "3px solid transparent",
                        cursor: "pointer",
                        display: "flex",
                        alignItems: "center",
                        gap: 8,
                        opacity: draggingQueueIdx === queueIdx ? 0.6 : 1,
                      }}
                    >
                      {dragHandle(queueIdx)}
                      <span style={{ opacity: isCurrent ? 1 : 0.7, width: 18, flexShrink: 0, fontSize: 12 }}>{item.track.index ?? idx + 1}</span>
                      <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: 14 }}>{item.track.title}</span>
                      {hoveredQueueIdx !== queueIdx && <span style={{ fontSize: 12, opacity: 0.8, flexShrink: 0 }}>{formatDuration(item.track.duration)}</span>}
                      {hoveredQueueIdx === queueIdx && (
                        <button
                          type="button"
                          title="Remove from queue"
                          onClick={(e) => { e.stopPropagation(); onRemoveFromQueue(queueIdx); }}
                          style={{ width: 22, height: 22, padding: 0, border: "none", borderRadius: 6, background: "transparent", color: "var(--app-muted)", fontSize: 14, lineHeight: 1, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0 }}
                        >
                          ×
                        </button>
                      )}
                    </div>
                    {showLineBelow && (
                      <div style={{ height: 2, background: "var(--app-accent)", borderRadius: 1, marginTop: 2 }} aria-hidden />
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        );
      }) ) }
    </div>
  );
}

const SIDEBAR_WIDTH_MIN = 200;
const SIDEBAR_WIDTH_MAX = 420;

type QueueItem = { track: PlexTrack; album: PlexAlbum | null };

type ContextMenuItem = {
  label: string;
  icon: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
};

function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const handleContextMenu = (e: MouseEvent) => {
      e.preventDefault();
      onClose();
    };
    window.addEventListener("click", handleClick, true);
    window.addEventListener("contextmenu", handleContextMenu, true);
    return () => {
      window.removeEventListener("click", handleClick, true);
      window.removeEventListener("contextmenu", handleContextMenu, true);
    };
  }, [onClose]);
  return (
    <div
      ref={ref}
      role="menu"
      style={{
        position: "fixed",
        left: x,
        top: y,
        zIndex: 10000,
        minWidth: 180,
        padding: "6px 0",
        borderRadius: 10,
        background: "var(--app-surface)",
        border: "1px solid var(--app-border)",
        boxShadow: "0 10px 40px rgba(0,0,0,0.4)",
        display: "flex",
        flexDirection: "column",
      }}
    >
      {items.map((item, i) => (
              <button
          key={i}
                type="button"
          role="menuitem"
          disabled={item.disabled}
          onClick={() => {
            if (!item.disabled) {
              item.onClick();
              onClose();
            }
          }}
                style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            width: "100%",
                  padding: "8px 14px",
            border: "none",
            background: "transparent",
            color: item.disabled ? "var(--app-muted)" : "var(--app-text)",
                  fontSize: 13,
            cursor: item.disabled ? "default" : "pointer",
            textAlign: "left",
                }}
              >
          <span style={{ width: 18, height: 18, display: "flex", alignItems: "center", justifyContent: "center", opacity: item.disabled ? 0.5 : 1 }}>
            {item.icon}
          </span>
          {item.label}
              </button>
      ))}
            </div>
  );
}

function shuffleArray<T>(arr: T[]): T[] {
  const out = [...arr];
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

/** Wait for full buffer (or timeout) before starting playback. Load the whole song first, every time. */
function whenFullyBuffered(
  el: HTMLAudioElement,
  durationSec: number,
  onReady: () => void,
  timeoutMs: number
): () => void {
  const tolerance = 0.5;
  const isFull = () => {
    if (durationSec <= 0) return true;
    const b = el.buffered;
    if (b.length === 0) return false;
    return b.end(b.length - 1) >= durationSec - tolerance;
  };
  if (isFull()) {
    onReady();
    return () => {};
  }
  let timeoutId: ReturnType<typeof setTimeout> | null = setTimeout(() => {
    timeoutId = null;
    onReady();
  }, timeoutMs);
  const onProgress = () => {
    if (!isFull()) return;
    el.removeEventListener("progress", onProgress);
    if (timeoutId != null) {
      clearTimeout(timeoutId);
      timeoutId = null;
    }
    onReady();
  };
  el.addEventListener("progress", onProgress);
  return () => {
    el.removeEventListener("progress", onProgress);
    if (timeoutId != null) clearTimeout(timeoutId);
  };
}

const PLAYER_STATE_KEY = "crystalPlayerState";
const MAX_PERSISTED_QUEUE = 100;

function getSavedPlayerState(): {
  queue: QueueItem[];
  currentIndex: number;
  currentTime: number;
  shuffle: boolean;
  repeatMode: "off" | "all" | "one";
  volume: number;
} | null {
  try {
    const raw = window.localStorage.getItem(PLAYER_STATE_KEY);
    if (!raw) return null;
    const data = JSON.parse(raw) as unknown;
    if (!data || typeof data !== "object") return null;
    const q = Array.isArray((data as { queue?: unknown }).queue) ? (data as { queue: QueueItem[] }).queue : [];
    const hasValidItems = q.length > 0 && q.every((item) => item && item.track && typeof (item.track as { partKey?: unknown }).partKey !== "undefined");
    if (!hasValidItems) return null;
    const idx = typeof (data as { currentIndex?: unknown }).currentIndex === "number" ? (data as { currentIndex: number }).currentIndex : 0;
    const index = Math.max(0, Math.min(idx, q.length - 1));
    const time = typeof (data as { currentTime?: unknown }).currentTime === "number" ? (data as { currentTime: number }).currentTime : 0;
    const shuffle = typeof (data as { shuffle?: unknown }).shuffle === "boolean" ? (data as { shuffle: boolean }).shuffle : false;
    const repeatMode = (data as { repeatMode?: string }).repeatMode === "one" || (data as { repeatMode?: string }).repeatMode === "all" ? (data as { repeatMode: "off" | "all" | "one" }).repeatMode : "off";
    const volume = typeof (data as { volume?: unknown }).volume === "number" ? Math.max(0, Math.min(1, (data as { volume: number }).volume)) : 1;
    return { queue: q, currentIndex: index, currentTime: time, shuffle, repeatMode, volume };
  } catch {
    return null;
  }
}

function savePlayerState(state: { queue: QueueItem[]; currentIndex: number; currentTime: number; shuffle: boolean; repeatMode: string; volume: number }) {
  try {
    const toSave = {
      queue: state.queue.slice(0, MAX_PERSISTED_QUEUE),
      currentIndex: state.currentIndex,
      currentTime: 0, // don't persist position; avoids streaming/buffer issues on reopen
      shuffle: state.shuffle,
      repeatMode: state.repeatMode,
      volume: state.volume,
    };
    window.localStorage.setItem(PLAYER_STATE_KEY, JSON.stringify(toSave));
  } catch {
    // ignore quota or parse errors
  }
}

function PlayerBar({
  queue,
  currentIndex,
  isPlaying,
  currentTime,
  duration,
  currentAlbum,
  thumbUrl,
  loadingTrack,
  volume,
  shuffle,
  repeatMode,
  onVolumeChange,
  onPlayPause,
  onSeek,
  onPrevious,
  onNext,
  onShuffle,
  onRepeat,
  onMuteToggle,
  isNowPlaying,
  onNowPlayingToggle,
}: {
  queue: QueueItem[];
  currentIndex: number;
  isPlaying: boolean;
  currentTime: number;
  duration: number;
  currentAlbum: PlexAlbum | null;
  thumbUrl: string | null;
  loadingTrack: boolean;
  volume: number;
  shuffle: boolean;
  repeatMode: "off" | "all" | "one";
  onVolumeChange: (v: number) => void;
  onMuteToggle: () => void;
  onPlayPause: () => void;
  onSeek: (t: number) => void;
  onPrevious: () => void;
  onNext: () => void;
  onShuffle: () => void;
  onRepeat: () => void;
  isNowPlaying: boolean;
  onNowPlayingToggle: () => void;
}) {
  const current = queue[currentIndex];
  const [isDragging, setIsDragging] = useState(false);
  const [scrubTime, setScrubTime] = useState(currentTime);
  const rangeRef = useRef<HTMLInputElement>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const [barWidth, setBarWidth] = useState(1200);
  const max = Math.max(duration || 1, 1);
  const displayTime = isDragging ? scrubTime : currentTime;

  useEffect(() => {
    if (!isDragging) setScrubTime(currentTime);
  }, [currentTime, isDragging]);

  useEffect(() => {
    const el = barRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect?.width;
      if (typeof w === "number" && Number.isFinite(w)) setBarWidth(w);
    });
    ro.observe(el);
    // initial measure (ResizeObserver can be async depending on platform)
    const rect = el.getBoundingClientRect();
    if (rect?.width) setBarWidth(rect.width);
    return () => ro.disconnect();
  }, []);

  // Responsive collapse rules (in order):
  // - hide title/artist
  // - hide fullscreen button
  // - hide timestamps
  // - hide repeat + shuffle
  // - hide volume
  // Tuned to feel less "violent" while resizing.
  const showMetaText = barWidth >= 720;
  const showFullscreen = barWidth >= 640;
  const showTimestamps = barWidth >= 580;
  const showExtraButtons = barWidth >= 540; // shuffle/repeat
  const showVolume = barWidth >= 500;

  const startDrag = () => {
    setScrubTime(currentTime);
    setIsDragging(true);
  };

  const timeStr = (s: number) =>
    `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

  const handleSeekChange = (t: number) => {
    const clamped = Math.max(0, Math.min(max, t));
    setScrubTime(clamped);
  };

  const handleSeekCommit = () => {
    const el = rangeRef.current;
    const t = el ? Number(el.value) : scrubTime;
    const clamped = Math.max(0, Math.min(max, t));
    onSeek(clamped);
    setIsDragging(false);
  };

  const iconBtn = { width: 36, height: 36, border: "none", borderRadius: "50%", background: "transparent", color: "var(--app-text)", cursor: "pointer", fontSize: 18, display: "flex" as const, alignItems: "center", justifyContent: "center" };
  const playPauseBtn = { width: 40, height: 40, margin: "0 8px", border: "none", borderRadius: "50%", background: "#fff", color: "#0f172a", cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center", boxShadow: "0 2px 12px rgba(0,0,0,0.3)", padding: 0 };

  return (
    <div
      ref={barRef}
      style={{
        flexShrink: 0,
        background: "var(--app-bg)",
        borderTop: "1px solid var(--app-border)",
        boxShadow: "0 -4px 24px rgba(0,0,0,0.35)",
        // 3-column grid keeps center controls/seekbar perfectly centered even as left/right content disappears
        display: "grid",
        gridTemplateColumns: "1fr auto 1fr",
        alignItems: "center",
        padding: "8px 16px 10px",
        boxSizing: "border-box",
        gap: 12,
      }}
    >
      {/* Left: art + text */}
      <div style={{ display: "flex", alignItems: "center", gap: 10, minWidth: 0, justifySelf: "start" }}>
        <div style={{ width: 72, height: 72, flexShrink: 0, borderRadius: 6, overflow: "hidden", background: "var(--app-surface)" }}>
          <AlbumCover thumbUrl={thumbUrl} style={{ width: "100%", height: "100%" }} />
        </div>
        {showMetaText && (
          <div style={{ minWidth: 0, flex: 1, display: "flex", flexDirection: "column", gap: 2, justifyContent: "center" }}>
            <div style={{ fontSize: 14, fontWeight: 600, color: "var(--app-text)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", display: "flex", alignItems: "center", gap: 6 }}>
              <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis" }}>{current?.track.title ?? "—"}</span>
              {loadingTrack && (
                <span style={{ flexShrink: 0, fontSize: 11, fontWeight: 500, color: "var(--app-muted)", opacity: 0.9 }}>Loading…</span>
              )}
            </div>
            <div style={{ fontSize: 12, color: "var(--app-muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {currentAlbum?.artist ?? (current ? "—" : "No track")}
            </div>
          </div>
        )}
      </div>
      {/* Center: buttons + seek bar (SVG play/pause for consistent centering at any DPI) */}
      <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 4, minWidth: 0, justifySelf: "center" }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: 4 }}>
          {showExtraButtons && (
            <button type="button" onClick={onShuffle} aria-label="Shuffle" style={{ ...iconBtn, color: shuffle ? "var(--app-accent)" : "var(--app-text)" }} title="Shuffle">
              <svg width={18} height={18} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                <path d="M18 4l3 3-3 3" />
                <path d="M18 20l3-3-3-3" />
                <path d="M3 7h3a5 5 0 0 1 5 5 5 5 0 0 0 5 5h5" />
                <path d="M21 7h-5a5 5 0 0 0-5 5 5 5 0 0 1-5 5h-3" />
              </svg>
            </button>
          )}
          <button type="button" onClick={onPrevious} aria-label="Previous" style={iconBtn}>⏮</button>
          <button type="button" onClick={onPlayPause} aria-label={isPlaying ? "Pause" : "Play"} style={playPauseBtn}>
            {isPlaying ? (
              <svg width={20} height={20} viewBox="0 0 24 24" fill="currentColor" aria-hidden><rect x="6" y="4" width="4" height="16" rx="1" /><rect x="14" y="4" width="4" height="16" rx="1" /></svg>
            ) : (
              <svg width={20} height={20} viewBox="0 0 24 24" fill="currentColor" aria-hidden><path d="M8 5v14l11-7z" /></svg>
            )}
          </button>
          <button type="button" onClick={onNext} aria-label="Next" style={iconBtn}>⏭</button>
          {showExtraButtons && (
            <button type="button" onClick={onRepeat} aria-label="Repeat" style={{ ...iconBtn, color: repeatMode !== "off" ? "var(--app-accent)" : "var(--app-text)" }} title={repeatMode === "one" ? "Repeat one" : repeatMode === "all" ? "Repeat all" : "Repeat off"}>
              {repeatMode === "one" ? <span style={{ fontSize: 12, fontWeight: 700, color: "inherit" }}>1</span> : <svg width={18} height={18} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M17 1l4 4-4 4M3 11V9a4 4 0 014-4h14M7 23l-4-4 4-4M21 13v2a4 4 0 01-4 4H3"/></svg>}
            </button>
          )}
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 10, width: "100%", maxWidth: 380 }}>
          {showTimestamps && (
            <span style={{ fontSize: 11, color: "var(--app-muted)", minWidth: 32, textAlign: "right", fontVariantNumeric: "tabular-nums" }}>{timeStr(displayTime)}</span>
          )}
          <div style={{ flex: 1, minWidth: 0, position: "relative", height: 20 }}>
            <div style={{ position: "absolute", left: 0, right: 0, top: "50%", transform: "translateY(-50%)", height: 6, borderRadius: 3, background: "var(--app-border)", pointerEvents: "none" }}>
              <div style={{ position: "absolute", left: 0, top: 0, bottom: 0, width: `${max > 0 ? (displayTime / max) * 100 : 0}%`, background: "var(--app-text)", borderRadius: 3 }} />
                </div>
                <input
              ref={rangeRef}
              type="range"
              className="player-range"
              min={0}
              max={max}
              step={0.1}
              value={displayTime}
              onMouseDown={startDrag}
              onTouchStart={startDrag}
              onInput={(e) => handleSeekChange(Number((e.target as HTMLInputElement).value))}
              onMouseUp={handleSeekCommit}
              onMouseLeave={() => { if (isDragging) handleSeekCommit(); setIsDragging(false); }}
              onTouchEnd={(e) => { e.preventDefault(); handleSeekCommit(); }}
              style={{ position: "absolute", left: 0, right: 0, top: "50%", transform: "translateY(-50%)", width: "100%", height: 20, margin: 0, cursor: "pointer", accentColor: "var(--app-text)" }}
              aria-label="Seek"
            />
          </div>
          {showTimestamps && (
            <span style={{ fontSize: 11, color: "var(--app-muted)", minWidth: 32, fontVariantNumeric: "tabular-nums" }}>{timeStr(duration)}</span>
          )}
        </div>
      </div>
      {/* Right: volume */}
      <div style={{ display: "flex", alignItems: "center", justifyContent: "flex-end", gap: 8, minWidth: 0, justifySelf: "end" }}>
        {showVolume && (
          <>
            <button type="button" onClick={onMuteToggle} style={{ display: "flex", alignItems: "center", justifyContent: "center", padding: 4, background: "none", border: "none", cursor: "pointer", color: "var(--app-text)", borderRadius: 4 }} aria-label={volume === 0 ? "Unmute" : "Mute"}>
              {volume === 0 ? (
                <svg width={20} height={20} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" style={{ opacity: 0.9 }}>
                  <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
                  <line x1={23} y1={9} x2={17} y2={15} />
                  <line x1={17} y1={9} x2={23} y2={15} />
                </svg>
              ) : (
                <svg width={20} height={20} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" style={{ opacity: 0.9 }}>
                  <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
                  <path d="M15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14" />
                </svg>
              )}
            </button>
            <div style={{ display: "flex", alignItems: "center", height: 24, position: "relative", width: 96 }}>
              <div style={{ position: "absolute", left: 0, right: 0, top: "50%", transform: "translateY(-50%)", height: 6, borderRadius: 3, background: "var(--app-border)", pointerEvents: "none" }}>
                <div style={{ position: "absolute", left: 0, top: 0, bottom: 0, width: `${volume * 100}%`, background: "var(--app-text)", borderRadius: 3 }} />
              </div>
              <input type="range" className="player-range" min={0} max={1} step={0.05} value={volume} onChange={(e) => onVolumeChange(Number(e.target.value))} style={{ position: "relative", width: "100%", height: 20, cursor: "pointer", margin: 0, padding: 0 }} aria-label="Volume" />
            </div>
          </>
        )}
        {showFullscreen && (
          <button
            type="button"
            onClick={onNowPlayingToggle}
            title={isNowPlaying ? "Exit fullscreen" : "Now Playing"}
            aria-label={isNowPlaying ? "Exit fullscreen" : "Now Playing"}
            style={{ display: "flex", alignItems: "center", justifyContent: "center", padding: 6, background: "none", border: "none", cursor: "pointer", color: "var(--app-text)", borderRadius: 4 }}
          >
            {isNowPlaying ? (
              <svg width={20} height={20} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                <path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7" />
              </svg>
            ) : (
              <svg width={20} height={20} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                <path d="M8 3H3v5M21 8V3h-5M3 16v5h5M16 21h5v-5" />
              </svg>
            )}
          </button>
        )}
      </div>
    </div>
  );
}

function App() {
  const [status, setStatus] = useState<PlexStatus>({ state: "unknown" });
  const [view, setView] = useState<MainView>("library");
  const [selectedAlbum, setSelectedAlbum] = useState<PlexAlbum | null>(null);
  const [libraryAlbums, setLibraryAlbums] = useState<PlexAlbum[] | null>(null);
  const [preloadedTracks, setPreloadedTracksState] = useState<Record<string, PlexTrack[]>>({});
  const [layoutLocked, setLayoutLocked] = useState(true);
  type SettingsTab = "playback" | "appearance" | "library" | "tracking";
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("playback");
  const [sidebarWidth, setSidebarWidthState] = useState(260);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const prevViewRef = useRef<MainView>("library");
  useEffect(() => {
    if (prevViewRef.current === "nowPlaying" && view !== "nowPlaying") {
      setSidebarCollapsed(false);
    }
    prevViewRef.current = view;
  }, [view]);
  const [queue, setQueue] = useState<QueueItem[]>([]);
  const [currentIndex, setCurrentIndex] = useState(0);
  /** Tracks played this session (in order), for "back" navigation. Cleared on refresh. */
  const [sessionHistory, setSessionHistory] = useState<QueueItem[]>([]);
  const [sessionHistoryIndex, setSessionHistoryIndex] = useState(-1);
  const skipNextSessionHistoryAddRef = useRef(false);
  const [isPlaying, setIsPlaying] = useState(false);
  type ContextMenuTarget = { x: number; y: number; type: "album"; album: PlexAlbum } | { x: number; y: number; type: "track"; track: PlexTrack; album: PlexAlbum | null; trackList: PlexTrack[]; trackIndex: number };
  const [contextMenu, setContextMenu] = useState<ContextMenuTarget | null>(null);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [loadingTrack, setLoadingTrack] = useState(false);
  const [volume, setVolume] = useState(() => getSavedPlayerState()?.volume ?? 1);
  type RepeatMode = "off" | "all" | "one";
  const [repeatMode, setRepeatMode] = useState<RepeatMode>(() => getSavedPlayerState()?.repeatMode ?? "off");
  const [shuffle, setShuffle] = useState(() => getSavedPlayerState()?.shuffle ?? false);
  /** When shuffle is on, we keep the pre-shuffle order here so "shuffle off" can restore. Not persisted. */
  const [originalQueueOrder, setOriginalQueueOrder] = useState<QueueItem[] | null>(null);
  const [streamingQualityLocal, setStreamingQualityLocal] = useState<StreamingQuality>(() =>
    (window.localStorage.getItem("streamingQualityLocal") as StreamingQuality) || "320"
  );
  const [streamingQualityRemote, setStreamingQualityRemote] = useState<StreamingQuality>(() =>
    (window.localStorage.getItem("streamingQualityRemote") as StreamingQuality) || "320"
  );
  const [playbackError, setPlaybackError] = useState<string | null>(null);
  /** Increment to force stream effect to re-run and fetch a fresh URL (e.g. after idle stream expired). */
  const [streamReloadTrigger, setStreamReloadTrigger] = useState(0);
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [selectedPlaylistId, setSelectedPlaylistId] = useState<string | null>(null);
  const [playlistNameEditId, setPlaylistNameEditId] = useState<string | null>(null);
  const [playlistNameEditValue, setPlaylistNameEditValue] = useState("");
  const [addToPlaylistContext, setAddToPlaylistContext] = useState<{ track: PlexTrack | null; album: PlexAlbum | null } | null>(null);
  const [newPlaylistName, setNewPlaylistName] = useState("");
  const [showNewPlaylistForm, setShowNewPlaylistForm] = useState(false);
  const [newPlaylistNameForList, setNewPlaylistNameForList] = useState("");
  const [editingPlaylistId, setEditingPlaylistId] = useState<string | null>(null);
  const [playlistMenuOpenId, setPlaylistMenuOpenId] = useState<string | null>(null);
  const [playlistContextMenu, setPlaylistContextMenu] = useState<{ x: number; y: number; playlist: Playlist } | null>(null);
  const [draggingPlaylistTrack, setDraggingPlaylistTrack] = useState<{ playlistId: string; index: number } | null>(null);
  const [dropTargetPlaylist, setDropTargetPlaylist] = useState<{ playlistId: string; index: number; place: "before" | "after" } | null>(null);
  const [cacheResetDone, setCacheResetDone] = useState<boolean | null>(null);
  const [lastFmUsername, setLastFmUsername] = useState<string | null>(() => window.localStorage.getItem("lastFmUsername"));
  const [lastFmConnected, setLastFmConnected] = useState(() => !!window.localStorage.getItem("lastFmSessionKey"));
  const [lastFmConnecting, setLastFmConnecting] = useState(false);
  const [lastFmCompleting, setLastFmCompleting] = useState(false);
  const [lastFmTestLoading, setLastFmTestLoading] = useState(false);
  const [lastFmTestResult, setLastFmTestResult] = useState<{ ok: true } | { ok: false; error: string } | null>(null);
  const [lastFmApiKey, setLastFmApiKey] = useState(() => window.localStorage.getItem("lastFmApiKey") || "");
  const [lastFmApiSecret, setLastFmApiSecret] = useState(() => window.localStorage.getItem("lastFmApiSecret") || "");
  const [localFilesEnabled, setLocalFilesEnabled] = useState(false);
  const [localMusicPath, setLocalMusicPath] = useState<string | null>(null);
  const [localTracks, setLocalTracks] = useState<PlexTrack[]>(() => {
    try {
      const s = window.localStorage.getItem("sonicLocalTracks");
      if (!s) return [];
      const arr = JSON.parse(s);
      return Array.isArray(arr) ? arr : [];
    } catch {
      return [];
    }
  });
  const [localFilesScanning, setLocalFilesScanning] = useState(false);
  const [libraryFilter, setLibraryFilter] = useState<"all" | "local">("all");
  const [lastFmError, setLastFmError] = useState<string | null>(null);
  const [discordClientId, setDiscordClientIdState] = useState(() => window.localStorage.getItem("discordClientId") || "");
  const [discordImageKey, setDiscordImageKeyState] = useState(() => window.localStorage.getItem("discordImageKey") || "");
  const [discordRpcStatus, setDiscordRpcStatus] = useState<{ ok: boolean; message: string } | null>(null);
  const [appFont, setAppFontState] = useState(() => window.localStorage.getItem("appFont") || "");
  const playlistImageInputRef = useRef<HTMLInputElement | null>(null);
  const playlistOpenTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [themeSeed, setThemeSeedState] = useState<string>(() => {
    const stored = window.localStorage.getItem("themeSeed");
    return stored && isValidHex(stored) ? (stored.startsWith("#") ? stored : "#" + stored) : DEFAULT_THEME_SEED;
  });
  const [themeHexInput, setThemeHexInput] = useState(themeSeed);
  const [themeFromAlbumArt, setThemeFromAlbumArt] = useState(() =>
    window.localStorage.getItem("themeFromAlbumArt") === "true"
  );
  const [themeTextMode, setThemeTextMode] = useState<ThemeTextMode>(() => {
    const stored = window.localStorage.getItem("themeTextMode");
    return stored === "light" || stored === "dark" ? stored : "auto";
  });
  const themeApplyTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastAlbumThemeThumbRef = useRef<string | null>(null);
  useEffect(() => {
    setThemeHexInput(themeSeed);
  }, [themeSeed]);
  useEffect(() => {
    window.localStorage.setItem("themeFromAlbumArt", themeFromAlbumArt ? "true" : "false");
  }, [themeFromAlbumArt]);
  useEffect(() => {
    window.localStorage.setItem("themeTextMode", themeTextMode);
  }, [themeTextMode]);
  const [loudnessNormalizer, setLoudnessNormalizer] = useState(() =>
    window.localStorage.getItem("loudnessNormalizer") === "true"
  );
  const [eqPreset, setEqPresetState] = useState<string>(() =>
    window.localStorage.getItem("eqPreset") || "flat"
  );
  const [eqGains, setEqGainsState] = useState<number[]>(() => {
    const stored = window.localStorage.getItem("eqGains");
    if (stored) {
      try {
        const parsed = JSON.parse(stored) as number[];
        if (Array.isArray(parsed) && parsed.length === EQ_BANDS.length) return parsed;
        if (Array.isArray(parsed) && parsed.length === 5) {
          const expanded = EQ_BANDS.map((_, i) => parsed[Math.round((i / (EQ_BANDS.length - 1)) * 4)] ?? 0);
          return expanded;
        }
      } catch {}
    }
    return (EQ_PRESETS.find((p) => p.id === (window.localStorage.getItem("eqPreset") || "flat")) ?? EQ_PRESETS[0]).gains.slice();
  });
  const [eqPreamp, setEqPreampState] = useState<number>(() => {
    const stored = window.localStorage.getItem("eqPreamp");
    if (stored != null) {
      const n = Number(stored);
      if (!Number.isNaN(n)) return Math.max(PREAMP_MIN_DB, Math.min(PREAMP_MAX_DB, n));
    }
    return 0;
  });

  const setThemeSeed = useCallback((hex: string) => {
    setThemeSeedState(hex);
    setThemeHexInput(hex);
    window.localStorage.setItem("themeSeed", hex);
    if (themeApplyTimeoutRef.current) clearTimeout(themeApplyTimeoutRef.current);
    themeApplyTimeoutRef.current = setTimeout(() => {
      themeApplyTimeoutRef.current = null;
      applyThemeFromSeed(hex, themeTextMode);
    }, 200);
  }, [themeTextMode]);

  const commitThemeHexInput = useCallback(() => {
    const normalized = parseHexInput(themeHexInput);
    if (normalized) setThemeSeed(normalized);
    else setThemeHexInput(themeSeed);
  }, [themeHexInput, themeSeed, setThemeSeed]);
  useEffect(() => {
    try {
      applyThemeFromSeed(themeSeed, themeTextMode);
    } catch (_) {
      applyThemeFromSeed(DEFAULT_THEME_SEED, "auto");
    }
    return () => {
      if (themeApplyTimeoutRef.current) clearTimeout(themeApplyTimeoutRef.current);
    };
  }, [themeSeed, themeTextMode]);

  const appFontFamily = appFont
    ? `${appFont}, system-ui, sans-serif`
    : "system-ui, -apple-system, BlinkMacSystemFont, \"Segoe UI\", sans-serif";

  useEffect(() => {
    document.documentElement.style.setProperty("--app-font", appFontFamily);
  }, [appFontFamily]);

  const handleSeek = useCallback((t: number) => {
    const active = activeAudioRef.current;
    if (!active) return;
    const target = Math.max(0, t);
    seekTargetRef.current = target;
    setCurrentTime(target);
    currentTimeRef.current = target;
    if (active.seekable.length > 0) {
      for (let i = 0; i < active.seekable.length; i++) {
        if (target >= active.seekable.start(i) && target <= active.seekable.end(i)) {
          active.currentTime = target;
          seekTargetRef.current = null;
          return;
        }
      }
    }
  }, []);

  // Tab title = current track when playing, else "Sonic"
  const currentForTitle = queue[currentIndex];
  useEffect(() => {
    if (currentForTitle?.track?.title) {
      const artist = currentForTitle.album?.artist?.trim() ?? "";
      document.title = artist ? `${currentForTitle.track.title} | ${artist}` : currentForTitle.track.title;
    } else {
      document.title = "Sonic";
    }
  }, [currentForTitle?.track?.title, currentForTitle?.album?.artist]);

  const setEqPreset = useCallback((id: string) => {
    const preset = EQ_PRESETS.find((p) => p.id === id) ?? EQ_PRESETS[0];
    setEqPresetState(preset.id);
    setEqGainsState(preset.gains.slice());
    window.localStorage.setItem("eqPreset", preset.id);
    window.localStorage.setItem("eqGains", JSON.stringify(preset.gains));
  }, []);
  const setEqGain = useCallback((bandIndex: number, value: number) => {
    setEqGainsState((prev) => {
      const next = [...prev];
      next[bandIndex] = Math.max(EQ_MIN_DB, Math.min(EQ_MAX_DB, value));
      window.localStorage.setItem("eqGains", JSON.stringify(next));
      window.localStorage.setItem("eqPreset", "custom");
      return next;
    });
    setEqPresetState("custom");
  }, []);
  const setEqPreamp = useCallback((db: number) => {
    const v = Math.max(PREAMP_MIN_DB, Math.min(PREAMP_MAX_DB, db));
    setEqPreampState(v);
    window.localStorage.setItem("eqPreamp", String(v));
  }, []);

  const setStreamingQualityLocalPersist = useCallback((q: StreamingQuality) => {
    setStreamingQualityLocal(q);
    window.localStorage.setItem("streamingQualityLocal", q);
  }, []);
  const setStreamingQualityRemotePersist = useCallback((q: StreamingQuality) => {
    setStreamingQualityRemote(q);
    window.localStorage.setItem("streamingQualityRemote", q);
  }, []);
  const [isLocalConnection, setIsLocalConnection] = useState<boolean | null>(null);
  const [connectionRefreshLoading, setConnectionRefreshLoading] = useState(false);
  /** Plex server base URL; when set + local, we build direct stream URLs client-side (no API call = instant start) */
  const [plexServerUri, setPlexServerUri] = useState<string | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const preloadAudioRef = useRef<HTMLAudioElement | null>(null);
  const directPlexAudioRef = useRef<HTMLAudioElement | null>(null);
  const audioContextRef = useRef<AudioContext | null>(null);
  const eqGraphInitedRef = useRef(false);
  const eqFiltersRef = useRef<BiquadFilterNode[] | null>(null);
  const eqPreampGainRef = useRef<GainNode | null>(null);
  const loudnessCompressorRef = useRef<DynamicsCompressorNode | null>(null);
  const sourceMainRef = useRef<MediaElementAudioSourceNode | null>(null);
  const sourcePreloadRef = useRef<MediaElementAudioSourceNode | null>(null);
  const sourceDirectRef = useRef<MediaElementAudioSourceNode | null>(null);
  const currentEqSourceRef = useRef<MediaElementAudioSourceNode | null>(null);
  const resizeRef = useRef<{ type: "sidebar"; startX: number; startW: number } | null>(null);
  const lastLoadedPartKeyRef = useRef<string | null>(null);
  const lastNowPlayingArtRef = useRef<string | null>(null);
  const lastNowPlayingTrackKeyRef = useRef<string | null>(null);
  const pausedAtRef = useRef<number | null>(null);
  const streamGenerationRef = useRef(0);
  const streamRetryCountRef = useRef(0);
  const preloadDelayTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const streamUrlFetchTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const activeAudioRef = useRef<HTMLAudioElement | null>(null);
  const currentBlobUrlRef = useRef<string | null>(null);
  const currentStreamUrlRef = useRef<string | null>(null);
  const isPlayingRef = useRef(false);
  const seekTargetRef = useRef<number | null>(null);
  const resumeWhenFullyBufferedCleanupRef = useRef<(() => void) | null>(null);
  const currentTimeRef = useRef(0);
  const preloadedPartKeyRef = useRef<string | null>(null);
  /** PartKey of the track actually loaded in the preload element (so we only reuse when it matches). */
  const partKeyInPreloadElementRef = useRef<string | null>(null);
  /** True when the currently playing track is in the preload element (after gapless promote). */
  const currentTrackInPreloadRef = useRef(false);
  const hoverPreloadTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const queueLengthRef = useRef(0);
  const pendingAlbumClickRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const volumeBeforeMuteRef = useRef(1);
  const previousViewBeforeNowPlayingRef = useRef<MainView>("library");
  const lastFmRef = useRef<{ artist: string; track: string; album: string; durationSec: number; startTime: number } | null>(null);
  const previousLastFmRef = useRef<{ artist: string; track: string; album: string; durationSec: number; startTime: number } | null>(null);
  const previousCurrentIndexRef = useRef(0);
  /** Throttle Now Playing: max once per (artist,track) per 30s to avoid Last.fm rate limit (29). */
  const lastNowPlayingSentRef = useRef<{ artist: string; track: string; at: number } | null>(null);
  const NOW_PLAYING_THROTTLE_MS = 30000;

  /** Build direct Plex stream URL (same network only). Used so preload + play share one URL = cache hits. */
  const buildDirectPlexUrl = useCallback((serverUri: string, partKey: string, token: string) => {
    const base = serverUri.replace(/\/$/, "");
    const path = partKey.startsWith("/") ? partKey : `/library/parts/${partKey}`;
    return `${base}${path}${path.includes("?") ? "&" : "?"}X-Plex-Token=${encodeURIComponent(token)}`;
  }, []);

  /** Create the master EQ chain once (preamp -> bands -> compressor -> destination). All app audio is routed through this. */
  const ensureEqChain = useCallback(() => {
    if (eqGraphInitedRef.current && eqFiltersRef.current?.length === EQ_BANDS.length) return;
    const ctx = new (window.AudioContext || (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext)();
    audioContextRef.current = ctx;
    const preamp = ctx.createGain();
    eqPreampGainRef.current = preamp;
    preamp.gain.setTargetAtTime(Math.pow(10, eqPreamp / 20), 0, 0.01);
    const filters: BiquadFilterNode[] = [];
    let chain: AudioNode = preamp;
    for (let i = 0; i < EQ_BANDS.length; i++) {
      const f = ctx.createBiquadFilter();
      f.type = "peaking";
      f.frequency.value = EQ_BANDS[i].freq;
      f.Q.value = 1;
      f.gain.setTargetAtTime(Math.max(EQ_MIN_DB, Math.min(EQ_MAX_DB, eqGains[i] ?? 0)), 0, 0.01);
      filters.push(f);
      chain.connect(f);
      chain = f;
    }
    eqFiltersRef.current = filters;
    const comp = ctx.createDynamicsCompressor();
    loudnessCompressorRef.current = comp;
    if (loudnessNormalizer) {
      comp.threshold.setTargetAtTime(-24, 0, 0.01);
      comp.knee.setTargetAtTime(30, 0, 0.01);
      comp.ratio.setTargetAtTime(12, 0, 0.01);
      comp.attack.setTargetAtTime(0.003, 0, 0.01);
      comp.release.setTargetAtTime(0.25, 0, 0.01);
    } else {
      comp.threshold.setTargetAtTime(0, 0, 0.01);
      comp.knee.setTargetAtTime(0, 0, 0.01);
      comp.ratio.setTargetAtTime(1, 0, 0.01);
    }
    chain.connect(comp);
    comp.connect(ctx.destination);
    eqGraphInitedRef.current = true;
    if (ctx.state === "suspended") ctx.resume().catch(() => {});
  }, [eqPreamp, eqGains, loudnessNormalizer]);

  /** Route the currently playing element into the master EQ. All sources (including direct Plex) go through the graph when crossOrigin is set so CORS allows use. */
  const connectActiveSourceToEq = useCallback((el: HTMLAudioElement | null) => {
    if (!el) return;
    ensureEqChain();
    const ctx = audioContextRef.current;
    const preamp = eqPreampGainRef.current;
    if (!ctx || !preamp) return;
    let source: MediaElementAudioSourceNode | null = null;
    if (el === audioRef.current) {
      if (!sourceMainRef.current) sourceMainRef.current = ctx.createMediaElementSource(el);
      source = sourceMainRef.current;
    } else if (el === preloadAudioRef.current) {
      if (!sourcePreloadRef.current) sourcePreloadRef.current = ctx.createMediaElementSource(el);
      source = sourcePreloadRef.current;
    } else if (el === directPlexAudioRef.current) {
      if (!sourceDirectRef.current) sourceDirectRef.current = ctx.createMediaElementSource(el);
      source = sourceDirectRef.current;
    }
    if (!source) return;
    currentEqSourceRef.current?.disconnect();
    source.connect(preamp);
    currentEqSourceRef.current = source;
    if (ctx.state === "suspended") ctx.resume().catch(() => {});
  }, [ensureEqChain]);

  const buildDirectPlexUrlRef = useRef(buildDirectPlexUrl);
  buildDirectPlexUrlRef.current = buildDirectPlexUrl;
  const connectActiveSourceToEqRef = useRef(connectActiveSourceToEq);
  connectActiveSourceToEqRef.current = connectActiveSourceToEq;

  useEffect(() => {
    const preamp = eqPreampGainRef.current;
    if (preamp) {
      const linear = Math.pow(10, eqPreamp / 20);
      preamp.gain.setTargetAtTime(linear, 0, 0.01);
    }
    const ctx = audioContextRef.current;
    if (ctx?.state === "suspended") ctx.resume().catch(() => {});
  }, [eqPreamp]);

  useEffect(() => {
    const filters = eqFiltersRef.current;
    if (!filters || filters.length !== eqGains.length) return;
    eqGains.forEach((gain, i) => {
      filters[i].gain.setTargetAtTime(Math.max(EQ_MIN_DB, Math.min(EQ_MAX_DB, gain)), 0, 0.01);
    });
    const ctx = audioContextRef.current;
    if (ctx?.state === "suspended") ctx.resume().catch(() => {});
  }, [eqGains]);

  useEffect(() => {
    const comp = loudnessCompressorRef.current;
    if (!comp) return;
    if (loudnessNormalizer) {
      comp.threshold.setTargetAtTime(-24, 0, 0.01);
      comp.knee.setTargetAtTime(30, 0, 0.01);
      comp.ratio.setTargetAtTime(12, 0, 0.01);
      comp.attack.setTargetAtTime(0.003, 0, 0.01);
      comp.release.setTargetAtTime(0.25, 0, 0.01);
    } else {
      comp.threshold.setTargetAtTime(0, 0, 0.01);
      comp.knee.setTargetAtTime(0, 0, 0.01);
      comp.ratio.setTargetAtTime(1, 0, 0.01);
    }
  }, [loudnessNormalizer]);

  // Record current track in session history when we start playing it (so user can go "back" through past tracks)
  useEffect(() => {
    const item = queue[currentIndex];
    if (!item || skipNextSessionHistoryAddRef.current) {
      skipNextSessionHistoryAddRef.current = false;
      return;
    }
    setSessionHistory((prev) => {
      const last = prev[prev.length - 1];
      if (last && last.track.key === item.track.key && last.track.partKey === item.track.partKey) return prev;
      return [...prev, item];
    });
  }, [queue, currentIndex]);

  // When we're playing the track that is last in session history, we're "at the end" of history
  useEffect(() => {
    if (sessionHistory.length === 0) {
      setSessionHistoryIndex(-1);
      return;
    }
    const item = queue[currentIndex];
    const last = sessionHistory[sessionHistory.length - 1];
    if (item && last && last.track.key === item.track.key && last.track.partKey === item.track.partKey) {
      setSessionHistoryIndex(sessionHistory.length - 1);
    }
  }, [sessionHistory, queue, currentIndex]);

  const goBackInSessionHistory = useCallback(() => {
    if (sessionHistoryIndex <= 0 || !sessionHistory.length) return false;
    const prevItem = sessionHistory[sessionHistoryIndex - 1];
    skipNextSessionHistoryAddRef.current = true;
    setQueue((q) => [prevItem, ...q]);
    setCurrentIndex(0);
    setSessionHistoryIndex(sessionHistoryIndex - 1);
    setCurrentTime(0);
    setDuration(prevItem.track.duration ? prevItem.track.duration / 1000 : 0);
    setIsPlaying(true);
    lastLoadedPartKeyRef.current = null;
    return true;
  }, [sessionHistory, sessionHistoryIndex]);

  const handlePrevious = useCallback(() => {
    if (goBackInSessionHistory()) return;
    const active = activeAudioRef.current;
    const dur = duration || 0;
    const pastHalf = dur > 0 && currentTime >= dur * 0.5;
    if (pastHalf && active) {
      seekTargetRef.current = null;
      active.currentTime = 0;
      setCurrentTime(0);
    } else if (currentIndex > 0) {
      const item = queue[currentIndex - 1];
      seekTargetRef.current = null;
      setCurrentIndex(currentIndex - 1);
      setCurrentTime(0);
      setDuration(item?.track.duration ? item.track.duration / 1000 : 0);
      setIsPlaying(true);
    } else if (currentIndex === 0 && active) {
      seekTargetRef.current = null;
      active.currentTime = 0;
      setCurrentTime(0);
    }
  }, [goBackInSessionHistory, duration, currentTime, currentIndex, queue]);

  const handleNext = useCallback(() => {
    if (currentIndex < queue.length - 1) {
      const nextIdx = currentIndex + 1;
      const item = queue[nextIdx];
      setCurrentIndex(nextIdx);
      setCurrentTime(0);
      setDuration(item?.track.duration ? item.track.duration / 1000 : 0);
      setIsPlaying(true);
    } else {
      setIsPlaying(false);
    }
  }, [currentIndex, queue]);

  const handlePlayPause = useCallback(() => {
    if (isPlaying) {
      pausedAtRef.current = Date.now();
      setIsPlaying(false);
    } else {
      const pausedDuration = Date.now() - (pausedAtRef.current || 0);
      if (pausedAtRef.current && pausedDuration > 5000) {
        lastLoadedPartKeyRef.current = null;
        setStreamReloadTrigger((c) => c + 1);
        setIsPlaying(true);
      } else {
        setIsPlaying(true);
      }
      pausedAtRef.current = null;
    }
  }, [isPlaying]);

  const mediaCommandHandlersRef = useRef({
    onPrevious: () => {},
    onNext: () => {},
    onPlayPause: () => {},
    onSeek: (_t: number) => {},
  });
  mediaCommandHandlersRef.current = {
    onPrevious: handlePrevious,
    onNext: handleNext,
    onPlayPause: handlePlayPause,
    onSeek: handleSeek,
  };

  useEffect(() => {
    if (typeof window === "undefined" || !window.SonicMedia?.onCommand) return;
    window.SonicMedia.onCommand((cmd: { type: string; position?: number }) => {
      const h = mediaCommandHandlersRef.current;
      if (cmd.type === "previous") h.onPrevious();
      else if (cmd.type === "next") h.onNext();
      else if (cmd.type === "playPause") h.onPlayPause();
      else if (cmd.type === "play") setIsPlaying(true);
      else if (cmd.type === "pause") setIsPlaying(false);
      else if (cmd.type === "seek" && typeof cmd.position === "number") h.onSeek(cmd.position / 1000);
    });
  }, []);

  const setPreloadedTracks = useCallback((key: string, tracks: PlexTrack[]) => {
    setPreloadedTracksState((prev) => (prev[key] === tracks ? prev : { ...prev, [key]: tracks }));
  }, []);

  const preloadedTracksWithLocal = useMemo(() => {
    const out = { ...preloadedTracks };
    const byKey = new Map<string, PlexTrack[]>();
    for (const t of localTracks) {
      const artist = (t as PlexTrack & { artist?: string }).artist ?? "Unknown Artist";
      const album = (t as PlexTrack & { album?: string }).album ?? "Unknown Album";
      const key = `local:album:${artist}\0${album}`;
      if (!byKey.has(key)) byKey.set(key, []);
      byKey.get(key)!.push(t);
    }
    byKey.forEach((tracks, key) => {
      out[key] = [...tracks].sort((a, b) => (a.index ?? 9999) - (b.index ?? 9999));
    });
    return out;
  }, [preloadedTracks, localTracks]);

  const ensureAlbumTracks = useCallback(
    async (album: PlexAlbum): Promise<PlexTrack[]> => {
      if (album.key.startsWith("local:album:")) {
        const cached = preloadedTracksWithLocal[album.key];
        return cached ?? [];
      }
      const cached = preloadedTracks[album.key];
      if (cached?.length) return cached;
      const res = await backendFetch(
        `${API_BASE}/album/${encodeURIComponent(album.key)}/tracks?token=${encodeURIComponent(status.token!)}`
      );
      if (!res.ok) return [];
      const data = (await res.json()) as { tracks?: PlexTrack[] };
      const list = data.tracks ?? [];
      setPreloadedTracksState((prev) => (prev[album.key] === list ? prev : { ...prev, [album.key]: list }));
      return list;
    },
    [preloadedTracks, preloadedTracksWithLocal, status.token]
  );

  const fetchPlaylists = useCallback(async () => {
    try {
      const res = await fetch(`${API_SERVER}/api/playlists`);
      if (res.ok) {
        const data = (await res.json()) as { playlists: Playlist[] };
        setPlaylists(Array.isArray(data.playlists) ? data.playlists : []);
      }
    } catch {
      setPlaylists([]);
    }
  }, []);

  const createPlaylist = useCallback(async (name?: string) => {
    try {
      const res = await fetch(`${API_SERVER}/api/playlists`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: name || "New Playlist" }),
      });
      if (res.ok) {
        const p = (await res.json()) as Playlist;
        setPlaylists((prev) => [...prev, p]);
        setSelectedPlaylistId(p.id);
        return p;
      }
    } catch {}
    return null;
  }, []);

  const updatePlaylist = useCallback(async (id: string, updates: { name?: string; image?: string | null }) => {
    try {
      const res = await fetch(`${API_SERVER}/api/playlists/${id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(updates),
      });
      if (res.ok) {
        const p = (await res.json()) as Playlist;
        setPlaylists((prev) => prev.map((x) => (x.id === id ? p : x)));
        return p;
      }
    } catch {}
    return null;
  }, []);

  const deletePlaylist = useCallback(async (id: string) => {
    try {
      const res = await fetch(`${API_SERVER}/api/playlists/${id}`, { method: "DELETE" });
      if (res.ok) {
        setPlaylists((prev) => prev.filter((p) => p.id !== id));
        if (selectedPlaylistId === id) setSelectedPlaylistId(null);
      }
    } catch {}
  }, [selectedPlaylistId]);

  const addToPlaylist = useCallback(async (playlistId: string, track: PlexTrack, album: PlexAlbum | null) => {
    try {
      const res = await fetch(`${API_SERVER}/api/playlists/${playlistId}/items`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ track, album }),
      });
      if (res.ok) {
        const p = (await res.json()) as Playlist;
        setPlaylists((prev) => prev.map((x) => (x.id === playlistId ? p : x)));
      }
    } catch {}
  }, []);

  const removeFromPlaylist = useCallback(async (playlistId: string, index: number) => {
    try {
      const res = await fetch(`${API_SERVER}/api/playlists/${playlistId}/items/${index}`, { method: "DELETE" });
      if (res.ok) {
        const p = (await res.json()) as Playlist;
        setPlaylists((prev) => prev.map((x) => (x.id === playlistId ? p : x)));
      }
    } catch {}
  }, []);

  const reorderPlaylist = useCallback(async (playlistId: string, fromIndex: number, toIndex: number) => {
    try {
      const res = await fetch(`${API_SERVER}/api/playlists/${playlistId}/reorder`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ fromIndex, toIndex }),
      });
      if (res.ok) {
        const p = (await res.json()) as Playlist;
        setPlaylists((prev) => prev.map((x) => (x.id === playlistId ? p : x)));
      }
    } catch {}
  }, []);

  useEffect(() => {
    if (view === "playlists") fetchPlaylists();
  }, [view, fetchPlaylists]);

  useEffect(() => {
    if (addToPlaylistContext) fetchPlaylists();
  }, [addToPlaylistContext, fetchPlaylists]);

  const playAlbum = useCallback(
    async (album: PlexAlbum) => {
      if (!status.token) return;
      let tracks = preloadedTracks[album.key];
      if (!tracks?.length) tracks = await ensureAlbumTracks(album);
      if (!tracks?.length) return;
      lastLoadedPartKeyRef.current = null;
      if (currentBlobUrlRef.current) {
        URL.revokeObjectURL(currentBlobUrlRef.current);
        currentBlobUrlRef.current = null;
      }
      setOriginalQueueOrder(null);
      const items: QueueItem[] = tracks.map((track) => ({ track, album }));
      setQueue(items);
      setCurrentIndex(0);
      setCurrentTime(0);
      setDuration(tracks[0]?.duration ? tracks[0].duration / 1000 : 0);
      setIsPlaying(true);
    },
    [preloadedTracks, status.token, ensureAlbumTracks]
  );

  const cancelHoverPreload = useCallback(() => {
    if (hoverPreloadTimerRef.current) {
      clearTimeout(hoverPreloadTimerRef.current);
      hoverPreloadTimerRef.current = null;
    }
  }, []);

  // Preload track on hover or mousedown (when queue empty) so click starts fast; same URL as play = cache hit
  const startHoverPreload = useCallback(
    (track: PlexTrack, immediate?: boolean) => {
      if (queue.length > 0) return;
      const partKey = track?.partKey != null ? String(track.partKey).trim() : null;
      if (!partKey) return;
      const isLocal = partKey.startsWith("local:");
      if (!isLocal && !status.token) return;
      if (hoverPreloadTimerRef.current) clearTimeout(hoverPreloadTimerRef.current);
      const delayMs = immediate ? 0 : TRACK_HOVER_PRELOAD_MS;
      const doPreload = () => {
        hoverPreloadTimerRef.current = null;
        const preloadEl = preloadAudioRef.current;
        if (!preloadEl || queueLengthRef.current > 0) return;
        const url = isLocal
          ? `${API_SERVER}/api/local-files/stream?path=${encodeURIComponent(partKey.slice(6))}`
          : (() => {
              const container = (track.container || "").trim().toLowerCase();
              const isM4a = /m4a|mp4|x-m4a/.test(container);
              const params = new URLSearchParams();
              params.set("token", status.token!);
              params.set("path", partKey);
              if (container) params.set("container", container);
              if (isM4a && track.key) {
                params.set("transcode", "1");
                params.set("ratingKey", track.key);
                params.set("musicBitrate", "320");
              }
              return `${API_BASE}/stream?${params.toString()}`;
            })();
        preloadedPartKeyRef.current = partKey;
        partKeyInPreloadElementRef.current = partKey;
        preloadEl.src = url;
      };
      if (delayMs <= 0) doPreload();
      else hoverPreloadTimerRef.current = setTimeout(doPreload, delayMs);
    },
    [queue.length, status.token]
  );

  const playTrackAt = useCallback(
    (trackList: PlexTrack[], index: number, album: PlexAlbum | null) => {
      cancelHoverPreload();
      const track = trackList[index];
      const partKey = track?.partKey != null ? String(track.partKey).trim() : null;
      if (!partKey) return;
      const isLocal = partKey.startsWith("local:");
      if (!isLocal && !status.token) return;
      lastLoadedPartKeyRef.current = null;
      if (currentBlobUrlRef.current) {
        URL.revokeObjectURL(currentBlobUrlRef.current);
        currentBlobUrlRef.current = null;
      }
      const items: QueueItem[] = trackList.map((t) => ({ track: t, album }));
      setQueue((prev) => {
        const idx = Math.min(currentIndex, prev.length);
        return [...prev.slice(0, idx), ...items, ...prev.slice(idx + 1)];
      });
      setCurrentIndex(currentIndex + index);
      setCurrentTime(0);
      const t = trackList[index];
      setDuration(t?.duration ? t.duration / 1000 : 0);
      setIsPlaying(true);
    },
    [status.token, currentIndex, cancelHoverPreload]
  );

  const addAlbumToQueue = useCallback(
    async (album: PlexAlbum) => {
      const tracks = await ensureAlbumTracks(album);
      if (!tracks.length) return;
      const items: QueueItem[] = tracks.map((track) => ({ track, album }));
      setQueue((prev) => [...prev, ...items]);
    },
    [ensureAlbumTracks]
  );

  const shufflePlayAlbum = useCallback(
    async (album: PlexAlbum) => {
      const tracks = await ensureAlbumTracks(album);
      if (!tracks.length) return;
      if (!album.key.startsWith("local:album:") && !status.token) return;
      setSelectedAlbum(album);
      const shuffled = shuffleArray(tracks);
      const items: QueueItem[] = shuffled.map((track) => ({ track, album }));
      lastLoadedPartKeyRef.current = null;
      if (currentBlobUrlRef.current) {
        URL.revokeObjectURL(currentBlobUrlRef.current);
        currentBlobUrlRef.current = null;
      }
      setQueue((prev) => {
        const idx = Math.min(currentIndex, prev.length);
        return [...prev.slice(0, idx), ...items, ...prev.slice(idx + 1)];
      });
      setCurrentTime(0);
      setDuration(shuffled[0]?.duration ? shuffled[0].duration / 1000 : 0);
      setIsPlaying(true);
    },
    [ensureAlbumTracks, status.token, currentIndex]
  );

  const addTrackToQueue = useCallback((track: PlexTrack, album: PlexAlbum | null) => {
    setQueue((prev) => [...prev, { track, album }]);
  }, []);

  const shufflePlayTrack = useCallback(
    (trackList: PlexTrack[], index: number, album: PlexAlbum | null) => {
      const track = trackList[index];
      const partKey = track?.partKey != null ? String(track.partKey).trim() : null;
      if (!partKey) return;
      if (!partKey.startsWith("local:") && !status.token) return;
      const shuffled = shuffleArray(trackList);
      const newIndexInShuffled = shuffled.findIndex((t) => t.key === track.key);
      if (newIndexInShuffled < 0) return;
      setSelectedAlbum(album ?? null);
      const items: QueueItem[] = shuffled.map((t) => ({ track: t, album }));
      lastLoadedPartKeyRef.current = null;
      if (currentBlobUrlRef.current) {
        URL.revokeObjectURL(currentBlobUrlRef.current);
        currentBlobUrlRef.current = null;
      }
      setQueue((prev) => {
        const idx = Math.min(currentIndex, prev.length);
        return [...prev.slice(0, idx), ...items, ...prev.slice(idx + 1)];
      });
      setCurrentIndex(currentIndex + newIndexInShuffled);
      setCurrentTime(0);
      setDuration(shuffled[newIndexInShuffled]?.duration ? shuffled[newIndexInShuffled].duration! / 1000 : 0);
      setIsPlaying(true);
    },
    [status.token, currentIndex]
  );

  const playQueueIndex = useCallback((index: number) => {
    setCurrentIndex(index);
    setCurrentTime(0);
    const item = queue[index];
    setDuration(item?.track.duration ? item.track.duration / 1000 : 0);
    setIsPlaying(true);
  }, [queue]);

  const removeFromQueue = useCallback((queueIndex: number) => {
    setOriginalQueueOrder(null);
    setQueue((prev) => {
      const next = prev.filter((_, i) => i !== queueIndex);
      if (queueIndex < currentIndex) {
        setCurrentIndex((p) => Math.max(0, p - 1));
      } else if (queueIndex === currentIndex && next.length > 0) {
        const nextIdx = Math.min(currentIndex, next.length - 1);
        const newCurrent = next[nextIdx];
        setCurrentIndex(nextIdx);
        setCurrentTime(0);
        setDuration(newCurrent.track.duration ? newCurrent.track.duration / 1000 : 0);
        setIsPlaying(true);
      } else if (queueIndex === currentIndex && next.length === 0) {
        setIsPlaying(false);
      }
      return next;
    });
  }, [currentIndex]);

  const reorderQueue = useCallback((fromIndex: number, toIndex: number) => {
    if (fromIndex === toIndex) return;
    setOriginalQueueOrder(null);
    setQueue((prev) => {
      const copy = [...prev];
      const [item] = copy.splice(fromIndex, 1);
      copy.splice(toIndex, 0, item);
      const wasPlayingKey = prev[currentIndex]?.track.key;
      const newTrackAtZero = copy[0];
      if (newTrackAtZero && newTrackAtZero.track.key !== wasPlayingKey) {
        lastLoadedPartKeyRef.current = null;
        setCurrentTime(0);
        setDuration(newTrackAtZero.track.duration ? newTrackAtZero.track.duration / 1000 : 0);
        setIsPlaying(true);
      }
      return copy;
    });
    setCurrentIndex(0);
  }, [currentIndex]);

  const clearQueue = useCallback(() => {
    setOriginalQueueOrder(null);
    setQueue([]);
    setCurrentIndex(0);
    setSessionHistory([]);
    setSessionHistoryIndex(-1);
    setCurrentTime(0);
    setDuration(0);
    setIsPlaying(false);
    lastLoadedPartKeyRef.current = null;
    currentStreamUrlRef.current = null;
    if (currentBlobUrlRef.current) {
      URL.revokeObjectURL(currentBlobUrlRef.current);
      currentBlobUrlRef.current = null;
    }
    const audio = audioRef.current;
    if (audio) {
      audio.removeAttribute("src");
      audio.load();
    }
    const direct = directPlexAudioRef.current;
    if (direct) {
      direct.removeAttribute("src");
      direct.load();
    }
  }, []);

  const refreshLocalFiles = useCallback(() => {
    setLocalFilesScanning(true);
    fetch(`${API_SERVER}/api/local-files/scan`)
      .then((r) => (r.ok ? r.json() : null))
      .then((data: { tracks?: PlexTrack[]; scanId?: number } | null) => {
        if (data && Array.isArray(data.tracks)) {
          setLocalTracks(data.tracks);
          if (data.scanId != null) localArtScanId = data.scanId;
          else localArtScanId = Date.now();
        }
      })
      .catch(() => {})
      .finally(() => setLocalFilesScanning(false));
  }, []);

  const handleAlbumClick = useCallback((album: PlexAlbum) => {
    if (pendingAlbumClickRef.current) {
      clearTimeout(pendingAlbumClickRef.current);
      pendingAlbumClickRef.current = null;
    }
    pendingAlbumClickRef.current = setTimeout(() => {
      setSelectedAlbum((prev) => (prev?.key === album.key ? null : album));
      pendingAlbumClickRef.current = null;
    }, 300);
  }, []);

  const handleAlbumDoubleClick = useCallback(
    (album: PlexAlbum) => {
      if (pendingAlbumClickRef.current) {
        clearTimeout(pendingAlbumClickRef.current);
        pendingAlbumClickRef.current = null;
      }
      playAlbum(album);
    },
    [playAlbum]
  );

  const lastTimeUpdateRef = useRef(0);
  useEffect(() => {
    const mainEl = audioRef.current;
    const preloadEl = preloadAudioRef.current;
    const directEl = directPlexAudioRef.current;
    if (!mainEl) return;
    if (!activeAudioRef.current) activeAudioRef.current = mainEl;
    const onTimeUpdate = (e: Event) => {
      const el = e.target as HTMLAudioElement;
      if (el !== activeAudioRef.current) return;
      const target = seekTargetRef.current;
      if (target != null) {
        if (Math.abs(el.currentTime - target) < 0.5) {
          seekTargetRef.current = null;
        } else if (el.seekable.length > 0) {
          for (let i = 0; i < el.seekable.length; i++) {
            if (target >= el.seekable.start(i) && target <= el.seekable.end(i)) {
              el.currentTime = target;
              setCurrentTime(target);
              currentTimeRef.current = target;
              seekTargetRef.current = null;
              break;
            }
          }
        }
        return;
      }
      const t = el.currentTime;
      currentTimeRef.current = t;
      const now = Date.now();
      if (now - lastTimeUpdateRef.current >= 50) {
        lastTimeUpdateRef.current = now;
        setCurrentTime(t);
      }
    };
    const onLoadedMetadata = (e: Event) => {
      const el = e.target as HTMLAudioElement;
      if (el !== activeAudioRef.current) return;
      const d = el.duration;
      if (typeof d === "number" && Number.isFinite(d) && d > 0) setDuration(d);
    };
    const onEnded = (e: Event) => {
      const el = e.target as HTMLAudioElement;
      if (el !== activeAudioRef.current) return;
      const mode = repeatMode;
      if (mode === "one") {
        const active = activeAudioRef.current;
        if (active && currentStreamUrlRef.current) {
          setCurrentTime(0);
          active.src = currentStreamUrlRef.current;
          active.currentTime = 0;
          connectActiveSourceToEqRef.current(active);
          active.play().catch(() => setIsPlaying(false));
        }
        return;
      }
      setCurrentIndex((i) => {
        let next = i + 1;
        if (next >= queue.length) {
          if (mode === "all" && queue.length > 0) next = 0;
          else {
            setIsPlaying(false);
            return i;
          }
        }
        if (queue[next]?.track.partKey && status.token) {
          const item = queue[next];
          setCurrentTime(0);
          setDuration(item.track.duration ? item.track.duration / 1000 : 0);
          setIsPlaying(true);
        } else setIsPlaying(false);
        return next;
      });
    };
    const onError = (e: Event) => {
      const el = e.target as HTMLAudioElement;
      if (el !== activeAudioRef.current) return;
      const err = el.error;
      // MEDIA_ERR_NETWORK (2) or SRC_NOT_SUPPORTED (4). Plex music streams don't timeout; only reload on actual failure.
      if (err?.code === 2 || err?.code === 4) {
        // If the track is already fully buffered, just resume play — no new URL needed
        const dur = el.duration;
        const buf = el.buffered;
        const fullyBuffered = dur > 0 && buf.length > 0 && buf.end(buf.length - 1) >= dur - 0.5;
        if (fullyBuffered) {
          setPlaybackError(null);
          if (isPlayingRef.current) {
            connectActiveSourceToEqRef.current(el);
            el.play().catch(() => {});
          }
          return;
        }
        lastLoadedPartKeyRef.current = null;
        setPlaybackError(null);
        setStreamReloadTrigger((t) => t + 1);
        return;
      }
      if (err?.code === 3 && currentBlobUrlRef.current && currentStreamUrlRef.current) {
        if (currentBlobUrlRef.current) {
          URL.revokeObjectURL(currentBlobUrlRef.current);
          currentBlobUrlRef.current = null;
        }
        const t = el.currentTime;
        el.src = currentStreamUrlRef.current;
        el.currentTime = t;
        if (isPlayingRef.current) {
          connectActiveSourceToEqRef.current(el);
          el.play().catch(() => {});
        }
      }
    };
    mainEl.addEventListener("timeupdate", onTimeUpdate);
    mainEl.addEventListener("loadedmetadata", onLoadedMetadata);
    mainEl.addEventListener("ended", onEnded);
    mainEl.addEventListener("error", onError);
    if (preloadEl) {
      preloadEl.addEventListener("timeupdate", onTimeUpdate);
      preloadEl.addEventListener("loadedmetadata", onLoadedMetadata);
      preloadEl.addEventListener("ended", onEnded);
      preloadEl.addEventListener("error", onError);
    }
    if (directEl) {
      directEl.addEventListener("timeupdate", onTimeUpdate);
      directEl.addEventListener("loadedmetadata", onLoadedMetadata);
      directEl.addEventListener("ended", onEnded);
      directEl.addEventListener("error", onError);
    }
    return () => {
      mainEl.removeEventListener("timeupdate", onTimeUpdate);
      mainEl.removeEventListener("loadedmetadata", onLoadedMetadata);
      mainEl.removeEventListener("ended", onEnded);
      mainEl.removeEventListener("error", onError);
      if (preloadEl) {
        preloadEl.removeEventListener("timeupdate", onTimeUpdate);
        preloadEl.removeEventListener("loadedmetadata", onLoadedMetadata);
        preloadEl.removeEventListener("ended", onEnded);
        preloadEl.removeEventListener("error", onError);
      }
      if (directEl) {
        directEl.removeEventListener("timeupdate", onTimeUpdate);
        directEl.removeEventListener("loadedmetadata", onLoadedMetadata);
        directEl.removeEventListener("ended", onEnded);
        directEl.removeEventListener("error", onError);
      }
    };
  }, [queue, currentIndex, status.token, repeatMode, isLocalConnection]);

  // Stream from backend: current track only has priority; stale loads cannot pause or break the player
  useEffect(() => {
    const audio = audioRef.current;
    const preloadEl = preloadAudioRef.current;
    if (!audio || currentIndex >= queue.length) return;
    const item = queue[currentIndex];
    const partKey = item?.track.partKey;
    if (!partKey) return;

    // Defer showing "Loading" so instant loads (e.g. local files, cached) don't flash
    const loadingDelayRef = { current: null as ReturnType<typeof setTimeout> | null };
    const LOADING_DELAY_MS = 120;
    const showLoadingAfterDelay = () => {
      if (loadingDelayRef.current != null) return;
      loadingDelayRef.current = setTimeout(() => {
        loadingDelayRef.current = null;
        setLoadingTrack(true);
      }, LOADING_DELAY_MS);
    };
    const clearLoadingState = () => {
      if (loadingDelayRef.current != null) {
        clearTimeout(loadingDelayRef.current);
        loadingDelayRef.current = null;
      }
      setLoadingTrack(false);
    };

    const whenFullyBufferedCleanupRef = { current: null as (() => void) | null };

    // Local files: stream from our API (no Plex)
    if (typeof partKey === "string" && partKey.startsWith("local:")) {
      const localPath = partKey.slice(6);
      const localUrl = `${API_SERVER}/api/local-files/stream?path=${encodeURIComponent(localPath)}`;
      if (lastLoadedPartKeyRef.current === partKey) {
        setLoadingTrack(false);
        setPlaybackError(null);
        activeAudioRef.current = audio;
        if (isPlayingRef.current) {
          connectActiveSourceToEqRef.current(audio);
          audio?.play().catch(() => {});
        }
        return;
      }
      const localDurationSec = (item.track.duration ?? 0) / 1000 || 0;
      if (audio) { audio.pause(); audio.currentTime = 0; }
      if (preloadEl) { preloadEl.pause(); preloadEl.currentTime = 0; }
      lastLoadedPartKeyRef.current = partKey;
      currentTrackInPreloadRef.current = false;
      activeAudioRef.current = audio;
      currentStreamUrlRef.current = localUrl;
      seekTargetRef.current = null;
      currentTimeRef.current = 0;
      setPlaybackError(null);
      showLoadingAfterDelay();
      setDuration(localDurationSec);
      streamGenerationRef.current += 1;
      audio.src = localUrl;
      const onReadyLocal = () => {
        clearLoadingState();
        if (isPlayingRef.current) {
          connectActiveSourceToEqRef.current(audio);
          audio.play().catch(() => {});
        }
      };
      const localCanThroughTimeoutRef = { current: null as ReturnType<typeof setTimeout> | null };
      const onCanLocal = () => {
        if (localCanThroughTimeoutRef.current != null) {
          clearTimeout(localCanThroughTimeoutRef.current);
          localCanThroughTimeoutRef.current = null;
        }
        whenFullyBufferedCleanupRef.current?.();
        whenFullyBufferedCleanupRef.current = whenFullyBuffered(audio, localDurationSec, onReadyLocal, 20000);
      };
      audio.addEventListener("canplaythrough", onCanLocal, { once: true });
      localCanThroughTimeoutRef.current = setTimeout(() => {
        localCanThroughTimeoutRef.current = null;
        audio.removeEventListener("canplaythrough", onCanLocal);
        onReadyLocal();
      }, 12000);
      const onError = () => {
        clearLoadingState();
        whenFullyBufferedCleanupRef.current?.();
        if (localCanThroughTimeoutRef.current != null) {
          clearTimeout(localCanThroughTimeoutRef.current);
          localCanThroughTimeoutRef.current = null;
        }
        setPlaybackError("Could not play local file.");
      };
      audio.addEventListener("error", onError, { once: true });
      return () => {
        if (loadingDelayRef.current != null) {
          clearTimeout(loadingDelayRef.current);
          loadingDelayRef.current = null;
        }
        whenFullyBufferedCleanupRef.current?.();
        whenFullyBufferedCleanupRef.current = null;
        if (localCanThroughTimeoutRef.current != null) {
          clearTimeout(localCanThroughTimeoutRef.current);
          localCanThroughTimeoutRef.current = null;
        }
        audio.removeEventListener("canplaythrough", onCanLocal);
        audio.removeEventListener("error", onError);
      };
    }

    const container = (item.track.container || "").trim().toLowerCase();
    const useDirectPlex = !!(plexServerUri && status.token);
    const isM4a = /m4a|mp4|x-m4a/.test(container);
    const ratingKey = (item.track as { key?: string }).key;
    const buildProxyStreamUrl = (token: string, pathKey: string, cont: string, rk: string | undefined, addTranscode: boolean) => {
      const params = new URLSearchParams();
      params.set("token", token);
      params.set("path", pathKey);
      if (cont) params.set("container", cont);
      if (addTranscode && rk) {
        params.set("transcode", "1");
        params.set("ratingKey", rk);
        params.set("musicBitrate", "320");
      }
      return `${API_BASE}/stream?${params.toString()}`;
    };
    // M4A/MP4: force proxy+transcode — direct stream can trigger "demuxer error ffmpeg no supported streams" in Electron
    const streamUrl = status.token && (!useDirectPlex || isM4a)
      ? buildProxyStreamUrl(status.token, partKey, container, ratingKey, isM4a)
      : "";

    // Single object so callbacks never touch a lexical that could be TDZ after minify
    const load = { gen: ++streamGenerationRef.current, startTime: currentTime };
    const abortRef = { current: new AbortController() };

    // Same track already loaded (e.g. effect re-ran due to isLocalConnection / deps). Do not start
    // playback here — let the resume effect start playback after full buffer (whenFullyBuffered).
    // Otherwise reopen triggers this path and we play immediately with partial buffer.
    if (lastLoadedPartKeyRef.current === partKey) {
      clearLoadingState();
      setPlaybackError(null);
      activeAudioRef.current = useDirectPlex && !isM4a && directPlexAudioRef.current?.src ? directPlexAudioRef.current : (currentTrackInPreloadRef.current ? preloadEl : audio);
      if (isPlayingRef.current) {
        connectActiveSourceToEqRef.current(activeAudioRef.current);
        // Do not call play() here; resume effect will doWaitFullThenPlay() and start after full buffer
      }
      return;
    }

    // Loading a different track: stop all elements so we never hear the old track when skipping fast.
    if (audio) {
      audio.pause();
      audio.currentTime = 0;
    }
    if (preloadEl) {
      preloadEl.pause();
      preloadEl.currentTime = 0;
    }
    const directEl = directPlexAudioRef.current;
    if (directEl) {
      directEl.pause();
      directEl.currentTime = 0;
    }
    activeAudioRef.current = null;

    // Reusing preload: only when the preload element actually has this track (after promote it has the previous track).
    let urlToApply: string | null = null;
    let reusePreload = false;
    if (partKeyInPreloadElementRef.current === partKey && preloadEl?.src) {
      preloadedPartKeyRef.current = null;
      partKeyInPreloadElementRef.current = null;
      urlToApply = preloadEl.src;
      reusePreload = true;
    }

    if (preloadDelayTimerRef.current) {
      clearTimeout(preloadDelayTimerRef.current);
      preloadDelayTimerRef.current = null;
    }
    if (!reusePreload && preloadEl) {
      preloadedPartKeyRef.current = null;
      partKeyInPreloadElementRef.current = null;
      preloadEl.removeAttribute("src");
      preloadEl.load();
    }

    setPlaybackError(null);
    if (currentBlobUrlRef.current) {
      URL.revokeObjectURL(currentBlobUrlRef.current);
      currentBlobUrlRef.current = null;
    }
    lastLoadedPartKeyRef.current = partKey;
    seekTargetRef.current = null;
    currentTimeRef.current = 0;
    showLoadingAfterDelay();

    // Last.fm: send Now Playing immediately when we switch to this track (don't wait for stream URL)
    const artist = item.album?.artist ?? "";
    const trackTitle = item.track.title ?? "";
    const albumTitle = item.album?.title ?? "";
    const durationSec = item.track.duration ? item.track.duration / 1000 : 0;
    const nowPlayingStartTime = Date.now();
    previousLastFmRef.current = lastFmRef.current;
    lastFmRef.current = { artist, track: trackTitle, album: albumTitle, durationSec, startTime: nowPlayingStartTime };
    const sk = window.localStorage.getItem("lastFmSessionKey");
    const apiKey = window.localStorage.getItem("lastFmApiKey") || "";
    const secret = window.localStorage.getItem("lastFmApiSecret") || "";
    const now = Date.now();
    const lastSent = lastNowPlayingSentRef.current;
    const throttleOk = lastSent && lastSent.artist === artist && lastSent.track === trackTitle && (now - lastSent.at) < NOW_PLAYING_THROTTLE_MS;
    if (sk && artist && trackTitle && apiKey && secret && !throttleOk) {
      lastNowPlayingSentRef.current = { artist, track: trackTitle, at: now };
      fetch(`${API_SERVER}/api/lastfm/nowPlaying`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ sk, artist, track: trackTitle, album: albumTitle || undefined, duration: durationSec || undefined, apiKey, secret }),
      })
        .then(async (res) => {
          const data = await res.json().catch(() => ({}));
          if (!res.ok) {
            const msg = data?.error || "Now Playing failed";
            const rateLimit = data?.errorCode === 29 || (typeof msg === "string" && /rate limit/i.test(msg));
            setLastFmError(rateLimit ? "Last.fm rate limit. Now Playing will retry when you change track." : msg);
          } else setLastFmError(null);
        })
        .catch((e) => setLastFmError(String(e?.message || e)));
    } else if (sk && (!apiKey || !secret)) setLastFmError("API Key and Secret required for scrobbling.");

    const applySrc = (src: string, isRetry = false) => {
      if (streamGenerationRef.current !== load.gen) return;
      if (!isRetry) streamRetryCountRef.current = 0;
      currentTrackInPreloadRef.current = false;
      activeAudioRef.current = audio;
      setPlaybackError(null);
      currentStreamUrlRef.current = src;
      audio.src = src;
      const onLoadedMetadata = () => {
        if (streamGenerationRef.current !== load.gen) return;
        clearLoadingState();
        setPlaybackError(null);
        audio.removeEventListener("loadedmetadata", onLoadedMetadata);
      };
      const startPlay = () => {
        if (streamGenerationRef.current !== load.gen) return;
        clearLoadingState();
        if (load.startTime > 0) {
          audio.currentTime = load.startTime;
          currentTimeRef.current = load.startTime;
        }
        if (isPlayingRef.current) {
          connectActiveSourceToEqRef.current(audio);
          audio.play().catch(() => {
            if (streamGenerationRef.current !== load.gen) return;
            setPlaybackError("Playback was blocked. Click Play to start.");
          });
        }
      };
      const canThroughTimeoutRef = { current: null as ReturnType<typeof setTimeout> | null };
      const onCanThrough = () => {
        if (canThroughTimeoutRef.current != null) {
          clearTimeout(canThroughTimeoutRef.current);
          canThroughTimeoutRef.current = null;
        }
        startPlay();
      };
      audio.addEventListener("canplaythrough", onCanThrough, { once: true });
      canThroughTimeoutRef.current = setTimeout(() => {
        canThroughTimeoutRef.current = null;
        audio.removeEventListener("canplaythrough", onCanThrough);
        onCanThrough();
      }, 12000);
      const onError = () => {
        if (streamGenerationRef.current !== load.gen) return;
        const err = audio.error;
        const code = err?.code ?? 0;
        const isNetworkOrServer = code === 2 || code === 4; // MEDIA_ERR_NETWORK, MEDIA_ERR_SRC_NOT_SUPPORTED (e.g. 500)
        if (isNetworkOrServer && streamRetryCountRef.current < 1) {
          streamRetryCountRef.current = 1;
          audio.removeEventListener("error", onError);
          setTimeout(() => {
            if (streamGenerationRef.current !== load.gen) return;
            const sameSrc = currentStreamUrlRef.current;
            if (sameSrc) {
              audio.src = sameSrc;
              audio.load();
              applySrc(sameSrc, true);
            } else {
              clearLoadingState();
              setPlaybackError(err?.message || "Network error");
            }
          }, 600);
          return;
        }
        clearLoadingState();
        if (canThroughTimeoutRef.current != null) {
          clearTimeout(canThroughTimeoutRef.current);
          canThroughTimeoutRef.current = null;
        }
        const msg = err?.message || (code === 2 ? "Network error" : code === 3 ? "Decode error" : "Could not load audio");
        setPlaybackError(msg);
        audio.removeEventListener("error", onError);
      };
      audio.addEventListener("loadedmetadata", onLoadedMetadata, { once: true });
      audio.addEventListener("error", onError, { once: true });
      setDuration(item.track.duration ? item.track.duration / 1000 : 0);
    };

    /** Direct Plex: use dedicated element; still routes through master EQ like everything else. */
    const applySrcDirect = (src: string, isRetry = false) => {
      if (streamGenerationRef.current !== load.gen) return;
      const el = directPlexAudioRef.current;
      if (!el) return;
      if (!isRetry) streamRetryCountRef.current = 0;
      currentTrackInPreloadRef.current = false;
      activeAudioRef.current = el;
      setPlaybackError(null);
      currentStreamUrlRef.current = src;
      el.crossOrigin = "anonymous";
      el.src = src;
      const onLoadedMetadata = () => {
        if (streamGenerationRef.current !== load.gen) return;
        clearLoadingState();
        setPlaybackError(null);
        el.removeEventListener("loadedmetadata", onLoadedMetadata);
      };
      const startPlayDirect = () => {
        if (streamGenerationRef.current !== load.gen) return;
        clearLoadingState();
        if (load.startTime > 0) {
          el.currentTime = load.startTime;
          currentTimeRef.current = load.startTime;
        }
        if (isPlayingRef.current) {
          connectActiveSourceToEqRef.current(el);
          el.play().catch(() => {
            if (streamGenerationRef.current !== load.gen) return;
            setPlaybackError("Playback was blocked. Click Play to start.");
          });
        }
      };
      const canThroughTimeoutDirectRef = { current: null as ReturnType<typeof setTimeout> | null };
      const onCanThroughDirect = () => {
        if (canThroughTimeoutDirectRef.current != null) {
          clearTimeout(canThroughTimeoutDirectRef.current);
          canThroughTimeoutDirectRef.current = null;
        }
        startPlayDirect();
      };
      el.addEventListener("canplaythrough", onCanThroughDirect, { once: true });
      canThroughTimeoutDirectRef.current = setTimeout(() => {
        canThroughTimeoutDirectRef.current = null;
        el.removeEventListener("canplaythrough", onCanThroughDirect);
        onCanThroughDirect();
      }, 12000);
      const onError = () => {
        if (streamGenerationRef.current !== load.gen) return;
        const err = el.error;
        const code = err?.code ?? 0;
        const isNetworkOrServer = code === 2 || code === 4;
        if (isNetworkOrServer && streamRetryCountRef.current < 1) {
          streamRetryCountRef.current = 1;
          el.removeEventListener("error", onError);
          setTimeout(() => {
            if (streamGenerationRef.current !== load.gen) return;
            const sameSrc = currentStreamUrlRef.current;
            if (sameSrc) {
              el.src = sameSrc;
              el.load();
              applySrcDirect(sameSrc, true);
            } else {
              clearLoadingState();
              setPlaybackError(err?.message || "Network error");
            }
          }, 600);
          return;
        }
        clearLoadingState();
        if (canThroughTimeoutDirectRef.current != null) {
          clearTimeout(canThroughTimeoutDirectRef.current);
          canThroughTimeoutDirectRef.current = null;
        }
        const msg = err?.message || (code === 2 ? "Network error" : code === 3 ? "Decode error" : "Could not load audio");
        setPlaybackError(msg);
        el.removeEventListener("error", onError);
      };
      el.addEventListener("loadedmetadata", onLoadedMetadata, { once: true });
      el.addEventListener("error", onError, { once: true });
      setDuration(item.track.duration ? item.track.duration / 1000 : 0);
    };

    if (urlToApply && reusePreload && preloadEl) {
      // Gapless: play from preload element; wait for canplaythrough (same as first track) then start.
      partKeyInPreloadElementRef.current = partKey; // preloadEl keeps this track
      currentTrackInPreloadRef.current = true;
      activeAudioRef.current = preloadEl;
      currentStreamUrlRef.current = preloadEl.src;
      setPlaybackError(null);
      setDuration(item.track.duration ? item.track.duration / 1000 : 0);
      const reusePreloadTimeoutRef = { current: null as ReturnType<typeof setTimeout> | null };
      if (isPlayingRef.current) {
        const startPreloadPlay = () => {
          clearLoadingState();
          connectActiveSourceToEqRef.current(preloadEl);
          preloadEl.play().catch(() => {});
        };
        if (preloadEl.readyState >= 4) {
          startPreloadPlay();
        } else {
          showLoadingAfterDelay();
          const onPreloadReady = () => {
            clearLoadingState();
            preloadEl.removeEventListener("canplaythrough", onPreloadReady);
            if (reusePreloadTimeoutRef.current != null) {
              clearTimeout(reusePreloadTimeoutRef.current);
              reusePreloadTimeoutRef.current = null;
            }
            connectActiveSourceToEqRef.current(preloadEl);
            preloadEl.play().catch(() => {});
          };
          preloadEl.addEventListener("canplaythrough", onPreloadReady, { once: true });
          reusePreloadTimeoutRef.current = setTimeout(() => {
            reusePreloadTimeoutRef.current = null;
            preloadEl.removeEventListener("canplaythrough", onPreloadReady);
            clearLoadingState();
            connectActiveSourceToEqRef.current(preloadEl);
            preloadEl.play().catch(() => {});
          }, 20000);
        }
      } else {
        clearLoadingState();
      }
      // Preload next track into main element so next advance can use it (or promote again).
      const queueLen = queue.length;
      const repeat = repeatMode;
      let nextIdx = currentIndex + 1;
      if (nextIdx >= queueLen) nextIdx = repeat === "all" ? 0 : -1;
      const nextItem = nextIdx >= 0 ? queue[nextIdx] : null;
      const nextPartKey = nextItem?.track.partKey;
      if (nextPartKey && audio) {
        preloadedPartKeyRef.current = nextPartKey;
        const token = status.token;
        const nextContainer = (nextItem.track.container || "").trim().toLowerCase();
        const nextIsM4a = /m4a|mp4|x-m4a/.test(nextContainer);
        const nextRatingKey = (nextItem.track as { key?: string }).key;
        const useDirectForNext = useDirectPlex && !nextIsM4a;
        if (useDirectForNext && plexServerUri && token) {
          audio.crossOrigin = "anonymous";
          audio.src = buildDirectPlexUrlRef.current(plexServerUri, nextPartKey, token);
        } else if (useDirectForNext && token) {
          fetch(`${API_BASE}/stream-url?token=${encodeURIComponent(token)}&path=${encodeURIComponent(nextPartKey)}`)
            .then((r) => r.json())
            .then((data: { url?: string }) => {
              if (streamGenerationRef.current !== load.gen || preloadedPartKeyRef.current !== nextPartKey) return;
              if (data?.url) {
                audio.crossOrigin = "anonymous";
                audio.src = data.url;
              }
            })
            .catch(() => {});
        } else if (token) {
          audio.src = buildProxyStreamUrl(token, nextPartKey, nextContainer, nextRatingKey, nextIsM4a);
        }
      } else if (audio) {
        preloadedPartKeyRef.current = null;
        audio.removeAttribute("src");
        audio.load();
      }
      return () => {
        if (loadingDelayRef.current != null) {
          clearTimeout(loadingDelayRef.current);
          loadingDelayRef.current = null;
        }
        abortRef.current.abort();
        if (preloadDelayTimerRef.current) {
          clearTimeout(preloadDelayTimerRef.current);
          preloadDelayTimerRef.current = null;
        }
        if (streamUrlFetchTimeoutRef.current) {
          clearTimeout(streamUrlFetchTimeoutRef.current);
          streamUrlFetchTimeoutRef.current = null;
        }
        if (reusePreloadTimeoutRef.current != null) {
          clearTimeout(reusePreloadTimeoutRef.current);
          reusePreloadTimeoutRef.current = null;
        }
      };
    }
    if (urlToApply) {
      applySrc(urlToApply);
    } else if (useDirectPlex && !isM4a && status.token && plexServerUri) {
      // Direct Plex: use dedicated element (no Web Audio graph) so cross-origin stream has audio
      applySrcDirect(buildDirectPlexUrlRef.current(plexServerUri, partKey, status.token));
    } else if (useDirectPlex && !isM4a && status.token) {
      // Direct Plex but no server URI cached yet: fetch once
      fetch(
        `${API_BASE}/stream-url?token=${encodeURIComponent(status.token)}&path=${encodeURIComponent(partKey)}`,
        { signal: abortRef.current.signal }
      )
        .then((r) => r.json())
        .then((data: { url?: string }) => {
          if (streamGenerationRef.current !== load.gen) return;
          const url = data?.url;
          if (url) applySrcDirect(url);
          else {
            clearLoadingState();
            setPlaybackError("Could not get stream URL");
          }
        })
        .catch((err) => {
          if (streamGenerationRef.current !== load.gen) return;
          if (err?.name === "AbortError") return;
          clearLoadingState();
          setPlaybackError(err?.message || "Stream failed");
        });
    } else if (streamUrl) {
      applySrc(streamUrl);
    } else {
      clearLoadingState();
      setPlaybackError(null);
    }

    // Preload next track so it's buffered when user skips (and for gapless)
    const queueLen = queue.length;
    const tok = status.token;
    const repeat = repeatMode;
    let nextIndex = currentIndex + 1;
    if (nextIndex >= queueLen) nextIndex = repeat === "all" ? 0 : -1;
    const nextItem = nextIndex >= 0 ? queue[nextIndex] : null;
    const nextPartKey = nextItem?.track.partKey;

    const doNextPreload = () => {
      const el = preloadAudioRef.current;
      if (!el || !tok || streamGenerationRef.current !== load.gen) return;
      if (nextIndex >= 0 && nextPartKey) {
        preloadedPartKeyRef.current = nextPartKey;
        partKeyInPreloadElementRef.current = nextPartKey;
        const nextContainer = (nextItem.track.container || "").trim().toLowerCase();
        const nextIsM4a = /m4a|mp4|x-m4a/.test(nextContainer);
        const nextRatingKey = (nextItem.track as { key?: string }).key;
        const useDirectForNext = useDirectPlex && !nextIsM4a;
        if (useDirectForNext && plexServerUri) {
          el.crossOrigin = "anonymous";
          el.src = buildDirectPlexUrlRef.current(plexServerUri, nextPartKey, tok);
        } else if (useDirectForNext) {
          el.crossOrigin = "anonymous";
          fetch(`${API_BASE}/stream-url?token=${encodeURIComponent(tok)}&path=${encodeURIComponent(nextPartKey)}`)
            .then((r) => r.json())
            .then((data: { url?: string }) => {
              if (streamGenerationRef.current !== load.gen || preloadedPartKeyRef.current !== nextPartKey) return;
              if (data?.url) {
                el.crossOrigin = "anonymous";
                el.src = data.url;
              }
            })
            .catch(() => {});
        } else {
          el.src = buildProxyStreamUrl(tok, nextPartKey, nextContainer, nextRatingKey, nextIsM4a);
        }
      } else {
        preloadedPartKeyRef.current = null;
        partKeyInPreloadElementRef.current = null;
        el.removeAttribute("src");
        el.load();
      }
    };

    // Start next-track preload immediately so it's not cancelled by effect cleanup (e.g. when loading state updates)
    doNextPreload();

    return () => {
      if (loadingDelayRef.current != null) {
        clearTimeout(loadingDelayRef.current);
        loadingDelayRef.current = null;
      }
      abortRef.current.abort();
      if (preloadDelayTimerRef.current) {
        clearTimeout(preloadDelayTimerRef.current);
        preloadDelayTimerRef.current = null;
      }
      if (streamUrlFetchTimeoutRef.current) {
        clearTimeout(streamUrlFetchTimeoutRef.current);
        streamUrlFetchTimeoutRef.current = null;
      }
    };
  }, [currentIndex, queue, status.token, isPlaying, repeatMode, isLocalConnection, plexServerUri, streamReloadTrigger]);

  // When an album is open and queue is empty, preload its first track so "Play" / "Play track 1" starts instantly
  useEffect(() => {
    const preloadEl = preloadAudioRef.current;
    if (queue.length > 0) return;
    if (!selectedAlbum) return;
    const tracks = preloadedTracksWithLocal[selectedAlbum.key];
    if (!tracks?.length) return;
    const first = tracks[0];
    const partKey = first?.partKey;
    if (!partKey) return;
    const isLocal = typeof partKey === "string" && partKey.startsWith("local:");
    const url = isLocal
      ? `${API_SERVER}/api/local-files/stream?path=${encodeURIComponent(partKey.slice(6))}`
      : (() => {
          if (!status.token) return null;
          const container = (first.container || "").trim().toLowerCase();
          const isM4a = /m4a|mp4|x-m4a/.test(container);
          const params = new URLSearchParams();
          params.set("token", status.token);
          params.set("path", partKey);
          if (container) params.set("container", container);
          if (isM4a && first.key) {
            params.set("transcode", "1");
            params.set("ratingKey", first.key);
            params.set("musicBitrate", "320");
          }
          return `${API_BASE}/stream?${params.toString()}`;
        })();
    if (!url) return;
    preloadedPartKeyRef.current = partKey;
    partKeyInPreloadElementRef.current = partKey;
    if (preloadEl) preloadEl.src = url;
  }, [selectedAlbum?.key, preloadedTracksWithLocal, queue.length, status.token]);

  // Last.fm: scrobble previous track when currentIndex changes (if played 50% or 4 min)
  useEffect(() => {
    const prevIdx = previousCurrentIndexRef.current;
    previousCurrentIndexRef.current = currentIndex;
    if (prevIdx === currentIndex || !queue.length) return;
    const info = previousLastFmRef.current;
    previousLastFmRef.current = null;
    if (!info) return;
    const elapsedSec = (Date.now() - info.startTime) / 1000;
    const threshold = info.durationSec > 0 ? Math.min(info.durationSec / 2, 240) : 240;
    if (elapsedSec >= threshold) {
      const sk = window.localStorage.getItem("lastFmSessionKey");
      const apiKey = window.localStorage.getItem("lastFmApiKey") || "";
      const secret = window.localStorage.getItem("lastFmApiSecret") || "";
      if (sk && info.artist && info.track && apiKey && secret) {
        // Use track start time; cap at (now - 1)s so we never send "timestamp too new" (Last.fm code 4) if client clock is ahead
        const startTs = Math.floor(info.startTime / 1000);
        const maxTs = Math.floor(Date.now() / 1000) - 1;
        const timestamp = Math.min(startTs, maxTs);
        fetch(`${API_SERVER}/api/lastfm/scrobble`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ sk, artist: info.artist, track: info.track, timestamp, album: info.album || undefined, duration: info.durationSec || undefined, apiKey, secret }),
        })
          .then(async (res) => {
            const data = await res.json().catch(() => ({}));
            if (!res.ok) setLastFmError(data?.error || "Scrobble failed");
            else setLastFmError(null);
          })
          .catch((e) => setLastFmError(String(e?.message || e)));
      }
    }
  }, [currentIndex, queue.length]);

  useEffect(() => {
    isPlayingRef.current = isPlaying;
  }, [isPlaying]);

  useEffect(() => {
    const main = audioRef.current;
    const preload = preloadAudioRef.current;
    const direct = directPlexAudioRef.current;
    if (main) main.volume = volume;
    if (preload) preload.volume = volume;
    if (direct) direct.volume = volume;
  }, [volume]);

  useEffect(() => {
    const active = activeAudioRef.current;
    if (!active) return;
    if (isPlaying) {
      const url = currentStreamUrlRef.current;
      const haveData = active.readyState >= 2;
      const haveSource = active.networkState !== 0;
      const doPlay = () => {
        connectActiveSourceToEqRef.current(active);
        active.play().catch(() => setIsPlaying(false));
      };
      const durationSec = (queue[currentIndex]?.track?.duration ?? 0) / 1000;
      const doWaitFullThenPlay = () => {
        resumeWhenFullyBufferedCleanupRef.current?.();
        resumeWhenFullyBufferedCleanupRef.current = whenFullyBuffered(active, durationSec, doPlay, 20000);
      };
      // Plex music streams don't timeout; connection stays valid. Only reload on actual error (see onError).
      if (url && (!haveData || !haveSource)) {
        active.src = url;
        active.currentTime = currentTimeRef.current;
        active.load();
        let resumeTimeoutId: ReturnType<typeof setTimeout> | null = null;
        const onCan = () => {
          if (resumeTimeoutId != null) clearTimeout(resumeTimeoutId);
          doWaitFullThenPlay();
        };
        active.addEventListener("canplaythrough", onCan, { once: true });
        resumeTimeoutId = setTimeout(() => {
          active.removeEventListener("canplaythrough", onCan);
          onCan();
        }, 12000);
        return () => {
          active.removeEventListener("canplaythrough", onCan);
          if (resumeTimeoutId != null) clearTimeout(resumeTimeoutId);
        };
      }
      doWaitFullThenPlay();
    } else {
      active.pause();
      resumeWhenFullyBufferedCleanupRef.current?.();
      resumeWhenFullyBufferedCleanupRef.current = null;
    }
  }, [isPlaying, isLocalConnection]);

  const serverUriFetchedRef = useRef(false);
  useEffect(() => {
    const stored = window.localStorage.getItem("plexAuthToken");
    if (stored) {
      setStatus({ state: "signedIn", token: stored });
      serverUriFetchedRef.current = true;
      Promise.all([
        fetch(`${API_BASE}/server-uri?token=${encodeURIComponent(stored)}`).then((r) => (r.ok ? r.json() : null)),
        fetch(`${API_BASE}/connection?token=${encodeURIComponent(stored)}`).then((r) => (r.ok ? r.json() : null)),
      ]).then(([uriData, connData]) => {
        if (uriData && typeof uriData.serverUri === "string") setPlexServerUri(uriData.serverUri);
        if (connData && typeof connData.local === "boolean") setIsLocalConnection(connData.local);
      }).catch(() => {});
    } else {
      setStatus({ state: "signedOut" });
      serverUriFetchedRef.current = false;
    }
  }, []);

  // Last.fm callback: when we're in the popup after user authorized, exchange token for session (token in localStorage so popup can read it)
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    if (!params.get("lastfm_callback") || !window.opener) return;
    const token = window.localStorage.getItem("lastfm_token");
    if (!token) return;
    (async () => {
      try {
        const apiKey = window.localStorage.getItem("lastFmApiKey") || "";
        const secret = window.localStorage.getItem("lastFmApiSecret") || "";
        const res = await fetch(`${API_SERVER}/api/lastfm/session`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ token, apiKey, secret }),
        });
        const data = await res.json();
        if (data.sk && data.username) {
          window.localStorage.setItem("lastFmSessionKey", data.sk);
          window.localStorage.setItem("lastFmUsername", data.username);
          if (window.opener) window.opener.postMessage({ type: "lastfm_connected", username: data.username }, "*");
        }
      } finally {
        window.localStorage.removeItem("lastfm_token");
        window.close();
      }
    })();
  }, []);

  // Listen for Last.fm popup success so we can update UI
  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      if (e.data?.type === "lastfm_connected") {
        setLastFmConnected(true);
        if (e.data.username) setLastFmUsername(e.data.username);
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  // Restore queue from localStorage only when signed in and current queue is empty (never overwrite a full queue with stale saved state).
  const hasRestoredPlayerRef = useRef(false);
  useEffect(() => {
    if (status.state !== "signedIn") return;
    if (queue.length > 0) return; // never overwrite existing queue (avoids other tabs or stale saves shrinking the queue)
    if (hasRestoredPlayerRef.current) return;
    const saved = getSavedPlayerState();
    if (!saved || saved.queue.length === 0) return;
    hasRestoredPlayerRef.current = true;
    setQueue(saved.queue);
    const idx = Math.min(Math.max(0, saved.currentIndex), saved.queue.length - 1);
    setCurrentIndex(idx);
    setCurrentTime(0); // don't restore position; always start song from beginning after reopen
    const item = saved.queue[idx];
    if (item?.track.duration) setDuration(item.track.duration / 1000);
  }, [status.state, queue.length]);

  // Persist player state when queue, index, time, shuffle, repeat, volume change (so resume after quit restores song + position)
  useEffect(() => {
    if (queue.length === 0) return;
    savePlayerState({ queue, currentIndex, currentTime, shuffle, repeatMode, volume });
  }, [queue, currentIndex, currentTime, shuffle, repeatMode, volume]);

  // Ref of latest state (updated every render so shuffle/interval always see current queue)
  const playerStateRef = useRef({ queue, currentIndex, currentTime, shuffle, repeatMode, volume });
  playerStateRef.current = { queue, currentIndex, currentTime, shuffle, repeatMode, volume };

  useEffect(() => {
    const interval = setInterval(() => {
      const s = playerStateRef.current;
      if (s.queue.length > 0) savePlayerState(s);
    }, 3000);
    const onBeforeUnload = () => {
      const s = playerStateRef.current;
      if (s.queue.length > 0) savePlayerState(s);
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => {
      clearInterval(interval);
      window.removeEventListener("beforeunload", onBeforeUnload);
    };
  }, []);

  useEffect(() => {
    if (status.state !== "signedIn" || !status.token) return;
    let mounted = true;
    fetch(`${API_BASE}/connection?token=${encodeURIComponent(status.token)}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((data) => {
        if (mounted && data && typeof data.local === "boolean") setIsLocalConnection(data.local);
      })
      .catch(() => {});
    return () => { mounted = false; };
  }, [status.state, status.token]);

  useEffect(() => {
    if (status.state === "signedOut") serverUriFetchedRef.current = false;
  }, [status.state]);

  // When user signs in via UI (e.g. Plex pin), fetch server URI so direct streaming works. Skip if we already fetched on restore.
  useEffect(() => {
    if (status.state !== "signedIn" || !status.token) return;
    if (serverUriFetchedRef.current) return;
    serverUriFetchedRef.current = true;
    let mounted = true;
    const token = status.token;
    Promise.all([
      fetch(`${API_BASE}/server-uri?token=${encodeURIComponent(token)}`).then((r) => (r.ok ? r.json() : null)),
      fetch(`${API_BASE}/connection?token=${encodeURIComponent(token)}`).then((r) => (r.ok ? r.json() : null)),
    ]).then(([uriData, connData]) => {
      if (!mounted) return;
      if (uriData && typeof uriData.serverUri === "string") setPlexServerUri(uriData.serverUri);
      if (connData && typeof connData.local === "boolean") setIsLocalConnection(connData.local);
    }).catch(() => {});
    return () => { mounted = false; };
  }, [status.state, status.token]);

  // Discord Rich Presence: update activity when now-playing changes (or clear when nothing playing)
  useEffect(() => {
    const clientId = (window.localStorage.getItem("discordClientId") || "").trim();
    if (!clientId) return;
    const item = queue[currentIndex];
    const trackTitle = item?.track?.title;
    const artistName = item?.album?.artist ?? "";
    const albumName = item?.album?.title ?? "";
    const imageKey = (window.localStorage.getItem("discordImageKey") || "").trim() || undefined;
    const albumArtUrl = (status.state === "signedIn" && status.token
      ? getAlbumThumbUrl(item?.album?.thumb, status.token)
      : null) ?? undefined;
    const body = trackTitle && isPlaying
      ? { clientId, trackTitle, artistName, albumName, imageKey: imageKey || undefined, albumArtUrl }
      : { clientId, clear: true };
    fetch(`${API_SERVER}/api/discord/activity`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }).catch(() => {});
  }, [queue, currentIndex, isPlaying, discordClientId, status.state, status.token]);

  useEffect(() => {
    if (view === "settings" && status.state === "signedIn" && status.token) {
      fetch(`${API_BASE}/connection?token=${encodeURIComponent(status.token)}`)
        .then((r) => (r.ok ? r.json() : null))
        .then((data) => { if (data && typeof data.local === "boolean") setIsLocalConnection(data.local); })
        .catch(() => {});
    }
  }, [view, status.state, status.token]);

  useEffect(() => {
    try {
      window.localStorage.setItem("sonicLocalTracks", JSON.stringify(localTracks));
    } catch (_) {}
  }, [localTracks]);

  // macOS media center (Now Playing): never send an update without artwork when we have a track, or pausing overwrites and clears cover
  useEffect(() => {
    if (typeof window === "undefined" || !window.SonicMedia || typeof window.SonicMedia.setNowPlaying !== "function") return;
    const item = queue[currentIndex];
    const title = item?.track?.title || "";
    const trackKey = item?.track?.key;
    if (trackKey !== lastNowPlayingTrackKeyRef.current) {
      lastNowPlayingTrackKeyRef.current = trackKey ?? null;
      lastNowPlayingArtRef.current = null;
    }
    if (!title) {
      lastNowPlayingArtRef.current = null;
      window.SonicMedia.setNowPlaying({ title: "", artist: "", state: "stopped", currentTime: 0, duration: 0 });
      return;
    }
    const rawArtist = item?.album?.artist;
    const artist = (rawArtist != null && String(rawArtist) !== "undefined") ? String(rawArtist) : "";
    const setMeta = (albumArtDataUrl?: string) => {
      const art = albumArtDataUrl ?? lastNowPlayingArtRef.current ?? undefined;
      if (albumArtDataUrl) lastNowPlayingArtRef.current = albumArtDataUrl;
      window.SonicMedia!.setNowPlaying({
        title,
        artist,
        state: isPlaying ? "playing" : "paused",
        currentTime: Math.round(currentTime * 1000),
        duration: Math.round(duration * 1000),
        albumArt: art,
      });
    };
    // Only push metadata when we have artwork (cached or new). Otherwise we'd overwrite and clear cover on pause.
    if (lastNowPlayingArtRef.current) setMeta();
    let cancelled = false;
    (async () => {
      try {
        const rawThumb = item?.album?.thumb;
        const token = status.state === "signedIn" ? status.token : undefined;
        const thumbUrl = getAlbumThumbUrl(rawThumb, token);
        if (!thumbUrl) return;
        const absUrl = thumbUrl.startsWith("http") ? thumbUrl : (window.location.origin + thumbUrl);
        const res = await fetch(absUrl);
        if (!res.ok || cancelled) return;
        const blob = await res.blob();
        if (cancelled) return;
        const reader = new FileReader();
        reader.onloadend = () => {
          if (cancelled || typeof reader.result !== "string") return;
          setMeta(reader.result);
        };
        reader.readAsDataURL(blob);
      } catch {}
    })();
    return () => { cancelled = true; };
  }, [queue, currentIndex, isPlaying, currentTime, duration, status.state, status.token]);

  // When opening Settings, fetch local files status and sync Last.fm
  useEffect(() => {
    if (view !== "settings") return;
    const sk = window.localStorage.getItem("lastFmSessionKey");
    const username = window.localStorage.getItem("lastFmUsername");
    setLastFmConnected(!!sk);
    if (username) setLastFmUsername(username);
    fetch(`${API_SERVER}/api/local-files/status`)
      .then((r) => (r.ok ? r.json() : null))
      .then((data: { enabled?: boolean; path?: string } | null) => {
        if (data) {
          setLocalFilesEnabled(!!data.enabled);
          setLocalMusicPath(data.path ?? null);
        }
      })
      .catch(() => {});
  }, [view]);

  // When opening Settings and Last.fm is connected, run connection test so status is visible
  useEffect(() => {
    if (view !== "settings") return;
    const sk = window.localStorage.getItem("lastFmSessionKey");
    if (!sk) {
      setLastFmTestResult(null);
      return;
    }
    setLastFmTestResult(null);
    const apiKey = window.localStorage.getItem("lastFmApiKey") || "";
    const secret = window.localStorage.getItem("lastFmApiSecret") || "";
    if (!apiKey || !secret) {
      setLastFmTestResult({ ok: false, error: "API Key and Secret required. Enter them above and reconnect if needed." });
      return;
    }
    let cancelled = false;
    fetch(`${API_SERVER}/api/lastfm/test`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ sk, apiKey, secret }),
    })
      .then((r) => r.json())
      .then((data) => {
        if (cancelled) return;
        if (data.ok === true) {
          setLastFmTestResult({ ok: true });
          setLastFmError(null);
        } else setLastFmTestResult({ ok: false, error: data.error || "Connection failed" });
      })
      .catch((e) => {
        if (!cancelled) setLastFmTestResult({ ok: false, error: (e as Error).message || "Network error" });
      });
    return () => { cancelled = true; };
  }, [view]);

  // Whenever we have a Plex token in the frontend (e.g. restored from
  // localStorage on reload), push it to the backend so it can use it.
  useEffect(() => {
    if (status.state !== "signedIn" || !status.token) return;

    (async () => {
      try {
        await fetch(`${API_BASE}/token`, {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            Accept: "application/json",
          },
          body: JSON.stringify({ token: status.token }),
        });
      } catch (err) {
        console.error("Failed to sync Plex token to backend", err);
      }
    })();
  }, [status]);

  // Preload tracks for the first N albums only so early clicks are instant without overwhelming the server
  const BULK_PREFETCH_ALBUM_LIMIT = 28;
  useEffect(() => {
    if (!libraryAlbums?.length || status.state !== "signedIn" || !status.token) return;
    const albumsToFetch = libraryAlbums.slice(0, BULK_PREFETCH_ALBUM_LIMIT);
    albumsToFetch.forEach((album) => {
      if (preloadedTracksRef.current[album.key]) return;
      fetch(
        `${API_BASE}/album/${encodeURIComponent(album.key)}/tracks?token=${encodeURIComponent(status.token!)}`
      )
        .then((r) => (r.ok ? r.json() : null))
        .then((data: { tracks: PlexTrack[] } | null) => {
          if (data?.tracks) setPreloadedTracks(album.key, data.tracks);
        })
        .catch(() => {});
    });
  }, [libraryAlbums, status.state, status.token, setPreloadedTracks]);

  const prefetchAlbumTracks = useCallback((album: PlexAlbum) => {
    if (album.key.startsWith("local:album:")) return;
    if (preloadedTracksRef.current[album.key] || status.state !== "signedIn" || !status.token) return;
    fetch(
      `${API_BASE}/album/${encodeURIComponent(album.key)}/tracks?token=${encodeURIComponent(status.token!)}`
    )
      .then((r) => (r.ok ? r.json() : null))
      .then((data: { tracks: PlexTrack[] } | null) => {
        if (data?.tracks) setPreloadedTracks(album.key, data.tracks);
      })
      .catch(() => {});
  }, [status.state, status.token, setPreloadedTracks]);

  // When an album is selected, ensure its tracks are in cache so sidebar shows instantly (or as soon as fetch completes)
  const preloadedTracksRef = useRef(preloadedTracks);
  preloadedTracksRef.current = preloadedTracks;
  queueLengthRef.current = queue.length;
  useEffect(() => {
    if (!selectedAlbum) return;
    if (selectedAlbum.key.startsWith("local:album:")) return;
    if (status.state !== "signedIn" || !status.token) return;
    if (preloadedTracksRef.current[selectedAlbum.key]) return;
    let mounted = true;
    const albumKey = selectedAlbum.key;
    fetch(
      `${API_BASE}/album/${encodeURIComponent(albumKey)}/tracks?token=${encodeURIComponent(status.token)}`
    )
      .then((r) => (r.ok ? r.json() : null))
      .then((data: { tracks: PlexTrack[] } | null) => {
        if (mounted && data?.tracks) setPreloadedTracks(albumKey, data.tracks);
      })
      .catch(() => {});
    return () => { mounted = false; };
  }, [selectedAlbum?.key, status.state, status.token, setPreloadedTracks]);

  const handleSidebarResizeStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    resizeRef.current = { type: "sidebar", startX: e.clientX, startW: sidebarWidth };
    const onMove = (ev: MouseEvent) => {
      if (!resizeRef.current || resizeRef.current.type !== "sidebar") return;
      const dx = ev.clientX - resizeRef.current.startX;
      setSidebarWidthState(() =>
        Math.min(SIDEBAR_WIDTH_MAX, Math.max(SIDEBAR_WIDTH_MIN, resizeRef.current!.startW + dx))
      );
    };
    const onUp = () => {
      resizeRef.current = null;
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, [sidebarWidth]);

  const currentAlbum = queue[currentIndex]?.album ?? null;
  const playerThumbUrl = getAlbumThumbUrl(currentAlbum?.thumb, status.state === "signedIn" ? status.token : undefined);

  // When "theme from album art" is on, set theme to dominant color when the *track* changes (not on initial load, so saved theme persists after refresh)
  useEffect(() => {
    if (!themeFromAlbumArt) {
      lastAlbumThemeThumbRef.current = null;
      return;
    }
    if (!playerThumbUrl) return;
    if (lastAlbumThemeThumbRef.current === playerThumbUrl) return;
    const isInitialLoad = lastAlbumThemeThumbRef.current === null;
    lastAlbumThemeThumbRef.current = playerThumbUrl;
    if (isInitialLoad) return;
    getDominantColorFromImageUrl(playerThumbUrl).then((hex) => {
      if (hex) setThemeSeed(hex);
    });
  }, [themeFromAlbumArt, playerThumbUrl, currentIndex, setThemeSeed]);

  const contextMenuItemsAlbum = contextMenu?.type === "album" ? [
    { label: "Add to queue", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2}><path d="M8 6h13M8 12h13M8 18h13M3 6v.01M3 12v.01M3 18v.01" /></svg>, onClick: () => addAlbumToQueue(contextMenu.album) },
    { label: "Shuffle play", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2}><path d="M16 3h5v5M4 20L21 3M21 16v5h-5M15 15l6 6M4 4l5 5" /></svg>, onClick: () => shufflePlayAlbum(contextMenu.album) },
    { label: "Add to playlist", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2}><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z" /><path d="M14 2v6h6M12 18v-6M9 15h6" /></svg>, onClick: () => { setContextMenu(null); setAddToPlaylistContext({ album: contextMenu.album, track: null }); } },
  ] : contextMenu?.type === "track" ? [
    { label: "Add to queue", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2}><path d="M8 6h13M8 12h13M8 18h13M3 6v.01M3 12v.01M3 18v.01" /></svg>, onClick: () => addTrackToQueue(contextMenu.track, contextMenu.album) },
    { label: "Shuffle play", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2}><path d="M16 3h5v5M4 20L21 3M21 16v5h-5M15 15l6 6M4 4l5 5" /></svg>, onClick: () => shufflePlayTrack(contextMenu.trackList, contextMenu.trackIndex, contextMenu.album) },
    { label: "Add to playlist", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2}><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z" /><path d="M14 2v6h6M12 18v-6M9 15h6" /></svg>, onClick: () => { setContextMenu(null); setAddToPlaylistContext({ track: contextMenu.track, album: contextMenu.album }); } },
  ] : [];

  return (
    <div
                  style={{
                    width: "100%",
        height: "100vh",
        boxSizing: "border-box",
        display: "flex",
        flexDirection: "column",
        overflow: "hidden",
        fontFamily: appFontFamily,
        background:
          "var(--app-bg)",
        color: "var(--app-text)",
      }}
    >
      {contextMenu && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          items={contextMenuItemsAlbum}
          onClose={() => setContextMenu(null)}
        />
      )}
      {addToPlaylistContext && (
        <div style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.6)", display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1000 }} onClick={() => { setAddToPlaylistContext(null); setNewPlaylistName(""); }}>
          <div style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, maxWidth: 360, maxHeight: "80vh", overflow: "auto", border: "1px solid var(--app-border)" }} onClick={(e) => e.stopPropagation()}>
            <h3 style={{ margin: "0 0 12px", color: "var(--app-text)" }}>Add to playlist</h3>
            {playlists.length === 0 ? (
              <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
                <p style={{ fontSize: 13, color: "var(--app-muted)", margin: 0 }}>Create a new playlist and add this.</p>
                <input
                  type="text"
                  placeholder="Playlist name"
                  value={newPlaylistName}
                  onChange={(e) => setNewPlaylistName(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).nextElementSibling?.querySelector("button")?.click(); }}
                  style={{ padding: "8px 12px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 13 }}
                  autoFocus
                />
                <div style={{ display: "flex", gap: 8 }}>
                  <button
                    type="button"
                    onClick={async () => {
                      const name = newPlaylistName.trim() || "New Playlist";
                      const p = await createPlaylist(name);
                      if (p && addToPlaylistContext) {
                        const { track, album } = addToPlaylistContext;
                        if (track) await addToPlaylist(p.id, track, album);
                        else if (album) {
                          const tracks = await ensureAlbumTracks(album);
                          for (const t of tracks) await addToPlaylist(p.id, t, album);
                        }
                      }
                      setAddToPlaylistContext(null);
                      setNewPlaylistName("");
                    }}
                    style={{ padding: "8px 14px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-accent)", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}
                  >
                    Create and add
                  </button>
                  <button type="button" onClick={() => { setAddToPlaylistContext(null); setNewPlaylistName(""); }} style={{ padding: "8px 14px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-surface)", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}>Cancel</button>
                </div>
              </div>
            ) : (
              <>
                <div style={{ display: "flex", flexDirection: "column", gap: 8, marginBottom: 12 }}>
                  <p style={{ fontSize: 12, color: "var(--app-muted)", margin: 0 }}>Create new</p>
                  <input
                    type="text"
                    placeholder="Playlist name"
                    value={newPlaylistName}
                    onChange={(e) => setNewPlaylistName(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).nextElementSibling?.querySelector("button")?.click(); }}
                    style={{ padding: "8px 12px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 13 }}
                  />
                  <button
                    type="button"
                    onClick={async () => {
                      const name = newPlaylistName.trim() || "New Playlist";
                      const p = await createPlaylist(name);
                      if (p && addToPlaylistContext) {
                        const { track, album } = addToPlaylistContext;
                        if (track) await addToPlaylist(p.id, track, album);
                        else if (album) {
                          const tracks = await ensureAlbumTracks(album);
                          for (const t of tracks) await addToPlaylist(p.id, t, album);
                        }
                      }
                      setAddToPlaylistContext(null);
                      setNewPlaylistName("");
                    }}
                    style={{ alignSelf: "flex-start", padding: "8px 14px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-accent)", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}
                  >
                    Create and add
                  </button>
                </div>
                <p style={{ fontSize: 12, color: "var(--app-muted)", margin: "0 0 6px" }}>Or add to existing</p>
                <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 6 }}>
                  {playlists.map((p) => (
                    <li key={p.id}>
                      <button
                        type="button"
                        onClick={async () => {
                          const { track, album } = addToPlaylistContext;
                          if (track) {
                            await addToPlaylist(p.id, track, album);
                          } else if (album) {
                            const tracks = await ensureAlbumTracks(album);
                            for (const t of tracks) await addToPlaylist(p.id, t, album);
                          }
                          setAddToPlaylistContext(null);
                        }}
                        style={{ width: "100%", textAlign: "left", padding: "8px 12px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}
                      >
                        {p.name}
                      </button>
                    </li>
                  ))}
                </ul>
                <button type="button" onClick={() => { setAddToPlaylistContext(null); setNewPlaylistName(""); }} style={{ marginTop: 12, padding: "8px 14px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-surface)", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}>Cancel</button>
              </>
            )}
          </div>
        </div>
      )}
      {/* No buffer/RAM limit: main + preload hold current + next + hover-preload in full (browser-managed). */}
      <audio
        ref={(el) => {
          (audioRef as React.MutableRefObject<HTMLAudioElement | null>).current = el;
        }}
        preload="auto"
      />
      <audio ref={preloadAudioRef} preload="auto" style={{ position: "absolute", opacity: 0, pointerEvents: "none", width: 0, height: 0 }} aria-hidden />
      <audio ref={directPlexAudioRef} preload="auto" style={{ position: "absolute", opacity: 0, pointerEvents: "none", width: 0, height: 0 }} aria-hidden />
      <div style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "row", overflow: "hidden", fontFamily: appFontFamily }}>
      {/* Sidebar - full height, not covered by player; can be collapsed from Now Playing */}
      <aside
        style={{
          width: sidebarCollapsed ? 0 : sidebarWidth,
          minWidth: sidebarCollapsed ? 0 : sidebarWidth,
          height: "100vh",
          flexShrink: 0,
          padding: sidebarCollapsed ? 0 : 20,
          borderRight: sidebarCollapsed ? "none" : "1px solid var(--app-border)",
          background: "var(--app-surface)",
          display: "flex",
          flexDirection: "column",
          gap: 18,
          overflow: "hidden",
          boxSizing: "border-box",
          position: "relative",
          fontFamily: "inherit",
          transition: "width 0.2s ease, min-width 0.2s ease, padding 0.2s ease",
        }}
      >
        <button
          type="button"
          onClick={() => setView("library")}
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            gap: 10,
            flexShrink: 0,
            width: "100%",
            border: "none",
            background: "transparent",
            color: "inherit",
            cursor: "pointer",
            padding: "4px 0",
          }}
          title="Go to Home"
        >
          <img src="/sonic-title.png" alt="Sonic" style={{ height: 48, width: "auto", display: "block", flexShrink: 0, objectFit: "contain" }} />
        </button>

        <nav
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 6,
            marginTop: 8,
            flexShrink: 0,
          }}
          aria-label="Main navigation"
        >
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr 1fr", gap: 6 }}>
            {[
              { id: "library", title: "Home", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d="M3 9l9-7 9 7v11a2 2 0 01-2 2H5a2 2 0 01-2-2z" /><polyline points="9 22 9 12 15 12 15 22" /></svg> },
              { id: "queue", title: "Queue", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden><line x1="8" y1="6" x2="21" y2="6" /><line x1="8" y1="12" x2="21" y2="12" /><line x1="8" y1="18" x2="21" y2="18" /><line x1="3" y1="6" x2="3.01" y2="6" /><line x1="3" y1="12" x2="3.01" y2="12" /><line x1="3" y1="18" x2="3.01" y2="18" /></svg> },
              { id: "playlists", title: "Playlists", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden><line x1="8" y1="6" x2="15" y2="6" /><line x1="8" y1="12" x2="15" y2="12" /><line x1="8" y1="18" x2="15" y2="18" /><line x1="18" y1="12" x2="21" y2="12" /><line x1="19.5" y1="10.5" x2="19.5" y2="13.5" /></svg> },
              { id: "settings", title: "Settings", icon: <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden><circle cx={12} cy={12} r={3} /><path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 010 2.83 2 2 0 01-2.83 0l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-2 2 2 2 0 01-2-2v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83 0 2 2 0 010-2.83l.06-.06a1.65 1.65 0 00.33-1.82 1.65 1.65 0 00-1.51-1H3a2 2 0 01-2-2 2 2 0 012-2h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 010-2.83 2 2 0 012.83 0l.06.06a1.65 1.65 0 001.82.33H9a1.65 1.65 0 001-1.51V3a2 2 0 012-2 2 2 0 012 2v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 0 2 2 0 010 2.83l-.06.06a1.65 1.65 0 00-.33 1.82V9a1.65 1.65 0 001.51 1H21a2 2 0 012 2 2 2 0 01-2 2h-.09a1.65 1.65 0 00-1.51 1z" /></svg> },
            ].map((item) => {
              const isActive = view === (item.id as MainView);
              return (
                <button
                  key={item.id}
                  type="button"
                  onClick={() => setView(item.id as MainView)}
                  title={item.title}
                  aria-label={item.title}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    aspectRatio: "1",
                    padding: 0,
                    borderRadius: 10,
                    border: "none",
                    backgroundColor: isActive ? "var(--app-accent)" : "transparent",
                    color: isActive ? "var(--app-text)" : "var(--app-muted)",
                    cursor: "pointer",
                  }}
                >
                  {item.icon}
                </button>
              );
            })}
          </div>
        </nav>

        {((status.state === "signedIn" && status.token) || localTracks.length > 0) && (
          <div
            style={{
              marginTop: "auto",
              flex: 1,
              minHeight: 0,
              display: "flex",
              flexDirection: "column",
              overflow: "hidden",
            }}
          >
            {selectedAlbum ? (
              <AlbumDetailSidebar
                album={selectedAlbum}
                token={status.state === "signedIn" ? status.token : ""}
                preloadedTracks={preloadedTracksWithLocal}
                onTracksLoaded={setPreloadedTracks}
                onPlayTrack={(list, idx) => playTrackAt(list, idx, selectedAlbum)}
                onTrackContextMenu={(track, alb, trackList, trackIndex, e) => setContextMenu({ x: e.clientX, y: e.clientY, type: "track", track, album: alb, trackList, trackIndex })}
                onClose={() => setSelectedAlbum(null)}
                playingTrackKey={queue[currentIndex]?.track.key ?? null}
                parentFetchesTracks
                onTrackHover={startHoverPreload}
                onTrackHoverEnd={cancelHoverPreload}
              />
            ) : (
              <MiniQueueSidebar
                queue={queue}
                currentIndex={currentIndex}
                token={status.state === "signedIn" ? status.token : ""}
                onPlayQueueIndex={playQueueIndex}
                onTrackContextMenu={(track, alb, trackList, trackIndex, e) => setContextMenu({ x: e.clientX, y: e.clientY, type: "track", track, album: alb, trackList, trackIndex })}
                onRemoveFromQueue={removeFromQueue}
              />
            )}
          </div>
        )}
        {!layoutLocked && (
          <div
            role="separator"
            aria-label="Resize sidebar"
            onMouseDown={handleSidebarResizeStart}
            style={{
              position: "absolute",
              right: 0,
              top: 0,
              bottom: 0,
              width: 8,
              cursor: "col-resize",
              zIndex: 2,
              background: "var(--app-border)",
              borderRadius: "4px 0 0 4px",
            }}
          />
        )}
      </aside>

      {/* Main column: content + player bar (player only here, not over sidebar) */}
      <div style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column", overflow: "hidden", fontFamily: "inherit" }}>
      <main
        className={view !== "nowPlaying" ? "hide-scrollbar" : undefined}
        style={{
          flex: 1,
          minHeight: 0,
          padding: 24,
          paddingBottom: 16,
          display: "flex",
          flexDirection: "column",
          gap: 16,
          overflowY: view === "nowPlaying" ? "hidden" : "auto",
          boxSizing: "border-box",
          fontFamily: "inherit",
        }}
      >
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            gap: 12,
          }}
        >
        </div>

        {view === "settings" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 24, width: "100%", maxWidth: 920 }}>
            <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
              {(["playback", "appearance", "library", "tracking"] as const).map((tab) => (
                <button
                  key={tab}
                  type="button"
                  onClick={() => setSettingsTab(tab)}
                  style={{
                    padding: "10px 18px",
                    borderRadius: 10,
                    border: "1px solid var(--app-border)",
                    background: settingsTab === tab ? "var(--app-accent)" : "var(--app-surface)",
                    color: settingsTab === tab ? "var(--app-text)" : "var(--app-muted)",
                    fontSize: 14,
                    fontWeight: 500,
                    cursor: "pointer",
                    textTransform: "capitalize",
                  }}
                >
                  {tab === "playback" ? "Playback" : tab === "appearance" ? "Appearance" : tab === "library" ? "Library" : "Tracking"}
                </button>
              ))}
            </div>
            {settingsTab === "playback" && (
            <>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Parametric EQ &amp; normalization</h2>
              <label style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 16, cursor: "pointer" }}>
                <input
                  type="checkbox"
                  checked={loudnessNormalizer}
                  onChange={(e) => {
                    const v = e.target.checked;
                    setLoudnessNormalizer(v);
                    window.localStorage.setItem("loudnessNormalizer", v ? "true" : "false");
                  }}
                  style={{ width: 18, height: 18, accentColor: "var(--app-accent)" }}
                />
                <span style={{ fontSize: 13, color: "var(--app-text)" }}>Loudness normalizer</span>
              </label>
              <p style={{ fontSize: 12, color: "var(--app-muted)", marginTop: -8, marginBottom: 16, marginLeft: 28 }}>
                Reduces volume differences between tracks so quiet songs and loud songs play at a more even level.
              </p>
              <p style={{ fontSize: 13, color: "var(--app-muted)", marginBottom: 16 }}>
                Preset or adjust each band (dB). Applied to playback.
              </p>
              <EqCurve gains={eqGains} width={860} height={96} />
              <label style={{ display: "flex", alignItems: "center", gap: 12, marginBottom: 12 }}>
                <span style={{ minWidth: 72, fontSize: 13, color: "var(--app-muted)" }}>Preamp</span>
                <input
                  type="range"
                  className="player-range eq-range"
                  min={PREAMP_MIN_DB}
                  max={PREAMP_MAX_DB}
                  step={1}
                  value={eqPreamp}
                  onChange={(e) => setEqPreamp(Number(e.target.value))}
                  style={{ width: 120, height: 20 }}
                  aria-label="Preamp gain"
                />
                <span style={{ fontSize: 12, color: "var(--app-muted)", fontVariantNumeric: "tabular-nums", minWidth: 32 }}>
                  {eqPreamp > 0 ? "+" : ""}{eqPreamp} dB
                </span>
              </label>
              <label style={{ display: "flex", alignItems: "center", gap: 12, marginBottom: 16 }}>
                <span style={{ minWidth: 80, fontSize: 13, color: "var(--app-muted)" }}>Preset</span>
                <select
                  value={eqPreset}
                  onChange={(e) => setEqPreset(e.target.value)}
                  style={{
                    padding: "8px 12px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: "var(--app-bg)",
                    color: "var(--app-text)",
                    fontSize: 13,
                    cursor: "pointer",
                  }}
                >
                  {EQ_PRESETS.map((p) => (
                    <option key={p.id} value={p.id}>{p.label}</option>
                  ))}
                  <option value="custom">Custom</option>
                </select>
              </label>
              <div style={{ display: "flex", alignItems: "flex-end", gap: 12, justifyContent: "center", flexWrap: "wrap" }}>
                {EQ_BANDS.map((band, i) => (
                  <div key={band.freq} style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 8 }}>
                    <span style={{ fontSize: 10, color: "var(--app-muted)", fontVariantNumeric: "tabular-nums" }}>{eqGains[i] !== undefined && eqGains[i] !== 0 ? (eqGains[i] > 0 ? "+" : "") + eqGains[i] : "0"}</span>
                    <div style={{ height: 100, width: 28, display: "flex", alignItems: "center", justifyContent: "center", position: "relative" }}>
                      <input
                        type="range"
                        className="player-range eq-range"
                        min={EQ_MIN_DB}
                        max={EQ_MAX_DB}
                        step={1}
                        value={eqGains[i] ?? 0}
                        onChange={(e) => setEqGain(i, Number(e.target.value))}
                        style={{
                          position: "absolute",
                          width: 100,
                          height: 20,
                          margin: 0,
                          cursor: "pointer",
                          transform: "rotate(-90deg)",
                          transformOrigin: "center",
                          left: "50%",
                          top: "50%",
                          marginLeft: -50,
                          marginTop: -10,
                        }}
                        aria-label={`EQ ${band.label} Hz`}
                />
              </div>
                    <span style={{ fontSize: 11, color: "var(--app-muted)", fontVariantNumeric: "tabular-nums" }}>{band.label}</span>
                  </div>
                ))}
              </div>
            </section>
            </>
            )}
            {settingsTab === "appearance" && (
            <>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Layout</h2>
              <label style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
                <span style={{ minWidth: 90, fontSize: 13, color: "var(--app-muted)" }}>Sidebar &amp; panels</span>
                <button
                  type="button"
                  onClick={() => setLayoutLocked((v) => !v)}
                  title={layoutLocked ? "Unlock layout to resize sidebar and album panel" : "Lock layout"}
                  style={{
                    padding: "8px 12px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: layoutLocked ? "var(--app-surface)" : "var(--app-accent-dim)",
                    color: "var(--app-text)",
                    fontSize: 13,
                    cursor: "pointer",
                  }}
                >
                  {layoutLocked ? "🔒 Locked" : "🔓 Unlocked"}
                </button>
                <span style={{ fontSize: 12, color: "var(--app-muted)" }}>{layoutLocked ? "Resizing disabled" : "Drag edges to resize"}</span>
              </label>
            </section>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Theme</h2>
              <p style={{ fontSize: 13, color: "var(--app-muted)", marginBottom: 16 }}>
                Pick a color for backgrounds and accents. Text stays the same for readability.
              </p>
              <label style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
                <span style={{ minWidth: 90, fontSize: 13, color: "var(--app-muted)" }}>Theme color</span>
                <input
                  type="color"
                  value={themeSeed}
                  onChange={(e) => setThemeSeed(e.target.value)}
                  style={{
                    width: 44,
                    height: 32,
                    padding: 2,
                    border: "1px solid var(--app-border)",
                    borderRadius: 8,
                    background: "var(--app-bg)",
                    cursor: "pointer",
                  }}
                  title="Color for backgrounds and accents"
                />
                <input
                  type="text"
                  value={themeHexInput}
                  onChange={(e) => setThemeHexInput(e.target.value)}
                  onBlur={commitThemeHexInput}
                  onKeyDown={(e) => e.key === "Enter" && commitThemeHexInput()}
                  placeholder="#0f172a"
                  style={{
                    width: 88,
                    padding: "6px 10px",
                    border: "1px solid var(--app-border)",
                    borderRadius: 8,
                    background: "var(--app-bg)",
                    color: "var(--app-text)",
                    fontSize: 12,
                    fontFamily: "monospace",
                  }}
                  title="Type or paste hex (e.g. 0f172a or #0f172a)"
                />
              </label>
              <label style={{ display: "flex", alignItems: "center", gap: 10, marginTop: 16, cursor: "pointer" }}>
                <input
                  type="checkbox"
                  checked={themeFromAlbumArt}
                  onChange={(e) => setThemeFromAlbumArt(e.target.checked)}
                  style={{ width: 18, height: 18, accentColor: "var(--app-accent)" }}
                />
                <span style={{ fontSize: 13, color: "var(--app-text)" }}>Use album art color for theme when playing</span>
              </label>
              <p style={{ fontSize: 12, color: "var(--app-muted)", marginTop: 6, marginLeft: 28 }}>
                When checked, the theme color updates to the most prominent color from the now-playing album art.
              </p>
              <label style={{ display: "flex", alignItems: "center", gap: 12, marginTop: 16, flexWrap: "wrap" }}>
                <span style={{ minWidth: 90, fontSize: 13, color: "var(--app-muted)" }}>Text</span>
                <select
                  value={themeTextMode}
                  onChange={(e) => setThemeTextMode(e.target.value as ThemeTextMode)}
                  style={{
                    padding: "6px 10px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: "var(--app-bg)",
                    color: "var(--app-text)",
                    fontSize: 13,
                    cursor: "pointer",
                  }}
                  title="Auto picks light or dark text based on theme color"
                >
                  <option value="auto">Auto (from theme color)</option>
                  <option value="light">Light text</option>
                  <option value="dark">Dark text</option>
                </select>
              </label>
              <p style={{ fontSize: 12, color: "var(--app-muted)", marginTop: 6, marginLeft: 102 }}>
                Auto uses light text on dark themes and dark text on light themes.
              </p>
            </section>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Font</h2>
              <label style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
                <span style={{ minWidth: 100, fontSize: 13, color: "var(--app-muted)" }}>App font</span>
                <select
                  value={appFont}
                  onChange={(e) => {
                    const v = e.target.value;
                    setAppFontState(v);
                    if (v) window.localStorage.setItem("appFont", v); else window.localStorage.removeItem("appFont");
                  }}
                  style={{
                    padding: "8px 12px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: "var(--app-bg)",
                    color: "var(--app-text)",
                    fontSize: 13,
                    cursor: "pointer",
                  }}
                >
                  {APP_FONT_OPTIONS.map((opt) => (
                    <option key={opt.value || "default"} value={opt.value}>{opt.label}</option>
                  ))}
                </select>
              </label>
            </section>
            </>
            )}
            {settingsTab === "library" && (
            <>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Local files</h2>
              <p style={{ fontSize: 13, color: "var(--app-muted)", marginBottom: 16 }}>
                Add music from a folder on your computer. Sonic creates <strong>SonicMusic</strong> in your Music folder; put MP3, M4A, FLAC, OGG, WAV there, then refresh to load them into your library.
              </p>
              {!localFilesEnabled ? (
                <button
                  type="button"
                  onClick={() => {
                    fetch(`${API_SERVER}/api/local-files/enable`, { method: "POST" })
                      .then((r) => (r.ok ? r.json() : null))
                      .then((data: { path?: string } | null) => {
                        if (data?.path) {
                          setLocalFilesEnabled(true);
                          setLocalMusicPath(data.path);
                        }
                      })
                      .catch(() => {});
                  }}
                  style={{
                    padding: "8px 16px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: "var(--app-accent)",
                    color: "var(--app-text)",
                    fontSize: 13,
                    cursor: "pointer",
                  }}
                >
                  Enable local files
                </button>
              ) : (
                <>
                  <p style={{ fontSize: 12, color: "var(--app-muted)", marginBottom: 8, fontFamily: "monospace" }}>{localMusicPath ?? ""}</p>
                  <button
                    type="button"
                    disabled={localFilesScanning}
                    onClick={() => {
                      setLocalFilesScanning(true);
                      fetch(`${API_SERVER}/api/local-files/scan`)
                        .then((r) => (r.ok ? r.json() : null))
                        .then((data: { tracks?: PlexTrack[]; scanId?: number } | null) => {
                          if (data && Array.isArray(data.tracks)) {
                            setLocalTracks(data.tracks);
                            if (data.scanId != null) localArtScanId = data.scanId;
                            else localArtScanId = Date.now();
                          }
                        })
                        .catch(() => {})
                        .finally(() => setLocalFilesScanning(false));
                    }}
                    style={{
                      padding: "8px 16px",
                      borderRadius: 8,
                      border: "1px solid var(--app-border)",
                      background: "var(--app-accent)",
                      color: "var(--app-text)",
                      fontSize: 13,
                      cursor: localFilesScanning ? "wait" : "pointer",
                    }}
                  >
                    {localFilesScanning ? "Scanning…" : "Refresh"}
                  </button>
                  {localTracks.length > 0 && (
                    <p style={{ fontSize: 12, color: "var(--app-muted)", marginTop: 12 }}>{localTracks.length} track{localTracks.length !== 1 ? "s" : ""} loaded. In Music, use the filter to show &quot;Local files&quot; or &quot;All music&quot;.</p>
                  )}
                </>
              )}
            </section>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Plex account</h2>
              <PlexAuthCard status={status} setStatus={setStatus} />
            </section>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Streaming quality</h2>
              <p style={{ fontSize: 13, color: "var(--app-muted)", marginBottom: 16 }}>
                The app uses your local network when possible. Choose quality for each case.
              </p>
              <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
                <label style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
                  <span style={{ minWidth: 140, fontSize: 13, color: "var(--app-muted)" }}>On local network</span>
                  <select
                    value={streamingQualityLocal}
                    onChange={(e) => setStreamingQualityLocalPersist(e.target.value as StreamingQuality)}
                    style={{
                      padding: "8px 12px",
                      borderRadius: 8,
                      border: "1px solid var(--app-border)",
                      background: "var(--app-bg)",
                      color: "var(--app-text)",
                      fontSize: 13,
                      cursor: "pointer",
                    }}
                  >
                    {STREAMING_QUALITY_OPTIONS.map((opt) => (
                      <option key={opt.value} value={opt.value}>{opt.label}</option>
                    ))}
                  </select>
                </label>
                <label style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
                  <span style={{ minWidth: 140, fontSize: 13, color: "var(--app-muted)" }}>On other networks</span>
                  <select
                    value={streamingQualityRemote}
                    onChange={(e) => setStreamingQualityRemotePersist(e.target.value as StreamingQuality)}
                    style={{
                      padding: "8px 12px",
                      borderRadius: 8,
                      border: "1px solid var(--app-border)",
                      background: "var(--app-bg)",
                      color: "var(--app-text)",
                      fontSize: 13,
                      cursor: "pointer",
                    }}
                  >
                    {STREAMING_QUALITY_OPTIONS.map((opt) => (
                      <option key={opt.value} value={opt.value}>{opt.label}</option>
                    ))}
                  </select>
                </label>
              </div>
              {status.state === "signedIn" && status.token && (
                <div style={{ fontSize: 12, color: "var(--app-muted)", marginTop: 12, display: "flex", flexDirection: "column", gap: 6 }}>
                  <p style={{ margin: 0, display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                    <span>
                      <strong style={{ color: "var(--app-text)" }}>Streaming:</strong>{" "}
                      {plexServerUri ? "Direct (same network)" : "Proxy"}
                      {plexServerUri && (
                        <span style={{ marginLeft: 6, opacity: 0.9 }} title={plexServerUri}>
                          — {plexServerUri.replace(/^https?:\/\//, "").replace(/\/$/, "")}
                        </span>
                      )}
                    </span>
                  </p>
                  {isLocalConnection !== null && (
                    <p style={{ margin: 0 }}>Connection: {isLocalConnection ? "Local network" : "Other network"}</p>
                  )}
                  <p style={{ margin: 0, display: "flex", alignItems: "center", gap: 8 }}>
                    <button
                      type="button"
                      disabled={connectionRefreshLoading}
                      onClick={async () => {
                        if (connectionRefreshLoading || !status.token) return;
                        setConnectionRefreshLoading(true);
                        try {
                          await fetch(`${API_SERVER}/api/cache/reset`, { method: "POST" });
                          const [connRes, uriRes] = await Promise.all([
                            fetch(`${API_BASE}/connection?token=${encodeURIComponent(status.token)}`),
                            fetch(`${API_BASE}/server-uri?token=${encodeURIComponent(status.token)}`),
                          ]);
                          const connData = connRes.ok ? await connRes.json() : null;
                          const uriData = uriRes.ok ? await uriRes.json() : null;
                          if (connData && typeof connData.local === "boolean") setIsLocalConnection(connData.local);
                          if (uriData && typeof uriData.serverUri === "string") setPlexServerUri(uriData.serverUri);
                          else setPlexServerUri(null);
                        } finally {
                          setConnectionRefreshLoading(false);
                        }
                      }}
                      style={{
                        padding: "4px 10px",
                        fontSize: 12,
                        borderRadius: 6,
                        border: "1px solid var(--app-border)",
                        background: "var(--app-bg)",
                        color: "var(--app-text)",
                        cursor: connectionRefreshLoading ? "wait" : "pointer",
                        alignSelf: "flex-start",
                      }}
                    >
                      {connectionRefreshLoading ? "Checking…" : "Refresh connection & streaming"}
                    </button>
                  </p>
                </div>
              )}
            </section>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Cache</h2>
              <p style={{ fontSize: 13, color: "var(--app-muted)", marginBottom: 16 }}>
                Clear API and image caches. Use if library or artwork is stale.
              </p>
              <button
                type="button"
                onClick={async () => {
                  try {
                    const base = (import.meta.env.VITE_API_BASE || "http://localhost:4000").replace(/\/api\/plex\/?$/, "");
                    const res = await fetch(`${base}/api/cache/reset`, { method: "POST" });
                    if (res.ok) setCacheResetDone(true);
                  } catch {
                    setCacheResetDone(false);
                  }
                }}
                style={{
                  padding: "8px 16px",
                  borderRadius: 8,
                  border: "1px solid var(--app-border)",
                  background: "var(--app-accent)",
                  color: "var(--app-text)",
                  fontSize: 13,
                  cursor: "pointer",
                }}
              >
                Reset cache
              </button>
              {cacheResetDone !== null && (
                <span style={{ marginLeft: 12, fontSize: 13, color: "var(--app-muted)" }}>
                  {cacheResetDone ? "Cache cleared." : "Failed (is the server running?)."}
                </span>
              )}
            </section>
            </>
            )}
            {settingsTab === "tracking" && (
            <>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Discord Rich Presence</h2>
              <p style={{ fontSize: 13, color: "var(--app-muted)", marginBottom: 16 }}>
                Show what you&apos;re listening to on Discord. Create an application at{" "}
                <a href="https://discord.com/developers/applications" target="_blank" rel="noopener noreferrer" style={{ color: "var(--app-accent)" }}>discord.com/developers</a>, then copy its <strong>Application ID</strong> (the long number — <em>not</em> the Client Secret). The Discord desktop app must be running.
              </p>
              {discordRpcStatus && (
                <p style={{ fontSize: 13, marginBottom: 12, color: discordRpcStatus.ok ? "var(--app-muted)" : "#e57373" }}>
                  {discordRpcStatus.ok ? "✓ " + discordRpcStatus.message : "✗ " + discordRpcStatus.message}
                </p>
              )}
              <label style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap", marginBottom: 12 }}>
                <span style={{ minWidth: 120, fontSize: 13, color: "var(--app-muted)" }}>Application ID</span>
                <input
                  type="text"
                  value={discordClientId}
                  onChange={(e) => {
                    const v = e.target.value;
                    setDiscordClientIdState(v);
                    setDiscordRpcStatus(null);
                    if (v.trim()) window.localStorage.setItem("discordClientId", v.trim());
                    else window.localStorage.removeItem("discordClientId");
                  }}
                  placeholder="e.g. 1234567890123456789"
                  style={{
                    width: 220,
                    padding: "8px 12px",
                    border: "1px solid var(--app-border)",
                    borderRadius: 8,
                    background: "var(--app-bg)",
                    color: "var(--app-text)",
                    fontSize: 13,
                    fontFamily: "monospace",
                  }}
                />
                <button
                  type="button"
                  onClick={async () => {
                    const id = (window.localStorage.getItem("discordClientId") || "").trim();
                    if (!id) {
                      setDiscordRpcStatus({ ok: false, message: "Enter your Application ID first." });
                      return;
                    }
                    setDiscordRpcStatus(null);
                    try {
                      const base = (import.meta.env.VITE_API_BASE || "http://localhost:4000").replace(/\/api\/plex\/?$/, "") || "http://localhost:4000";
                      const res = await fetch(`${base}/api/discord/test`, {
                        method: "POST",
                        headers: { "Content-Type": "application/json" },
                        body: JSON.stringify({ clientId: id }),
                      });
                      const data = await res.json().catch(() => ({}));
                      if (data.ok) {
                        setDiscordRpcStatus({ ok: true, message: "Connected. Check Discord — you should see \"Test track\"." });
                      } else {
                        setDiscordRpcStatus({ ok: false, message: data.error || "Request failed." });
                      }
                    } catch (e) {
                      setDiscordRpcStatus({ ok: false, message: "Network error. Is Sonic running?" });
                    }
                  }}
                  style={{
                    padding: "8px 14px",
                    borderRadius: 8,
                    border: "1px solid var(--app-border)",
                    background: "var(--app-accent)",
                    color: "var(--app-text)",
                    fontSize: 13,
                    cursor: "pointer",
                  }}
                >
                  Test connection
                </button>
              </label>
              <label style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
                <span style={{ minWidth: 120, fontSize: 13, color: "var(--app-muted)" }}>Image key (optional)</span>
                <input
                  type="text"
                  value={discordImageKey}
                  onChange={(e) => {
                    const v = e.target.value;
                    setDiscordImageKeyState(v);
                    if (v.trim()) window.localStorage.setItem("discordImageKey", v.trim());
                    else window.localStorage.removeItem("discordImageKey");
                  }}
                  placeholder="Art asset key from your app"
                  style={{
                    width: 220,
                    padding: "8px 12px",
                    border: "1px solid var(--app-border)",
                    borderRadius: 8,
                    background: "var(--app-bg)",
                    color: "var(--app-text)",
                    fontSize: 13,
                  }}
                />
              </label>
              <p style={{ fontSize: 12, color: "var(--app-muted)", marginTop: 8 }}>
                Hover text on the image will show the album name. To use a custom image, add an Art Asset in your Discord app&apos;s Rich Presence settings and enter its key here.
              </p>
            </section>
            <section style={{ background: "var(--app-surface)", borderRadius: 12, padding: 20, border: "1px solid var(--app-border)" }}>
              <h2 style={{ fontSize: 16, margin: "0 0 16px", color: "var(--app-text)" }}>Last.fm Scrobbling</h2>
              <p style={{ fontSize: 13, fontWeight: 600, marginBottom: 4, color: "var(--app-text)" }}>
                Status: {lastFmConnected
                  ? (lastFmUsername ? `Connected as ${lastFmUsername}` : "Connected")
                  : "Not connected"}
                {lastFmTestResult !== null && (
                  lastFmTestResult.ok
                    ? " — ✓ Working"
                    : " — ✗ Not working"
                )}
              </p>
              {lastFmTestResult !== null && !lastFmTestResult.ok && (
                <p style={{ fontSize: 12, color: "#e57373", margin: "0 0 12px" }}>{lastFmTestResult.error}</p>
              )}
              {lastFmError && (
                <p style={{ fontSize: 12, color: "#e57373", margin: "0 0 12px" }}>Scrobbling: {lastFmError}</p>
              )}
              <p style={{ fontSize: 13, color: "var(--app-muted)", marginBottom: 16 }}>
                Scrobble tracks to your Last.fm account. Enter your API key and secret from <a href="https://www.last.fm/api/account/create" target="_blank" rel="noopener noreferrer" style={{ color: "var(--app-accent)" }}>last.fm/api</a>, then press Connect.
              </p>
              <div style={{ display: "flex", flexDirection: "column", gap: 12, marginBottom: 16 }}>
                <label style={{ fontSize: 13, color: "var(--app-text)" }}>
                  API Key
                  <input
                    type="text"
                    value={lastFmApiKey}
                    onChange={(e) => {
                      const v = e.target.value;
                      setLastFmApiKey(v);
                      if (v) window.localStorage.setItem("lastFmApiKey", v); else window.localStorage.removeItem("lastFmApiKey");
                    }}
                    placeholder="Your Last.fm API key"
                    style={{ display: "block", marginTop: 4, width: "100%", maxWidth: 320, padding: "8px 10px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 13 }}
                  />
                </label>
                <label style={{ fontSize: 13, color: "var(--app-text)" }}>
                  API Secret
                  <input
                    type="password"
                    value={lastFmApiSecret}
                    onChange={(e) => {
                      const v = e.target.value;
                      setLastFmApiSecret(v);
                      if (v) window.localStorage.setItem("lastFmApiSecret", v); else window.localStorage.removeItem("lastFmApiSecret");
                    }}
                    placeholder="Your Last.fm API secret"
                    style={{ display: "block", marginTop: 4, width: "100%", maxWidth: 320, padding: "8px 10px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 13 }}
                  />
                </label>
              </div>
              {lastFmConnected ? (
                <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
                    <span style={{ fontSize: 13, color: "var(--app-text)" }}>
                      Connected{lastFmUsername ? ` as ${lastFmUsername}` : ""}
                    </span>
                    <button
                      type="button"
                      disabled={lastFmTestLoading}
                      onClick={async () => {
                        setLastFmTestResult(null);
                        setLastFmTestLoading(true);
                        try {
                          const sk = window.localStorage.getItem("lastFmSessionKey") || "";
                          const apiKey = window.localStorage.getItem("lastFmApiKey") || "";
                          const secret = window.localStorage.getItem("lastFmApiSecret") || "";
                          const res = await fetch(`${API_SERVER}/api/lastfm/test`, {
                            method: "POST",
                            headers: { "Content-Type": "application/json" },
                            body: JSON.stringify({ sk, apiKey, secret }),
                          });
                          const data = await res.json();
                          if (data.ok === true) {
                            setLastFmTestResult({ ok: true });
                            setLastFmError(null);
                          } else setLastFmTestResult({ ok: false, error: data.error || "Test failed" });
                        } catch (e) {
                          setLastFmTestResult({ ok: false, error: (e as Error).message || "Network error" });
                        } finally {
                          setLastFmTestLoading(false);
                        }
                      }}
                      style={{ padding: "8px 14px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 13, cursor: lastFmTestLoading ? "default" : "pointer", opacity: lastFmTestLoading ? 0.7 : 1 }}
                    >
                      {lastFmTestLoading ? "Testing…" : "Test connection"}
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                      window.localStorage.removeItem("lastFmSessionKey");
                      window.localStorage.removeItem("lastFmUsername");
                      setLastFmConnected(false);
                      setLastFmUsername(null);
                      setLastFmTestResult(null);
                      }}
                      style={{ padding: "8px 14px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-muted)", fontSize: 13, cursor: "pointer" }}
                    >
                      Disconnect
                    </button>
                  </div>
                  {lastFmTestResult !== null && (
                    <p style={{ fontSize: 13, margin: 0, color: lastFmTestResult.ok ? "var(--app-text)" : "#e57373" }}>
                      {lastFmTestResult.ok
                        ? "Connection OK. Now Playing and scrobbles use this account; your profile should update when you play tracks (scrobbles after ~50% or 4 min)."
                        : lastFmTestResult.error}
                    </p>
                  )}
                </div>
              ) : (
                <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
                  <button
                    type="button"
                    disabled={lastFmConnecting || (!lastFmApiKey.trim() || !lastFmApiSecret.trim())}
                    onClick={async () => {
                      const apiKey = lastFmApiKey.trim();
                      const secret = lastFmApiSecret.trim();
                      if (!apiKey || !secret) {
                        alert("Enter your Last.fm API Key and Secret above, then press Connect.");
                        return;
                      }
                      setLastFmConnecting(true);
                      try {
                        const res = await fetch(`${API_SERVER}/api/lastfm/token`, {
                          method: "POST",
                          headers: { "Content-Type": "application/json" },
                          body: JSON.stringify({ apiKey, secret }),
                        });
                        const data = await res.json();
                        if (!res.ok) {
                          alert(data.error || "Failed to connect to Last.fm.");
                          return;
                        }
                        const { token, authUrl } = data;
                        if (!token || !authUrl) return;
                        window.localStorage.setItem("lastfm_token", token);
                        const w = window.open(authUrl, "lastfm_auth", "width=500,height=600");
                        const check = setInterval(() => {
                          if (w?.closed) {
                            clearInterval(check);
                            setLastFmConnected(!!window.localStorage.getItem("lastFmSessionKey"));
                            setLastFmUsername(window.localStorage.getItem("lastFmUsername"));
                          }
                        }, 500);
                      } finally {
                        setLastFmConnecting(false);
                      }
                    }}
                    style={{ padding: "8px 16px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-accent)", color: "var(--app-text)", fontSize: 13, cursor: lastFmConnecting || (!lastFmApiKey.trim() || !lastFmApiSecret.trim()) ? "default" : "pointer", opacity: lastFmConnecting || (!lastFmApiKey.trim() || !lastFmApiSecret.trim()) ? 0.7 : 1 }}
                  >
                    {lastFmConnecting ? "Connecting…" : "Connect to Last.fm"}
                  </button>
                  <p style={{ fontSize: 12, color: "var(--app-muted)", margin: 0 }}>
                    After you allow the app on Last.fm, click below to complete the connection.
                  </p>
                  <button
                    type="button"
                    disabled={lastFmCompleting}
                    onClick={async () => {
                      setLastFmTestResult(null);
                      setLastFmCompleting(true);
                      try {
                        const token = window.localStorage.getItem("lastfm_token");
                        const apiKey = window.localStorage.getItem("lastFmApiKey") || "";
                        const secret = window.localStorage.getItem("lastFmApiSecret") || "";
                        if (token && apiKey && secret) {
                          const res = await fetch(`${API_SERVER}/api/lastfm/session`, {
                            method: "POST",
                            headers: { "Content-Type": "application/json" },
                            body: JSON.stringify({ token, apiKey, secret }),
                          });
                          const data = await res.json();
                          if (data.sk && data.username) {
                            window.localStorage.setItem("lastFmSessionKey", data.sk);
                            window.localStorage.setItem("lastFmUsername", data.username);
                            window.localStorage.removeItem("lastfm_token");
                            setLastFmConnected(true);
                            setLastFmUsername(data.username);
                            setLastFmTestResult({ ok: true });
                            setLastFmError(null);
                          } else {
                            setLastFmTestResult({ ok: false, error: data.error || "Connection failed" });
                          }
                        } else if (window.localStorage.getItem("lastFmSessionKey")) {
                          setLastFmConnected(true);
                          setLastFmUsername(window.localStorage.getItem("lastFmUsername"));
                          setLastFmTestResult({ ok: true });
                          setLastFmError(null);
                        } else {
                          setLastFmTestResult({ ok: false, error: "Click Connect to Last.fm first, sign in in the popup, then click this button again." });
                        }
                      } catch (e) {
                        setLastFmTestResult({ ok: false, error: (e as Error).message || "Network error" });
                      } finally {
                        setLastFmCompleting(false);
                      }
                    }}
                    style={{ padding: "8px 16px", borderRadius: 8, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 13, cursor: lastFmCompleting ? "default" : "pointer", opacity: lastFmCompleting ? 0.7 : 1 }}
                  >
                    {lastFmCompleting ? "Completing…" : "I've finished signing in — complete connection"}
                  </button>
                </div>
              )}
            </section>
            </>
            )}
          </div>
        )}

        {view === "library" && (
          <LibraryView
            status={status}
            selectedAlbum={selectedAlbum}
            onSelectAlbum={handleAlbumClick}
            onPlayAlbum={handleAlbumDoubleClick}
            onAlbumsLoaded={setLibraryAlbums}
            onServerUri={setPlexServerUri}
            onAlbumContextMenu={(album, e) => setContextMenu({ x: e.clientX, y: e.clientY, type: "album", album })}
            preloadedTracks={preloadedTracksWithLocal}
            onPlayTrack={(list, idx, album) => playTrackAt(list, idx, album)}
            onAlbumHover={prefetchAlbumTracks}
            onTrackHover={startHoverPreload}
            onTrackHoverEnd={cancelHoverPreload}
            localTracks={localTracks}
            libraryFilter={libraryFilter}
            onLibraryFilterChange={setLibraryFilter}
            onRefreshLocalFiles={refreshLocalFiles}
            onGoToSettings={() => setView("settings")}
          />
        )}

        {view === "nowPlaying" && (
          <div style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column", position: "relative", overflow: "hidden" }}>
          <button
            type="button"
            onClick={() => setSidebarCollapsed((c) => !c)}
            title={sidebarCollapsed ? "Show sidebar" : "Hide sidebar"}
            aria-label={sidebarCollapsed ? "Show sidebar" : "Hide sidebar"}
            style={{
              position: "absolute",
              left: 12,
              top: 12,
              zIndex: 10,
              width: 32,
              height: 32,
              borderRadius: 8,
              border: "1px solid var(--app-border)",
              background: "var(--app-surface)",
              color: "var(--app-text)",
              cursor: "pointer",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              boxShadow: "0 2px 8px rgba(0,0,0,0.15)",
            }}
          >
            <svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" style={{ transform: sidebarCollapsed ? "rotate(180deg)" : "none" }}>
              <path d="M15 18l-6-6 6-6" />
            </svg>
          </button>
          <NowPlayingTheatre
            queue={queue}
            currentIndex={currentIndex}
            token={status.state === "signedIn" ? status.token : ""}
          />
          </div>
        )}

        {view === "queue" && ((status.state === "signedIn" && status.token) || localTracks.length > 0) && (
          <QueuePage
            queue={queue}
            currentIndex={currentIndex}
            token={status.state === "signedIn" ? status.token : ""}
            onPlayQueueIndex={playQueueIndex}
            onTrackContextMenu={(track, alb, trackList, trackIndex, e) => setContextMenu({ x: e.clientX, y: e.clientY, type: "track", track, album: alb, trackList, trackIndex })}
            onRemoveFromQueue={removeFromQueue}
            onReorderQueue={reorderQueue}
            onClearQueue={clearQueue}
          />
        )}

        {view === "queue" && status.state !== "signedIn" && localTracks.length === 0 && (
          <div style={{ fontSize: 14, color: "var(--app-muted)" }}>Sign in to Plex or enable local files in Settings to add and see your queue.</div>
        )}

        {view === "playlists" && (
          <div style={{ display: "flex", flexDirection: "column", width: "100%", flex: 1, minHeight: 0 }}>
            {selectedPlaylistId ? (() => {
              const playlist = playlists.find((p) => p.id === selectedPlaylistId);
              if (!playlist) return <div style={{ color: "var(--app-muted)" }}>Playlist not found.</div>;
              const isEditingName = playlistNameEditId === playlist.id;
              return (
                <>
                  <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 10, flexShrink: 0, paddingBottom: 10 }}>
                    <button
                      type="button"
                      onClick={() => { setSelectedPlaylistId(null); setEditingPlaylistId(null); setPlaylistMenuOpenId(null); }}
                      style={{ padding: "9px 13px", borderRadius: 9, border: "1px solid var(--app-border)", background: "var(--app-surface)", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}
                    >
                      ← Back
                    </button>
                    <div style={{ position: "relative", flexShrink: 0 }}>
                      <button
                        type="button"
                        onClick={() => setPlaylistMenuOpenId(playlistMenuOpenId === playlist.id ? null : playlist.id)}
                        style={{ padding: "9px 13px", borderRadius: 9, border: "1px solid var(--app-border)", background: "var(--app-surface)", color: "var(--app-text)", fontSize: 16, cursor: "pointer", lineHeight: 1 }}
                        title="Options"
                        aria-label="Playlist options"
                      >
                        ⋮
                      </button>
                      {playlistMenuOpenId === playlist.id && (
                        <>
                          <div style={{ position: "fixed", inset: 0, zIndex: 1 }} onClick={() => setPlaylistMenuOpenId(null)} aria-hidden />
                          <div style={{ position: "absolute", right: 0, top: "100%", marginTop: 6, padding: 7, borderRadius: 9, background: "var(--app-surface)", border: "1px solid var(--app-border)", boxShadow: "0 4px 12px rgba(0,0,0,0.2)", zIndex: 2, minWidth: 155 }}>
                            <button
                              type="button"
                              onClick={() => { setEditingPlaylistId(editingPlaylistId === playlist.id ? null : playlist.id); setPlaylistMenuOpenId(null); }}
                              style={{ display: "block", width: "100%", textAlign: "left", padding: "9px 13px", borderRadius: 7, border: "none", background: "transparent", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}
                            >
                              {editingPlaylistId === playlist.id ? "Done editing" : "Edit playlist"}
                            </button>
                            <button
                              type="button"
                              onClick={() => { if (window.confirm("Delete this playlist?")) deletePlaylist(playlist.id); setPlaylistMenuOpenId(null); }}
                              style={{ display: "block", width: "100%", textAlign: "left", padding: "9px 13px", borderRadius: 7, border: "none", background: "transparent", color: "#e11d48", fontSize: 13, cursor: "pointer" }}
                            >
                              Delete
                            </button>
                          </div>
                        </>
                      )}
                    </div>
                  </div>
                  <div className="hide-scrollbar" style={{ flex: 1, minHeight: 0, overflowY: "auto", overflowX: "hidden", display: "flex", flexDirection: "column", gap: 18, paddingBottom: 100 }}>
                    <input
                      ref={(el) => { playlistImageInputRef.current = el; }}
                      type="file"
                      accept="image/*"
                      style={{ display: "none" }}
                      onChange={(e) => {
                        const file = e.target.files?.[0];
                        if (file) {
                          const r = new FileReader();
                          r.onload = () => { updatePlaylist(playlist.id, { image: r.result as string }); };
                          r.readAsDataURL(file);
                        }
                        e.target.value = "";
                      }}
                    />
                    <div style={{ display: "flex", gap: 20, alignItems: "flex-start", flexShrink: 0 }}>
                      <div
                        role={editingPlaylistId === playlist.id ? "button" : undefined}
                        onClick={editingPlaylistId === playlist.id ? () => playlistImageInputRef.current?.click() : undefined}
                        style={{
                          width: 206,
                          height: 206,
                          borderRadius: 14,
                          overflow: "hidden",
                          backgroundColor: "var(--app-surface)",
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "center",
                          border: "none",
                          cursor: editingPlaylistId === playlist.id ? "pointer" : "default",
                          padding: 0,
                          flexShrink: 0,
                        }}
                        title={editingPlaylistId === playlist.id ? "Click to change cover" : undefined}
                      >
                        {playlist.image ? (
                          <img src={playlist.image} alt="" style={{ width: "100%", height: "100%", objectFit: "cover" }} />
                        ) : (
                          <span style={{ fontSize: 15, color: "var(--app-muted)" }}>{editingPlaylistId === playlist.id ? "Add cover" : ""}</span>
                        )}
                      </div>
                      <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", justifyContent: "center", gap: 7 }}>
                        {isEditingName ? (
                          <input
                            value={playlistNameEditValue}
                            onChange={(e) => setPlaylistNameEditValue(e.target.value)}
                            onBlur={() => {
                              const v = playlistNameEditValue.trim();
                              if (v) updatePlaylist(playlist.id, { name: v });
                              setPlaylistNameEditId(null);
                            }}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") {
                                const v = playlistNameEditValue.trim();
                                if (v) updatePlaylist(playlist.id, { name: v });
                                setPlaylistNameEditId(null);
                                (e.target as HTMLInputElement).blur();
                              }
                            }}
                            autoFocus
                            style={{ padding: "10px 14px", borderRadius: 10, border: "2px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 24, fontWeight: 700, width: "100%", maxWidth: 345 }}
                          />
                        ) : (
                          <h1
                            style={{ fontSize: 31, fontWeight: 700, margin: 0, color: "var(--app-text)", cursor: "pointer", lineHeight: 1.2, letterSpacing: "-0.02em" }}
                            onClick={() => { setPlaylistNameEditId(playlist.id); setPlaylistNameEditValue(playlist.name); }}
                            title="Click to rename"
                          >
                            {playlist.name}
                          </h1>
                        )}
                        <p style={{ fontSize: 14, color: "var(--app-muted)", margin: 0 }}>{playlist.items.length} track{playlist.items.length !== 1 ? "s" : ""}</p>
                        {playlist.items.length > 0 && (
                          <button
                            type="button"
                            onClick={() => {
                              const items: QueueItem[] = playlist.items.map((it) => ({
                                track: { key: it.trackKey, title: it.title, index: null, duration: null, bitrate: null, audioChannels: null, audioCodec: null, container: null, partKey: it.partKey },
                                album: { key: it.trackKey, title: it.album, artist: it.artist, thumb: it.thumb, year: "" },
                              }));
                              setQueue(items);
                              setCurrentIndex(0);
                              setCurrentTime(0);
                              setDuration(0);
                              setIsPlaying(true);
                            }}
                            style={{ width: 48, height: 48, borderRadius: "50%", border: "none", background: "var(--app-accent)", color: "var(--app-text)", fontSize: 21, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0 }}
                            title="Play playlist"
                            aria-label="Play playlist"
                          >
                            ▶
                          </button>
                        )}
                      </div>
                    </div>
                    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                      {playlist.items.length === 0 && (
                        <button
                          type="button"
                          onClick={() => setView("library")}
                          style={{ padding: "10px 17px", borderRadius: 10, border: "1px solid var(--app-border)", background: "var(--app-surface)", color: "var(--app-text)", fontSize: 14, cursor: "pointer", alignSelf: "flex-start" }}
                        >
                          Add music
                        </button>
                      )}
                      <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 5 }}>
                          {playlist.items.map((item, idx) => {
                            const thumbUrl = getAlbumThumbUrl(item.thumb, status.state === "signedIn" && status.token ? status.token : undefined);
                            const isEditing = editingPlaylistId === playlist.id;
                            const isDragging = draggingPlaylistTrack?.playlistId === playlist.id && draggingPlaylistTrack?.index === idx;
                            const showLineAbove = dropTargetPlaylist?.playlistId === playlist.id && dropTargetPlaylist?.index === idx && dropTargetPlaylist?.place === "before";
                            const showLineBelow = dropTargetPlaylist?.playlistId === playlist.id && dropTargetPlaylist?.index === idx && dropTargetPlaylist?.place === "after";
                            const handleDragOver = (e: React.DragEvent, index: number) => {
                              e.preventDefault();
                              e.dataTransfer.dropEffect = "move";
                              const rect = e.currentTarget.getBoundingClientRect();
                              const place = e.clientY < rect.top + rect.height / 2 ? "before" : "after";
                              setDropTargetPlaylist({ playlistId: playlist.id, index, place });
                            };
                            const handleDrop = (e: React.DragEvent, dropIdx: number) => {
                              e.preventDefault();
                              const from = parseInt(e.dataTransfer.getData("text/plain"), 10);
                              const target = dropTargetPlaylist?.playlistId === playlist.id && dropTargetPlaylist?.index === dropIdx
                                ? dropTargetPlaylist
                                : null;
                              const to = target ? (target.place === "before" ? dropIdx : dropIdx + 1) : dropIdx;
                              setDropTargetPlaylist(null);
                              setDraggingPlaylistTrack(null);
                              if (!Number.isNaN(from) && from !== to) reorderPlaylist(playlist.id, from, to);
                            };
                            const hamburgerSvg = (
                              <svg width={14} height={14} viewBox="0 0 24 24" fill="currentColor" style={{ display: "block" }} aria-hidden>
                                <rect x="3" y="5" width="18" height="2" rx="1" />
                                <rect x="3" y="11" width="18" height="2" rx="1" />
                                <rect x="3" y="17" width="18" height="2" rx="1" />
                              </svg>
                            );
                            return (
                            <li key={`${item.partKey}-${idx}`} style={{ position: "relative" }}>
                              {showLineAbove && (
                                <div style={{ height: 2, background: "var(--app-accent)", borderRadius: 1, marginBottom: 4, flexShrink: 0 }} aria-hidden />
                              )}
                              <div
                                role="button"
                                tabIndex={0}
                                onClick={!isEditing && playlist.items.length > 0 ? () => {
                                  const items: QueueItem[] = playlist.items.map((it) => ({
                                    track: { key: it.trackKey, title: it.title, index: null, duration: null, bitrate: null, audioChannels: null, audioCodec: null, container: null, partKey: it.partKey },
                                    album: { key: it.trackKey, title: it.album, artist: it.artist, thumb: it.thumb, year: "" },
                                  }));
                                  setQueue(items);
                                  setCurrentIndex(idx);
                                  setCurrentTime(0);
                                  setDuration(0);
                                  setIsPlaying(true);
                                } : undefined}
                                onKeyDown={!isEditing && playlist.items.length > 0 ? (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); /* same as click */ } } : undefined}
                                onDragLeave={() => setDropTargetPlaylist((t) => (t?.playlistId === playlist.id ? null : t))}
                                onDragOver={isEditing ? (e) => handleDragOver(e, idx) : undefined}
                                onDrop={isEditing ? (e) => handleDrop(e, idx) : undefined}
                                style={{
                                  display: "flex",
                                  alignItems: "center",
                                  gap: 10,
                                  padding: "8px 12px",
                                  background: "var(--app-surface)",
                                  borderRadius: 8,
                                  borderLeft: "2px solid transparent",
                                  cursor: isEditing ? "default" : "pointer",
                                  opacity: isDragging ? 0.6 : 1,
                                }}
                              >
                                {isEditing && (
                                  <div
                                    draggable
                                    onDragStart={(e) => {
                                      e.dataTransfer.setData("text/plain", String(idx));
                                      e.dataTransfer.effectAllowed = "move";
                                      setDraggingPlaylistTrack({ playlistId: playlist.id, index: idx });
                                    }}
                                    onDragEnd={() => { setDraggingPlaylistTrack(null); setDropTargetPlaylist(null); }}
                                    onMouseDown={(e) => e.stopPropagation()}
                                    onClick={(e) => e.stopPropagation()}
                                    style={{
                                      width: 22,
                                      height: 22,
                                      flexShrink: 0,
                                      display: "flex",
                                      alignItems: "center",
                                      justifyContent: "center",
                                      cursor: isDragging ? "grabbing" : "grab",
                                      color: "var(--app-muted)",
                                    }}
                                    title="Drag to reorder"
                                  >
                                    {hamburgerSvg}
                                  </div>
                                )}
                                <div style={{ width: 41, height: 41, flexShrink: 0, borderRadius: 8, overflow: "hidden", backgroundColor: "var(--app-bg)", display: "flex", alignItems: "center", justifyContent: "center" }}>
                                  <AlbumCover thumbUrl={thumbUrl} style={{ width: "100%", height: "100%" }} />
                                </div>
                                <div style={{ flex: 1, minWidth: 0 }}>
                                  <div style={{ fontSize: 13, fontWeight: 600, color: "var(--app-text)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{item.title}</div>
                                  <div style={{ fontSize: 11, opacity: 0.8, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{item.artist} · {item.album}</div>
                                </div>
                                {isEditing && (
                                  <button type="button" onClick={(e) => { e.stopPropagation(); removeFromPlaylist(playlist.id, idx); }} title="Remove" style={{ width: 24, height: 24, padding: 0, borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-muted)", fontSize: 16, lineHeight: 1, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center" }}>×</button>
                                )}
                              </div>
                              {showLineBelow && (
                                <div style={{ height: 2, background: "var(--app-accent)", borderRadius: 1, marginTop: 4, flexShrink: 0 }} aria-hidden />
                              )}
                            </li>
                          ); })}
                        </ul>
                        {playlist.items.length === 0 && (
                          <p style={{ fontSize: 13, color: "var(--app-muted)", margin: 0 }}>No tracks. Click “Add music” to go to Music, then right‑click tracks or albums to add to this playlist.</p>
                        )}
                      </div>
                  </div>
                </>
              );
            })() : (
              <>
                <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 21 }}>
                  <h1 style={{ fontSize: 24, margin: 0, color: "var(--app-text)", fontWeight: 700 }}>Playlists</h1>
                  <button
                    type="button"
                    onClick={() => setShowNewPlaylistForm(true)}
                    title="New playlist"
                    aria-label="New playlist"
                    style={{ width: 34, height: 34, borderRadius: 9, border: "1px solid var(--app-border)", background: "var(--app-surface)", color: "var(--app-text)", fontSize: 18, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center", lineHeight: 1 }}
                  >
                    +
                  </button>
                </div>
                {showNewPlaylistForm && (
                  <div style={{ display: "flex", alignItems: "center", gap: 7, flexWrap: "wrap", marginBottom: 21 }}>
                    <input
                      type="text"
                      placeholder="Playlist name"
                      value={newPlaylistNameForList}
                      onChange={(e) => setNewPlaylistNameForList(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          const name = newPlaylistNameForList.trim() || "New Playlist";
                          createPlaylist(name);
                          setShowNewPlaylistForm(false);
                          setNewPlaylistNameForList("");
                        } else if (e.key === "Escape") {
                          setShowNewPlaylistForm(false);
                          setNewPlaylistNameForList("");
                        }
                      }}
                      autoFocus
                      style={{ padding: "10px 14px", borderRadius: 9, border: "1px solid var(--app-border)", background: "var(--app-bg)", color: "var(--app-text)", fontSize: 14, minWidth: 224 }}
                    />
                    <button
                      type="button"
                      onClick={() => {
                        const name = newPlaylistNameForList.trim() || "New Playlist";
                        createPlaylist(name);
                        setShowNewPlaylistForm(false);
                        setNewPlaylistNameForList("");
                      }}
                      style={{ padding: "10px 17px", borderRadius: 9, border: "1px solid var(--app-border)", background: "var(--app-accent)", color: "var(--app-text)", fontSize: 14, cursor: "pointer" }}
                    >
                      Create
                    </button>
                    <button
                      type="button"
                      onClick={() => { setShowNewPlaylistForm(false); setNewPlaylistNameForList(""); }}
                      style={{ padding: "10px 17px", borderRadius: 9, border: "1px solid var(--app-border)", background: "var(--app-surface)", color: "var(--app-text)", fontSize: 14, cursor: "pointer" }}
                    >
                      Cancel
                    </button>
                  </div>
                )}
                <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(173px, 1fr))", gap: 21 }}>
                  {playlists.map((p) => (
                    <div
                      key={p.id}
                      role="button"
                      tabIndex={0}
                      onClick={() => {
                        if (playlistOpenTimeoutRef.current) clearTimeout(playlistOpenTimeoutRef.current);
                        playlistOpenTimeoutRef.current = setTimeout(() => {
                          setSelectedPlaylistId(p.id);
                          playlistOpenTimeoutRef.current = null;
                        }, 250);
                      }}
                      onDoubleClick={(e) => {
                        e.preventDefault();
                        if (playlistOpenTimeoutRef.current) {
                          clearTimeout(playlistOpenTimeoutRef.current);
                          playlistOpenTimeoutRef.current = null;
                        }
                        if (p.items.length === 0) return;
                        const items: QueueItem[] = p.items.map((it) => ({
                          track: { key: it.trackKey, title: it.title, index: null, duration: null, bitrate: null, audioChannels: null, audioCodec: null, container: null, partKey: it.partKey },
                          album: { key: it.trackKey, title: it.album, artist: it.artist, thumb: it.thumb, year: "" },
                        }));
                        setQueue(items);
                        setCurrentIndex(0);
                        setCurrentTime(0);
                        setDuration(0);
                        setIsPlaying(true);
                      }}
                      onContextMenu={(e) => {
                        e.preventDefault();
                        setPlaylistContextMenu({ x: e.clientX, y: e.clientY, playlist: p });
                      }}
                      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setSelectedPlaylistId(p.id); } }}
                      style={{ minWidth: 0, cursor: "pointer", userSelect: "none", WebkitUserSelect: "none" }}
                    >
                      <div
                        style={{
                          borderRadius: 12,
                          overflow: "hidden",
                          aspectRatio: "1",
                          backgroundColor: "var(--app-surface)",
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "center",
                        }}
                      >
                        {p.image ? (
                          <img src={p.image} alt="" draggable={false} style={{ width: "100%", height: "100%", objectFit: "cover", pointerEvents: "none" }} />
                        ) : (
                          <span style={{ fontSize: 48, opacity: 0.4 }}>♪</span>
                        )}
                      </div>
                      <div style={{ paddingTop: 10, paddingBottom: 0, paddingLeft: 2, paddingRight: 2, minWidth: 0 }}>
                        <div style={{ fontWeight: 600, fontSize: 14, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{p.name}</div>
                        <div style={{ fontSize: 11, opacity: 0.8, marginTop: 4, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{p.items.length} track{p.items.length !== 1 ? "s" : ""}</div>
                      </div>
                    </div>
                  ))}
                </div>
                {playlistContextMenu && (
                  <>
                    <div style={{ position: "fixed", inset: 0, zIndex: 10 }} onClick={() => setPlaylistContextMenu(null)} aria-hidden />
                    <div
                      style={{
                        position: "fixed",
                        left: playlistContextMenu.x,
                        top: playlistContextMenu.y,
                        zIndex: 11,
                        padding: 6,
                        borderRadius: 9,
                        background: "var(--app-surface)",
                        border: "1px solid var(--app-border)",
                        boxShadow: "0 4px 12px rgba(0,0,0,0.2)",
                        minWidth: 138,
                      }}
                    >
                      <button
                        type="button"
                        onClick={() => {
                          const items: QueueItem[] = playlistContextMenu.playlist.items.map((it) => ({
                            track: { key: it.trackKey, title: it.title, index: null, duration: null, bitrate: null, audioChannels: null, audioCodec: null, container: null, partKey: it.partKey },
                            album: { key: it.trackKey, title: it.album, artist: it.artist, thumb: it.thumb, year: "" },
                          }));
                          setQueue((q) => [...q, ...items]);
                          setPlaylistContextMenu(null);
                        }}
                        style={{ display: "block", width: "100%", textAlign: "left", padding: "9px 13px", borderRadius: 7, border: "none", background: "transparent", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}
                      >
                        Add to queue
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          const pl = playlistContextMenu.playlist;
                          const items: QueueItem[] = pl.items.map((it) => ({
                            track: { key: it.trackKey, title: it.title, index: null, duration: null, bitrate: null, audioChannels: null, audioCodec: null, container: null, partKey: it.partKey },
                            album: { key: it.trackKey, title: it.album, artist: it.artist, thumb: it.thumb, year: "" },
                          }));
                          for (let i = items.length - 1; i > 0; i--) {
                            const j = Math.floor(Math.random() * (i + 1));
                            [items[i], items[j]] = [items[j], items[i]];
                          }
                          setQueue(items);
                          setCurrentIndex(0);
                          setCurrentTime(0);
                          setDuration(0);
                          setIsPlaying(true);
                          setPlaylistContextMenu(null);
                        }}
                        style={{ display: "block", width: "100%", textAlign: "left", padding: "9px 13px", borderRadius: 7, border: "none", background: "transparent", color: "var(--app-text)", fontSize: 13, cursor: "pointer" }}
                      >
                        Shuffle play
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          if (window.confirm("Delete this playlist?")) deletePlaylist(playlistContextMenu.playlist.id);
                          setPlaylistContextMenu(null);
                        }}
                        style={{ display: "block", width: "100%", textAlign: "left", padding: "9px 13px", borderRadius: 7, border: "none", background: "transparent", color: "#ef4444", fontSize: 13, cursor: "pointer" }}
                      >
                        Delete
                      </button>
                    </div>
                  </>
                )}
                {playlists.length === 0 && <p style={{ fontSize: 14, color: "var(--app-muted)" }}>Create a playlist to get started.</p>}
              </>
            )}
          </div>
        )}
      </main>

      {/* Player bar - always visible so user can use Now Playing toggle */}
      <>
        {playbackError && (
          <div
            style={{
              position: "fixed",
              bottom: 80,
              left: "50%",
              transform: "translateX(-50%)",
              padding: "10px 16px",
              borderRadius: 10,
              background: "var(--app-surface)",
              border: "1px solid var(--app-border)",
              color: "var(--app-accent)",
              fontSize: 13,
              maxWidth: "90%",
              zIndex: 100,
              display: "flex",
              alignItems: "center",
              gap: 12,
            }}
          >
            <span>{playbackError}</span>
            <span style={{ color: "var(--app-muted)", fontSize: 12 }}>Ensure the server is running (npm run server).</span>
            <button
              type="button"
              onClick={() => setPlaybackError(null)}
              style={{
                padding: "4px 8px",
                borderRadius: 6,
                border: "1px solid var(--app-border)",
                background: "var(--app-bg)",
                color: "var(--app-text)",
                fontSize: 12,
                cursor: "pointer",
              }}
            >
              Dismiss
            </button>
          </div>
        )}
      <PlayerBar
        queue={queue}
        currentIndex={currentIndex}
        isPlaying={isPlaying}
        currentTime={currentTime}
        duration={duration}
        currentAlbum={currentAlbum}
        thumbUrl={playerThumbUrl}
        loadingTrack={loadingTrack}
        volume={volume}
        shuffle={shuffle}
        repeatMode={repeatMode}
        onVolumeChange={setVolume}
        onMuteToggle={() => {
          if (volume > 0) {
            volumeBeforeMuteRef.current = volume;
            setVolume(0);
          } else {
            setVolume(volumeBeforeMuteRef.current || 0.5);
          }
        }}
        onPlayPause={handlePlayPause}
        onSeek={handleSeek}
        onPrevious={handlePrevious}
        onNext={handleNext}
        onShuffle={() => {
          const list = playerStateRef.current.queue;
          const currentIdx = playerStateRef.current.currentIndex;
          if (!shuffle) {
            // Turn shuffle ON: save current order, randomize list, set queue to that list (no add/remove)
            if (list.length <= 1) {
              setShuffle(true);
              return;
            }
            setOriginalQueueOrder([...list]);
            const randomized = shuffleArray([...list]);
            const currentItem = list[currentIdx];
            const newIdx = randomized.findIndex((item) => item.track.partKey === currentItem?.track.partKey && item.track.key === currentItem?.track.key);
            setQueue(randomized);
            setCurrentIndex(newIdx >= 0 ? newIdx : 0);
            setShuffle(true);
          } else {
            // Turn shuffle OFF: revert to saved order
            const saved = originalQueueOrder;
            if (!saved || saved.length === 0) {
              setShuffle(false);
              return;
            }
            const currentItem = list[currentIdx];
            const restoredIdx = saved.findIndex((item) => item.track.partKey === currentItem?.track.partKey && item.track.key === currentItem?.track.key);
            setQueue(saved);
            setCurrentIndex(restoredIdx >= 0 ? restoredIdx : 0);
            setOriginalQueueOrder(null);
            setShuffle(false);
          }
        }}
        onRepeat={() => {
          setRepeatMode((m) => (m === "off" ? "all" : m === "all" ? "one" : "off"));
        }}
        isNowPlaying={view === "nowPlaying"}
        onNowPlayingToggle={() => {
          if (view === "nowPlaying") {
            setView(previousViewBeforeNowPlayingRef.current);
          } else {
            previousViewBeforeNowPlayingRef.current = view;
            setView("nowPlaying");
          }
        }}
      />
      </>
      </div>
      </div>
    </div>
  );
}

export default App;

