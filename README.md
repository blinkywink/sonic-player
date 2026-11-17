<p align="center"><img alt="Sonic logo" src="native/assets/icon.png" width="96"></p>

# Sonic

A native Plex and local music player built with Rust, Slint, and Rodio. Browse albums, manage your queue and playlists, tune the equalizer, and connect Discord Rich Presence or Last.fm.

## Run on Windows

Open a successful [Windows build](https://github.com/blinkywink/sonic-player/actions/workflows/build.yml) and download the `sonic-windows-x64` artifact. Extract it and run `sonic-native.exe`.

- **Plex:** Open **Settings → Library → Sign in with Plex**, approve Sonic in your browser, then return to the app.
- **Local music:** Enable local files in **Settings → Library**, put music in the `SonicMusic` folder, and refresh. You can also pass audio file paths when launching Sonic.
- **Discord and Last.fm:** Both are under **Settings → Tracking**. Discord uses an Application ID and an optional image asset key. Last.fm uses an API key and secret, followed by browser approval.

On Windows, the Sonic tray menu has **Open Sonic**, **Run at startup**, and **Quit**. Closing the window keeps the player in the tray.

## Build from source

On Windows, install the Rust MSVC toolchain and Visual Studio C++ Build Tools with a Windows SDK. From the repository root:

```powershell
cargo build --release --locked --manifest-path native/Cargo.toml
cargo test --release --locked --manifest-path native/Cargo.toml
```

The executable is at `native/target/release/sonic-native.exe`. Windows is the verified build target. macOS and Linux still need runtime verification.

The [previous Electron client](https://github.com/blinkywink/sonic-player/tree/80a1ecb) is available in Git history.
