#!/usr/bin/env node
/**
 * Replaces /Applications/Sonic.app with the built app from dist/mac-arm64/Sonic.app.
 * Run after: npm run package:mac
 * Requires sudo: node scripts/install-mac-app.cjs   (or: sudo cp -R dist/mac-arm64/Sonic.app /Applications/)
 */
const path = require("path");
const fs = require("fs");

const root = path.join(__dirname, "..");
const source = path.join(root, "dist", "mac-arm64", "Sonic.app");
const target = "/Applications/Sonic.app";

if (!fs.existsSync(source)) {
  console.error("Not found:", source);
  console.error("Run first: npm run package:mac");
  process.exit(1);
}

const { execSync } = require("child_process");
try {
  execSync(`rm -rf "${target}"`, { stdio: "inherit" });
  execSync(`cp -R "${source}" "${target}"`, { stdio: "inherit" });
  console.log("Sonic.app installed to /Applications");
} catch (e) {
  if (e.status === 1 && /Permission denied|Operation not permitted/.test(e.message || "")) {
    console.error("Permission denied. Run with sudo:");
    console.error("  sudo node scripts/install-mac-app.cjs");
    console.error("Or manually:");
    console.error("  sudo rm -rf /Applications/Sonic.app");
    console.error("  sudo cp -R dist/mac-arm64/Sonic.app /Applications/");
  }
  process.exit(1);
}
