use crate::plex::Entry;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{env, fs, io::BufReader, path::PathBuf};

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Playlist {
    pub name: String,
    pub items: Vec<Entry>,
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cover: Vec<u8>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Preferences {
    pub theme: String,
    pub text_mode: i32,
    pub auto_art_theme: bool,
    pub sidebar: f32,
    pub track_panel: f32,
    pub lock_layout: bool,
    pub list_view: bool,
    pub local_enabled: bool,
    pub volume: f32,
    pub muted: bool,
    pub shuffled: bool,
    pub repeat_mode: i32,
    pub eq_gains: [f32; 20],
    pub eq_preamp: f32,
    pub eq_preset: i32,
    pub normalizer: bool,
    pub font: i32,
    pub discord_id: String,
    pub discord_image: String,
    pub streaming_local: i32,
    pub streaming_remote: i32,
    /// Look up lyrics on LRCLIB when a track has none of its own.
    pub online_lyrics: bool,
    /// Plex music libraries to show (section keys); empty shows all of them.
    pub plex_libraries: Vec<String>,
    /// Plex server address to try when away from home (e.g. "203.0.113.7:25565").
    pub plex_address: String,
    /// Output device by name; empty follows the system default.
    pub audio_device: String,
    /// Folders local files are loaded from; empty uses Music\SonicMusic.
    pub local_folders: Vec<String>,
    /// The album-art theme color of the last song, shown again on the next launch.
    pub art_theme: Option<[u8; 3]>,
    /// Closing the window keeps Sonic playing in the tray (Windows); off quits.
    pub close_to_tray: bool,
    /// Draw with the GPU (smooth scrolling); off uses the lighter CPU renderer.
    pub gpu_rendering: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: "#0f172a".into(),
            text_mode: 0,
            auto_art_theme: false,
            sidebar: 260.0,
            track_panel: 245.0,
            lock_layout: true,
            list_view: false,
            local_enabled: false,
            volume: 1.0,
            muted: false,
            shuffled: false,
            repeat_mode: 0,
            eq_gains: [0.0; 20],
            eq_preamp: 0.0,
            eq_preset: 0,
            normalizer: false,
            font: 0,
            discord_id: String::new(),
            discord_image: String::new(),
            streaming_local: 2,
            streaming_remote: 2,
            online_lyrics: true,
            plex_libraries: Vec::new(),
            plex_address: String::new(),
            audio_device: String::new(),
            local_folders: Vec::new(),
            art_theme: None,
            close_to_tray: true,
            gpu_rendering: true,
        }
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct PlaybackSnapshot {
    pub queue: Vec<Entry>,
    pub current: usize,
    pub server_id: Option<String>,
    #[serde(default)]
    pub original_order: Vec<String>,
    /// Seconds into the current track when the session was saved.
    #[serde(default)]
    pub position: f32,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LibraryData {
    pub playlists: Vec<Playlist>,
    pub preferences: Preferences,
    pub playback: Option<PlaybackSnapshot>,
}

pub fn path() -> Option<PathBuf> {
    if cfg!(windows) {
        env::var_os("LOCALAPPDATA")
            .map(|base| PathBuf::from(base).join("Sonic").join("library.json"))
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME")
            .map(|base| PathBuf::from(base).join("Library/Application Support/Sonic/library.json"))
    } else {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .map(|base| base.join("sonic/library.json"))
    }
}

pub fn load() -> Result<LibraryData> {
    let path = path().context("No user data directory")?;
    match fs::File::open(path) {
        Ok(file) => {
            serde_json::from_reader(BufReader::new(file)).context("Reading saved Sonic data")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LibraryData::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn save(data: &LibraryData) -> Result<()> {
    let path = path().context("No user data directory")?;
    let parent = path.parent().context("No parent directory")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(file.as_file_mut(), data)?;
    file.as_file_mut().sync_all()?;
    file.persist(path)?;
    Ok(())
}
