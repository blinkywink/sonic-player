#!/usr/bin/env node
/**
 * Prepares release-app/ for Mac extraResources: out/ (web dist), srv/ (bundled server), public.
 * Server is bundled with esbuild so it doesn't need node_modules (NODE_PATH isn't used for ESM).
 * Run before electron-builder --mac.
 */
const fs = require("fs");
const path = require("path");
const { execSync } = require("child_process");

const root = path.join(__dirname, "..");
const releaseApp = path.join(root, "release-app");
const srv = path.join(releaseApp, "srv");

// Patch media service so macOS next/previous stay enabled when setting now-playing info
require(path.join(root, "scripts", "patch-media-service-darwin.cjs"));

// Clean
if (fs.existsSync(releaseApp)) {
  fs.rmSync(releaseApp, { recursive: true });
}
fs.mkdirSync(srv, { recursive: true });

// Copy web dist as "out", and public for tray icon
const distSrc = path.join(root, "dist");
if (!fs.existsSync(distSrc)) {
  console.error("Missing dist/ - run npm run build first");
  process.exit(1);
}
fs.cpSync(distSrc, path.join(releaseApp, "out"), { recursive: true });
fs.cpSync(path.join(root, "public"), path.join(releaseApp, "public"), { recursive: true });

// Bundle server as CommonJS so Node built-ins (path, fs, etc.) work at runtime (ESM bundle had "Dynamic require not supported")
const serverEntry = path.join(root, "server.js");
const serverBundle = path.join(srv, "server.bundle.cjs");
// Define import.meta so CJS bundle has no import.meta reference (avoids esbuild warning)
execSync(
  `npx esbuild "${serverEntry}" --bundle --platform=node --format=cjs --outfile="${serverBundle}" --external:discord-rpc --define:import.meta=undefined --define:import.meta.url=undefined`,
  { cwd: root, stdio: "inherit" }
);

// Ship discord-rpc so the packaged app can load it (main app "files" does not include node_modules)
const releaseNodeModules = path.join(releaseApp, "node_modules");
const discordRpcSrc = path.join(root, "node_modules", "discord-rpc");
const discordRpcDest = path.join(releaseNodeModules, "discord-rpc");
if (fs.existsSync(discordRpcSrc)) {
  fs.mkdirSync(releaseNodeModules, { recursive: true });
  fs.cpSync(discordRpcSrc, discordRpcDest, { recursive: true });
}

console.log("[prepare-mac-app] release-app/ ready for extraResources");
