<p align="center"><img alt="Sonic logo" src="native/assets/icon.png" width="96"></p>

# Sonic

A native Plex and local music player built with Rust, Slint, and Rodio. Browse albums, manage your queue and playlists, tune the equalizer, and connect Discord Rich Presence or Last.fm.

## Features

- **Library:** Plex and local files in one album grid with search, sorting and grouping. Click an album to see its songs in the sidebar, double-click to play, right-click for Add to queue, Shuffle play or Add to playlist.
- **Queue:** grouped by album, drag to reorder, and Clear queue keeps the current song playing.
- **Playlists:** create, rename, reorder and remove tracks, add a custom cover, or let Sonic tile the first four albums as the cover.
- **Now Playing:** full-window cover with synced lyrics that follow the song and can be clicked to jump. Lyrics come from a `.lrc` file next to a local track, lyrics embedded in the file, or [LRCLIB](https://lrclib.net). The online lookup sends only the title, artist, album and length, and can be turned off in **Settings → Playback**.
- **Playback:** gapless playback, a 20-band EQ with loudness normalization and preamp, live level bars on the playing track, and pop-free seeking.
- **Windows media controls:** media keys and the system media flyout (with cover art) work as soon as a track is loaded.
- **Look:** themes from a chosen color or from the playing album's art, kept dark and subtle.

## Run on Windows

Download `sonicplayer-windows-x64.zip` from the [latest release](https://github.com/blinkywink/sonic-player/releases/latest), extract it, and run `sonicplayer.exe`. After that it keeps itself up to date: it checks for new releases in the background and the new version runs the next time you open Sonic.

- **Plex:** Open **Settings → Library → Sign in with Plex**, approve Sonic in your browser, then return to the app. If your server has several music libraries, choose which ones to show under the Plex account.
- **Local music:** Enable local files in **Settings → Library**, put music in the `SonicMusic` folder, and refresh. You can also pass audio file paths when launching Sonic.
- **Discord and Last.fm:** Both are under **Settings → Tracking**. Discord uses an Application ID and an optional image asset key. Last.fm uses an API key and secret, followed by browser approval.

On Windows, the Sonic tray menu has **Open Sonic**, **Run at startup**, and **Quit**. Closing the window keeps the player in the tray.

## Run on Linux or macOS (preview)

Linux and macOS downloads are on the same [releases page](https://github.com/blinkywink/sonic-player/releases/latest). Both are new and less tested than Windows.

- **Linux (x64):** download `sonicplayer-linux-x64.tar.gz`, extract it, and run `./install.sh` to add Sonic to your app launcher (no root needed), or run `./sonicplayer` directly. Needs ALSA (or PipeWire/PulseAudio with the ALSA plugin). Media keys work through MPRIS.
- **macOS (Apple Silicon):** download `sonicplayer-macos-arm64.zip`, unzip, and move **Sonic.app** to Applications. The app isn't notarized, so the first time, right-click it and choose **Open**.
- On both, closing the window quits Sonic (there's no tray yet), and the theme swatch doesn't open a color picker, so type a hex color instead.

## iPhone (sideload, preview)

A lean iPhone version lives in `mobile/`: your albums sorted A–Z by artist with search (2, 3 or 4 per row), an album's songs, a full-screen player (tap the mini player; swipe down to close), a simple queue (swipe a song left to remove it), synced lyrics, lock screen and headphone controls with artwork, Last.fm scrobbling, EQ presets, volume leveling, and Plex settings. Swipe in from the left edge to go back. It shares Sonic's Plex, streaming, lyrics, EQ and Last.fm code.

Download `sonic-ios-unsigned.ipa` from the [latest release](https://github.com/blinkywink/sonic-player/releases/latest) (or the `sonic-ios-unsigned` build artifact) and install it with your sideloading tool, which signs it with your Apple ID. It's built on GitHub's macOS runners, so no Mac is needed. Sign in with Plex in the Settings tab; on your home network, allow Sonic's local network access so it can reach your server. The newest IPA is always at `https://github.com/blinkywink/sonic-player/releases/latest/download/sonic-ios-unsigned.ipa`.

To try the phone UI on a desktop: `cargo run --release --manifest-path mobile/Cargo.toml` (it opens a phone-sized window and shows your local music folder when you're not signed in).

## Seamless playback

- Songs you play (and the next ones, preloaded) are kept on disk: up to 5 GB on desktop and 2 GB on iPhone, oldest removed first. Replays, going back and reopening albums start instantly, even offline.
- A dropped connection (switching from Wi-Fi to cellular, a tunnel) resumes the download from the same byte, finding a new server address if needed; seeking into a part that isn't downloaded yet jumps the download there.
- The next song is ready before the current one ends, so albums play gapless; the album list, tracks, loudness and lyrics are saved and only refreshed when something changed (or you refresh).

## Away from home

Sonic tries all of your Plex server's addresses at once and uses the best one that answers: your home network, then a direct remote connection, then Plex's relay. If the network changes (say you leave home), it finds a new address on its own. For fast remote playback, turn on **Remote Access** in your Plex server's settings and, if you forwarded a custom port, enter it there as the manual public port; otherwise Sonic falls back to Plex's slower relay.

## Build from source

On Windows, install the Rust MSVC toolchain and Visual Studio C++ Build Tools with a Windows SDK. From the repository root:

```powershell
cargo build --release --locked --manifest-path native/Cargo.toml
cargo test --release --locked --manifest-path native/Cargo.toml
```

The executable is at `native/target/release/sonicplayer.exe`. On Linux, install the ALSA, xkbcommon and fontconfig development packages first (for example `sudo apt install pkg-config libasound2-dev libxkbcommon-dev libfontconfig1-dev`). Windows is the most tested platform; Linux and macOS build and pass tests in CI but still need more real-world use.

The [previous Electron client](https://github.com/blinkywink/sonic-player/tree/80a1ecb) is available in Git history.

## Releases

Every push to `main` builds and tests Windows, Linux, macOS and the iPhone app ([workflow](https://github.com/blinkywink/sonic-player/actions/workflows/build.yml)). To publish a release, bump `version` in `native/Cargo.toml`, commit, then push a matching tag:

```sh
git tag v0.2.0
git push origin v0.2.0
```

The workflow checks the tag matches the version and attaches every download to a new GitHub Release. Installed Windows copies pick it up automatically.
