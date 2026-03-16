#!/usr/bin/env node
/**
 * Patches @arcanewizards/electron-media-service darwin service so next/previous
 * track commands stay enabled when we set now-playing info (stops them being greyed out).
 */
const fs = require("fs");
const path = require("path");

const root = path.join(__dirname, "..");
const serviceMm = path.join(
  root,
  "node_modules",
  "@arcanewizards",
  "electron-media-service",
  "src",
  "darwin",
  "service.mm"
);

if (!fs.existsSync(serviceMm)) {
  console.log("[patch-media-service-darwin] service.mm not found, skip");
} else {
  let content = fs.readFileSync(serviceMm, "utf8");
  const marker = "  [[MPNowPlayingInfoCenter defaultCenter] setNowPlayingInfo:songInfo];";
  const insertion =
    "  MPRemoteCommandCenter *remoteCommandCenter = [MPRemoteCommandCenter sharedCommandCenter];\n" +
    "  [remoteCommandCenter nextTrackCommand].enabled = true;\n" +
    "  [remoteCommandCenter previousTrackCommand].enabled = true;\n\n  " +
    "[[MPNowPlayingInfoCenter defaultCenter] setNowPlayingInfo:songInfo];";

  if (content.includes("previousTrackCommand].enabled = true;\n\n  [[MPNowPlayingInfoCenter defaultCenter] setNowPlayingInfo:songInfo]")) {
    console.log("[patch-media-service-darwin] already patched");
  } else if (!content.includes(marker)) {
    console.warn("[patch-media-service-darwin] marker not found, skip");
  } else {
    content = content.replace(marker, insertion);
    fs.writeFileSync(serviceMm, content);
    console.log("[patch-media-service-darwin] patched service.mm");
  }
}
