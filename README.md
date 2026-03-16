# Crystal Music

Web app + Node server for streaming music from Plex. Playlists, queue, and playback. Optional desktop tray app (macOS menu bar / Windows system tray) that runs the server and opens the app in your browser.

## Run in browser

1. Start the backend:
   ```bash
   npm run server
   ```
2. Start the frontend (dev):
   ```bash
   npm run dev
   ```
3. Open http://localhost:1430 and sign in with Plex.

## Build for production

```bash
npm run build
```

Then run the server with the built frontend:

```bash
NODE_ENV=production node server.js
```

Open http://localhost:4000 — the server serves the built app.

## Desktop tray app (macOS / Windows)

Runs the server and shows an icon in the **menu bar** (Mac) or **system tray** (Windows). Click to open the app in your browser. Options: Open in Browser, Run at startup, Quit.

- **Dev:** build once, then run Electron:
  ```bash
  npm run electron:dev
  ```
- **Package installers:**
  - macOS (DMG): `npm run package:mac`
  - Windows (NSIS installer): `npm run package:win`
  - Both: `npm run package`

Output is in the `dist/` folder (electron-builder output, not Vite’s dist).

## Features

- **Music:** Browse Plex libraries, play albums/tracks, queue, shuffle, repeat.
- **Playlists:** Create playlists, add songs/albums (from Music or Queue), drag to reorder, rename, set a cover image. Stored in `data/playlists.json`.
- **Now Playing / Queue / Settings:** Theme, streaming quality, cache reset.

## Push to GitHub

The repo is ready to push: `.gitignore` excludes `node_modules/`, `dist/`, and `data/`. Run `npm run build` and the tray app scripts on your machine; no need to commit build artifacts.
