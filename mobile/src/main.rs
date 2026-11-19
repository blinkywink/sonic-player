//! Sonic for iPhone: albums (A–Z by artist, searchable), an album's songs, playback
//! with a simple queue and a full-screen player, Last.fm scrobbling, EQ and Plex
//! settings. Shares Sonic's Plex, streaming, lyrics, EQ and Last.fm code with the
//! desktop app; on a desktop it runs in a phone-sized window for testing.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod credentials;
#[allow(dead_code)] // Shared with the desktop app, which uses more of it.
#[path = "../../native/src/eq.rs"]
mod eq;
#[cfg(target_os = "ios")]
mod ios;
/// Desktop test builds have no system media controls.
#[cfg(not(target_os = "ios"))]
mod ios {
    use crate::{NowPlaying, Remote};
    pub fn start_remote_commands() {}
    pub fn take_commands() -> Vec<Remote> {
        Vec::new()
    }
    pub fn artwork_key() -> Option<String> {
        None
    }
    pub fn set_artwork(_key: &str, _bytes: &[u8]) {}
    pub fn now_playing(_info: Option<&NowPlaying>) {}
    pub fn open_url(url: &str) {
        let _ = open::that_detached(url);
    }
    pub fn open_pending() {}
    pub fn is_active() -> bool {
        true
    }
}
#[allow(dead_code)] // Shared with the desktop app, which uses more of it.
#[path = "../../native/src/lastfm.rs"]
mod lastfm;
#[cfg(not(target_os = "ios"))]
#[path = "../../native/src/local.rs"]
mod local;
#[path = "../../native/src/lyrics.rs"]
mod lyrics;
#[allow(dead_code)] // Shared with the desktop app, which uses more of it.
#[path = "../../native/src/plex.rs"]
mod plex;
#[allow(dead_code)] // Shared with the desktop app, which uses more of it.
#[path = "../../native/src/stream.rs"]
mod stream;

use anyhow::{Context, Result};
use plex::{Entry, Plex};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink};
use serde::{Deserialize, Serialize};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode,
    VecModel,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    io::{BufReader, Read, Seek},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

slint::include_modules!();

/// Covers are shown at most ~170 pt wide (about 510 px on a 3x screen); 360 px keeps
/// them sharp enough while keeping memory small.
const COVER_SIZE: u32 = 360;
/// Decoded covers kept in memory; older ones are dropped and reloaded from disk.
const COVER_CACHE: usize = 120;
/// Lock screen artwork size (the system scales it down as needed).
const NOW_ART_SIZE: u32 = 1024;
/// How often failed scrobbles are retried.
const SCROBBLE_RETRY: Duration = Duration::from_secs(120);

type Art = SharedPixelBuffer<Rgba8Pixel>;

trait AudioInput: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> AudioInput for T {}

enum Event {
    Library(Result<(Plex, Vec<Entry>)>),
    NoPlex,
    #[cfg(not(target_os = "ios"))]
    Local(Result<Vec<(Entry, Vec<Entry>)>>),
    Tracks(String, Result<Vec<Entry>>),
    Art(String, Option<Art>),
    Audio(u64, Result<(Decoder<BufReader<Box<dyn AudioInput>>>, Option<plex::Loudness>)>),
    Lyrics(String, Option<Vec<lyrics::Line>>),
    /// Full-size cover (encoded) for the lock screen, by album key.
    NowArt(String, Vec<u8>),
    /// An album's tracks to add to the queue (fetched because they weren't saved).
    Enqueue(Result<Vec<Entry>>),
    /// Album tracks loaded in bulk (replacing the saved ones after a refresh).
    TracksLoaded(HashMap<String, Vec<Entry>>, bool),
    FmRestored(Option<lastfm::Session>),
    FmPending(u64, Result<lastfm::Pending>),
    FmConnected(u64, Result<lastfm::Session>),
    /// Scrobbles that still need sending (failed for now), and the last error.
    FmUnsent(Vec<Scrobble>, Option<String>),
}

/// A command from the lock screen, Control Center, headphones or car.
#[derive(Clone, Copy)]
pub enum Remote {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Seek(f64),
}

/// What the system media controls show.
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub elapsed: f64,
    pub playing: bool,
    pub art_key: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Scrobble {
    track: Entry,
    started_at: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
struct Settings {
    /// Plex music libraries to show; empty shows all.
    libraries: Vec<String>,
    /// Albums per row in the library (2–4).
    columns: u32,
    eq_preset: usize,
    normalizer: bool,
    online_lyrics: bool,
    /// Plex address to try away from home, e.g. "203.0.113.7:25565".
    server_address: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            libraries: Vec::new(),
            columns: 2,
            eq_preset: 0,
            normalizer: false,
            online_lyrics: true,
            server_address: String::new(),
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// Settings and unsent scrobbles (kept).
fn data_path(name: &str) -> Option<PathBuf> {
    Some(home()?.join("Library/Application Support/Sonic").join(name))
}

/// Album list and covers (may be cleared by iOS).
fn cache_path(name: &str) -> Option<PathBuf> {
    Some(home()?.join("Library/Caches/Sonic").join(name))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: Option<PathBuf>) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path?).ok()?).ok()
}

fn write_bytes(path: Option<PathBuf>, bytes: &[u8]) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, bytes);
}

fn write_json<T: Serialize>(path: Option<PathBuf>, value: &T) {
    if let Ok(bytes) = serde_json::to_vec(value) {
        write_bytes(path, &bytes);
    }
}

fn cover_file(key: &str) -> Option<PathBuf> {
    let name: String = key.chars().filter(char::is_ascii_alphanumeric).collect();
    if name.is_empty() {
        return None;
    }
    cache_path(&format!("covers/{name}.jpg"))
}

fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Sort text: case-insensitive, ignoring a leading "The ".
fn sort_text(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    lower.strip_prefix("the ").map(str::to_owned).unwrap_or(lower)
}

/// Artist A–Z, then each artist's albums newest first.
fn sort_albums(albums: &mut [Entry]) {
    albums.sort_by_cached_key(|album| {
        (sort_text(&album.parent_title), std::cmp::Reverse(album.year), sort_text(&album.title))
    });
}

struct App {
    tx: Sender<Event>,
    settings: Settings,
    plex: Option<Plex>,
    albums: Vec<Entry>,
    /// Album indices shown in each grid row (after search).
    rows: Vec<Vec<usize>>,
    query: String,
    tracks: HashMap<String, Vec<Entry>>,
    open_album: String,
    art: HashMap<String, Image>,
    art_order: VecDeque<String>,
    pending_art: HashSet<String>,
    queue: Vec<Entry>,
    current: usize,
    generation: u64,
    stream: Option<OutputStream>,
    sink: Option<Arc<Sink>>,
    playing: bool,
    equalizer: Arc<eq::Settings>,
    album_model: Rc<VecModel<AlbumItem>>,
    lyrics_cache: HashMap<String, Option<Vec<lyrics::Line>>>,
    lyrics_pending: HashSet<String>,
    lyrics_key: String,
    lyrics_chars: usize,
    lyric_index: i32,
    /// Album whose full-size lock screen cover is being fetched.
    now_art_pending: String,
    fm_session: Option<lastfm::Session>,
    fm_generation: u64,
    /// When the current song started (Unix seconds), how long it has really played,
    /// and whether it has been scrobbled.
    fm_started: Option<u64>,
    fm_listened: Duration,
    fm_done: bool,
    fm_unsent: Vec<Scrobble>,
    fm_sending: bool,
    fm_last_retry: Instant,
    last_tick: Instant,
    /// Whether the queue was shuffled (leveling then uses track gain).
    shuffled: bool,
    /// The next library load re-fetches every track's loudness (pull to refresh).
    reload_all_loudness: bool,
    /// Where the song was at the last tick, and how often it has been resumed after
    /// the stream dropped out (so a flaky connection doesn't skip the song).
    last_position: f32,
    resumes: u32,
    resume_at: Option<f32>,
    /// Whether the app was on screen at the last tick.
    was_active: bool,
}

impl App {
    fn publish_albums(&mut self, ui: &MobileApp) {
        self.album_model.set_vec(
            self.albums
                .iter()
                .map(|album| AlbumItem {
                    key: album.rating_key.as_str().into(),
                    title: album.title.as_str().into(),
                    artist: album.parent_title.as_str().into(),
                    art: self.art.get(&album.rating_key).cloned().unwrap_or_default(),
                })
                .collect::<Vec<_>>(),
        );
        self.publish_rows(ui);
    }

    /// Lays the (searched) albums out in rows of the chosen width.
    fn publish_rows(&mut self, ui: &MobileApp) {
        let columns = self.settings.columns.clamp(2, 4) as usize;
        let query = self.query.trim().to_lowercase();
        let visible: Vec<usize> = (0..self.albums.len())
            .filter(|&index| {
                let album = &self.albums[index];
                query.is_empty()
                    || album.title.to_lowercase().contains(&query)
                    || album.parent_title.to_lowercase().contains(&query)
            })
            .collect();
        self.rows = visible.chunks(columns).map(<[usize]>::to_vec).collect();
        ui.set_columns(columns as i32);
        ui.set_album_rows(ModelRc::new(VecModel::from(
            self.rows
                .iter()
                .map(|row| AlbumRow {
                    items: ModelRc::new(VecModel::from(row.iter().map(|&i| i as i32).collect::<Vec<_>>())),
                })
                .collect::<Vec<_>>(),
        )));
    }

    fn publish_libraries(&self, ui: &MobileApp) {
        let Some(plex) = &self.plex else {
            ui.set_libraries(ModelRc::default());
            return;
        };
        let chosen = plex.chosen_sections(&self.settings.libraries);
        ui.set_libraries(ModelRc::new(VecModel::from(
            plex.sections
                .iter()
                .map(|(key, title)| LibraryItem {
                    title: if title.is_empty() { "Music".into() } else { title.as_str().into() },
                    selected: chosen.contains(key),
                })
                .collect::<Vec<_>>(),
        )));
    }

    fn publish_settings(&self, ui: &MobileApp) {
        ui.set_columns(self.settings.columns.clamp(2, 4) as i32);
        ui.set_eq_preset(self.settings.eq_preset.min(eq::PRESETS.len() - 1) as i32);
        ui.set_normalizer(self.settings.normalizer);
        ui.set_online_lyrics(self.settings.online_lyrics);
    }

    fn save_settings(&self) {
        write_json(data_path("mobile.json"), &self.settings);
    }

    fn apply_audio_settings(&self) {
        let preset = self.settings.eq_preset.min(eq::PRESETS.len() - 1);
        self.equalizer.update(&eq::PRESETS[preset], 0.0);
        self.equalizer.set_normalizer(self.settings.normalizer);
    }

    fn reload_plex(&self, ui: &MobileApp) {
        let Some(plex) = self.plex.clone() else { return };
        ui.set_connecting(true);
        ui.set_status("Loading your library…".into());
        let (tx, wanted) = (self.tx.clone(), self.settings.libraries.clone());
        thread::spawn(move || {
            let result = plex.albums(&wanted).map(|albums| (plex, albums));
            let _ = tx.send(Event::Library(result));
        });
    }

    /// Requests covers for grid rows `first .. first + count`.
    fn request_art(&mut self, first_row: usize, rows: usize) {
        let wanted: Vec<usize> = self.rows.iter().skip(first_row).take(rows).flatten().copied().collect();
        for index in wanted {
            if self.pending_art.len() >= 6 {
                break;
            }
            if let Some(album) = self.albums.get(index).cloned() {
                self.fetch_art(&album);
            }
        }
    }

    fn fetch_art(&mut self, album: &Entry) {
        let key = album.rating_key.clone();
        if self.art.contains_key(&key) || self.pending_art.contains(&key) {
            return;
        }
        let plex = self.plex.clone();
        let (thumb, path) = (album.thumb.clone(), album.local_path.clone());
        if thumb.is_empty() && path.is_none() {
            return;
        }
        self.pending_art.insert(key.clone());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let art = match (path, plex) {
                #[cfg(not(target_os = "ios"))]
                (Some(path), _) => local::art(&path).and_then(|data| decode(&data)),
                (_, plex) => {
                    // Covers are kept on disk, so they show instantly next time.
                    let file = cover_file(&key);
                    match file.as_ref().and_then(|file| std::fs::read(file).ok()) {
                        Some(bytes) => decode(&bytes),
                        None => match plex {
                            Some(plex) if !thumb.is_empty() => {
                                plex_cover_bytes(&plex, &thumb, COVER_SIZE).and_then(|bytes| {
                                    let art = decode(&bytes);
                                    if art.is_some() {
                                        write_bytes(file, &bytes);
                                    }
                                    art
                                })
                            }
                            _ => None,
                        },
                    }
                }
            };
            let _ = tx.send(Event::Art(key, art));
        });
    }

    fn set_album_art(&self, key: &str, image: Image) {
        if let Some(index) = self.albums.iter().position(|album| album.rating_key == key)
            && let Some(mut row) = self.album_model.row_data(index)
        {
            row.art = image;
            self.album_model.set_row_data(index, row);
        }
    }

    fn store_art(&mut self, ui: &MobileApp, key: String, art: Art) {
        let image = Image::from_rgba8(art);
        // Drop the oldest cover from memory (it reloads from disk when needed).
        if self.art_order.len() >= COVER_CACHE
            && let Some(old) = self.art_order.pop_front()
        {
            self.art.remove(&old);
            self.set_album_art(&old, Image::default());
        }
        self.art_order.push_back(key.clone());
        self.art.insert(key.clone(), image.clone());
        self.set_album_art(&key, image.clone());
        if self.open_album == key {
            ui.set_album_art(image);
        }
        if self.queue.iter().skip(self.current).any(|track| track.parent_rating_key == key) {
            self.publish_now_playing(ui);
            self.publish_queue(ui);
        }
    }

    fn open_album(&mut self, ui: &MobileApp, key: &str) {
        let Some(album) = self.albums.iter().find(|a| a.rating_key == key).cloned() else {
            return;
        };
        self.open_album = key.to_owned();
        ui.set_album_title(album.title.as_str().into());
        ui.set_album_artist(album.parent_title.as_str().into());
        ui.set_album_year(if album.year > 0 { album.year.to_string().into() } else { "".into() });
        ui.set_album_art(self.art.get(key).cloned().unwrap_or_default());
        ui.set_screen(1);
        self.fetch_art(&album);
        if self.tracks.contains_key(key) {
            ui.set_tracks_loading(false);
            self.publish_tracks(ui);
        } else {
            ui.set_tracks(ModelRc::default());
            ui.set_tracks_loading(true);
            let (tx, plex, key) = (self.tx.clone(), self.plex.clone(), key.to_owned());
            thread::spawn(move || {
                let result = plex.context("Plex is disconnected").and_then(|plex| plex.tracks(&album));
                let _ = tx.send(Event::Tracks(key, result));
            });
        }
    }

    fn publish_tracks(&self, ui: &MobileApp) {
        let playing = self.queue.get(self.current).map(|t| t.rating_key.clone());
        let tracks = self.tracks.get(&self.open_album).cloned().unwrap_or_default();
        ui.set_tracks(ModelRc::new(VecModel::from(
            tracks
                .iter()
                .enumerate()
                .map(|(index, track)| TrackItem {
                    number: if track.index > 0 { track.index } else { index as u32 + 1 }.to_string().into(),
                    title: track.title.as_str().into(),
                    time: clock(track.duration / 1000).into(),
                    current: playing.as_deref() == Some(track.rating_key.as_str()),
                })
                .collect::<Vec<_>>(),
        )));
    }

    fn cover_for(&self, track: &Entry) -> Image {
        self.art.get(&track.parent_rating_key).cloned().unwrap_or_default()
    }

    fn publish_queue(&self, ui: &MobileApp) {
        ui.set_queue(ModelRc::new(VecModel::from(
            self.queue
                .iter()
                .enumerate()
                .skip(self.current)
                .map(|(index, track)| QueueItem {
                    title: track.title.as_str().into(),
                    artist: track.grandparent_title.as_str().into(),
                    art: self.cover_for(track),
                    current: index == self.current,
                })
                .collect::<Vec<_>>(),
        )));
    }

    fn publish_now_playing(&self, ui: &MobileApp) {
        let track = self.queue.get(self.current);
        ui.set_has_track(track.is_some());
        if let Some(track) = track {
            ui.set_now_title(track.title.as_str().into());
            ui.set_now_artist(track.grandparent_title.as_str().into());
            ui.set_now_album(track.parent_title.as_str().into());
            ui.set_now_art(self.cover_for(track));
            ui.set_duration((track.duration as f32 / 1000.0).max(1.0));
        }
        ui.set_playing(self.playing);
        self.publish_system();
    }

    /// Updates the lock screen / Control Center info (position, play state, artwork).
    fn publish_system(&self) {
        let Some(track) = self.queue.get(self.current) else {
            ios::now_playing(None);
            return;
        };
        ios::now_playing(Some(&NowPlaying {
            title: track.title.clone(),
            artist: track.grandparent_title.clone(),
            album: track.parent_title.clone(),
            duration: track.duration as f64 / 1000.0,
            elapsed: self.sink.as_ref().map(|s| s.get_pos().as_secs_f64()).unwrap_or(0.0),
            playing: self.playing,
            art_key: track.parent_rating_key.clone(),
        }));
    }

    /// Fetches a full-size cover of the current song's album for the lock screen.
    fn fetch_now_art(&mut self) {
        let Some(track) = self.queue.get(self.current) else { return };
        let key = track.parent_rating_key.clone();
        if ios::artwork_key().as_deref() == Some(key.as_str()) || self.now_art_pending == key {
            return;
        }
        let album = self.albums.iter().find(|a| a.rating_key == key);
        let thumb = [album.map(|a| a.thumb.clone()), Some(track.parent_thumb.clone()), Some(track.thumb.clone())]
            .into_iter()
            .flatten()
            .find(|thumb| !thumb.is_empty());
        let (Some(thumb), Some(plex)) = (thumb, self.plex.clone()) else { return };
        self.now_art_pending.clone_from(&key);
        let tx = self.tx.clone();
        thread::spawn(move || {
            if let Some(bytes) = plex_cover_bytes(&plex, &thumb, NOW_ART_SIZE) {
                let _ = tx.send(Event::NowArt(key, bytes));
            }
        });
    }

    /// Adds songs to the end of the queue (or plays them when nothing is queued).
    fn enqueue(&mut self, ui: &MobileApp, tracks: Vec<Entry>) {
        if tracks.is_empty() {
            return;
        }
        let count = tracks.len();
        if self.queue.is_empty() || self.current >= self.queue.len() {
            self.play_list(ui, tracks, 0);
        } else {
            self.queue.extend(tracks);
            self.shuffled = true; // a mixed queue: level songs by track
            self.publish_queue(ui);
            self.fetch_lyrics(ui);
        }
        ui.set_toast(if count == 1 { "Added to queue".into() } else { format!("Added {count} songs to queue").into() });
    }

    fn play_list(&mut self, ui: &MobileApp, tracks: Vec<Entry>, start: usize) {
        if tracks.is_empty() {
            return;
        }
        self.queue = tracks;
        self.current = start.min(self.queue.len() - 1);
        self.start_current(ui);
    }

    fn start_current(&mut self, ui: &MobileApp) {
        let Some(track) = self.queue.get(self.current).cloned() else {
            self.stop(ui);
            return;
        };
        self.generation += 1;
        if let Some(sink) = &self.sink {
            sink.stop();
        }
        self.sink = None;
        self.playing = true;
        self.last_position = 0.0;
        self.resumes = 0;
        self.resume_at = None;
        ui.set_position(0.0);
        self.publish_now_playing(ui);
        self.publish_queue(ui);
        self.publish_tracks(ui);
        self.fetch_lyrics(ui);
        self.fetch_now_art();
        self.start_scrobble(&track);
        if let Some(album) = self.albums.iter().find(|a| a.rating_key == track.parent_rating_key).cloned() {
            self.fetch_art(&album);
        }
        let (tx, plex, generation) = (self.tx.clone(), self.plex.clone(), self.generation);
        thread::spawn(move || {
            let _ = tx.send(Event::Audio(generation, open_audio(plex, &track)));
        });
    }

    /// Opens the current song's audio again (after a dropout), keeping everything else.
    fn reopen_current(&mut self) {
        let Some(track) = self.queue.get(self.current).cloned() else { return };
        self.generation += 1;
        let (tx, plex, generation) = (self.tx.clone(), self.plex.clone(), self.generation);
        thread::spawn(move || {
            let _ = tx.send(Event::Audio(generation, open_audio(plex, &track)));
        });
    }

    /// Looks up lyrics for the current and next track (each only once).
    fn fetch_lyrics(&mut self, ui: &MobileApp) {
        for offset in 0..2 {
            let Some(track) = self.queue.get(self.current + offset).cloned() else { break };
            let key = track.rating_key.clone();
            if self.lyrics_cache.contains_key(&key) || !self.lyrics_pending.insert(key.clone()) {
                continue;
            }
            let (tx, online) = (self.tx.clone(), self.settings.online_lyrics);
            thread::spawn(move || {
                let _ = tx.send(Event::Lyrics(key, lyrics::find(&track, online)));
            });
        }
        self.present_lyrics(ui);
    }

    fn present_lyrics(&mut self, ui: &MobileApp) {
        let key = self.queue.get(self.current).map(|t| t.rating_key.clone()).unwrap_or_default();
        self.lyrics_key.clone_from(&key);
        let lines = self.lyrics_cache.get(&key).cloned().flatten().unwrap_or_default();
        let slots = lyrics::layout(&lines, self.lyrics_chars);
        ui.set_lyrics_synced(lines.first().is_some_and(|line| line.time.is_some()));
        ui.set_lyrics(ModelRc::new(VecModel::from(
            lines
                .iter()
                .zip(slots)
                .map(|(line, (y, height))| LyricItem {
                    text: line.text.as_str().into(),
                    time: line.time.unwrap_or(-1.0),
                    // The phone uses larger rows than the desktop's 44 px.
                    y: y * 0.8,
                    height: height * 0.8,
                })
                .collect::<Vec<_>>(),
        )));
        self.lyric_index = -1;
        ui.set_lyric_index(-1);
        ui.set_lyrics_status(if self.lyrics_pending.contains(&key) {
            "Looking for lyrics…".into()
        } else {
            "No lyrics found for this song.".into()
        });
    }

    fn stop(&mut self, ui: &MobileApp) {
        if let Some(sink) = &self.sink {
            sink.stop();
        }
        self.sink = None;
        self.playing = false;
        self.publish_now_playing(ui);
    }

    fn toggle(&mut self, ui: &MobileApp) {
        match &self.sink {
            Some(sink) if sink.is_paused() => {
                sink.play();
                self.playing = true;
            }
            Some(sink) => {
                sink.pause();
                self.playing = false;
            }
            None if !self.queue.is_empty() => return self.start_current(ui),
            None => {}
        }
        ui.set_playing(self.playing);
        self.publish_system();
    }

    fn seek(&mut self, seconds: f32) {
        if let Some(sink) = self.sink.clone() {
            // A seek past what's downloaded waits for the stream, so it runs off
            // the UI thread; the app stays responsive meanwhile.
            thread::spawn(move || {
                let _ = sink.try_seek(Duration::from_secs_f32(seconds.max(0.0)));
            });
        }
        self.publish_system();
    }

    fn remote(&mut self, ui: &MobileApp, command: Remote) {
        match command {
            Remote::Play if !self.playing => self.toggle(ui),
            Remote::Pause if self.playing => self.toggle(ui),
            Remote::Play | Remote::Pause => {}
            Remote::Toggle => self.toggle(ui),
            Remote::Next => self.step(ui, true),
            Remote::Previous => self.step(ui, false),
            Remote::Seek(seconds) => {
                self.seek(seconds as f32);
                ui.set_position(seconds as f32);
            }
        }
    }

    fn step(&mut self, ui: &MobileApp, forward: bool) {
        if self.queue.is_empty() {
            return;
        }
        if forward {
            if self.current + 1 >= self.queue.len() {
                self.stop(ui);
                return;
            }
            self.current += 1;
        } else {
            // Restart the song if it's a few seconds in, like most players.
            let position = self.sink.as_ref().map(|s| s.get_pos().as_secs_f32()).unwrap_or(0.0);
            if position < 3.0 && self.current > 0 {
                self.current -= 1;
            }
        }
        self.start_current(ui);
    }

    // ---- Last.fm ----

    fn publish_lastfm(&self, ui: &MobileApp) {
        ui.set_fm_connected(self.fm_session.is_some());
        ui.set_fm_user(self.fm_session.as_ref().map(|s| s.username.as_str()).unwrap_or_default().into());
        if self.fm_session.is_some() {
            ui.set_fm_status(match self.fm_unsent.len() {
                0 => "Scrobbling is on.".into(),
                1 => "1 scrobble waiting to send.".into(),
                n => format!("{n} scrobbles waiting to send.").into(),
            });
        }
    }

    /// A new song started: tell Last.fm and start counting listening time.
    fn start_scrobble(&mut self, track: &Entry) {
        self.fm_started = Some(now_seconds());
        self.fm_listened = Duration::ZERO;
        self.fm_done = false;
        if let Some(session) = self.fm_session.clone() {
            let track = track.clone();
            thread::spawn(move || {
                let _ = session.now_playing(&track);
            });
        }
    }

    /// Scrobbles once the song has played for half its length or 4 minutes (songs
    /// over 30 seconds), right away, so nothing is lost if the app is closed.
    fn check_scrobble(&mut self) {
        let (Some(started_at), false) = (self.fm_started, self.fm_done) else { return };
        let Some(track) = self.queue.get(self.current) else { return };
        if track.duration <= 30_000 || self.fm_listened < Duration::from_millis((track.duration / 2).min(240_000)) {
            return;
        }
        self.fm_done = true;
        if self.fm_session.is_none() {
            return;
        }
        self.fm_unsent.push(Scrobble { track: track.clone(), started_at });
        self.send_scrobbles();
    }

    /// Sends waiting scrobbles in the background; failures stay queued for later.
    fn send_scrobbles(&mut self) {
        let Some(session) = self.fm_session.clone() else { return };
        if self.fm_sending || self.fm_unsent.is_empty() {
            return;
        }
        self.fm_sending = true;
        self.fm_last_retry = Instant::now();
        let batch = std::mem::take(&mut self.fm_unsent);
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut unsent = Vec::new();
            let mut last_error = None;
            for scrobble in batch {
                if let Err(error) = session.scrobble(&scrobble.track, scrobble.started_at) {
                    let message = error.to_string();
                    // Last.fm refusing a track (e.g. missing artist) won't change; drop it.
                    if !message.contains("did not accept") && !message.contains("missing an artist") {
                        unsent.push(scrobble);
                    }
                    last_error = Some(message);
                }
            }
            let _ = tx.send(Event::FmUnsent(unsent, last_error));
        });
    }

    /// Saves the album tracks for the next run (in the background).
    fn save_tracks(&self) {
        let tracks = self.tracks.clone();
        thread::spawn(move || write_json(cache_path("tracks.json"), &tracks));
    }

    fn save_unsent(&self) {
        write_json(data_path("scrobbles.json"), &self.fm_unsent);
    }

    fn receive(&mut self, ui: &MobileApp, event: Event) {
        match event {
            Event::Library(Ok((plex, mut albums))) => {
                ui.set_connecting(false);
                ui.set_signed_in(true);
                ui.set_status(format!("{} albums from Plex.", albums.len()).into());
                sort_albums(&mut albums);
                // Album tracks and loudness in the background: only what isn't saved
                // yet, or everything after a refresh. Saved for the next run.
                let (loader, wanted) = (plex.clone(), self.settings.libraries.clone());
                let all = std::mem::take(&mut self.reload_all_loudness);
                let (list, known, tx) = (albums.clone(), self.tracks.clone(), self.tx.clone());
                thread::spawn(move || {
                    write_json(cache_path("albums.json"), &list);
                    let sender = tx.clone();
                    let _ = loader.sync(&wanted, &list, &known, all, |tracks| {
                        let _ = sender.send(Event::TracksLoaded(tracks, all));
                    });
                    if let Some(path) = cache_path("loudness.json") {
                        plex::save_loudness(&path);
                    }
                });
                self.plex = Some(plex);
                self.albums = albums;
                self.publish_libraries(ui);
                self.publish_albums(ui);
            }
            Event::Library(Err(error)) => {
                ui.set_connecting(false);
                ui.set_status(format!("Plex: {error}").into());
            }
            Event::NoPlex => {
                ui.set_connecting(false);
                ui.set_status("Sign in with Plex below to see your music.".into());
                #[cfg(not(target_os = "ios"))]
                {
                    // Desktop test runs fall back to the local music folder.
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let _ = tx.send(Event::Local(local::scan()));
                    });
                }
            }
            #[cfg(not(target_os = "ios"))]
            Event::Local(Ok(albums)) if self.plex.is_none() && !albums.is_empty() => {
                ui.set_status(format!("{} local albums (desktop test mode).", albums.len()).into());
                self.albums = albums.iter().map(|(album, _)| album.clone()).collect();
                sort_albums(&mut self.albums);
                self.tracks = albums.into_iter().map(|(album, tracks)| (album.rating_key, tracks)).collect();
                self.publish_albums(ui);
            }
            #[cfg(not(target_os = "ios"))]
            Event::Local(_) => {}
            Event::Tracks(key, result) => {
                if key == self.open_album {
                    ui.set_tracks_loading(false);
                }
                match result {
                    Ok(tracks) => {
                        self.tracks.insert(key.clone(), tracks);
                        self.save_tracks();
                        if key == self.open_album {
                            self.publish_tracks(ui);
                        }
                    }
                    Err(error) => ui.set_status(format!("Couldn't load songs: {error}").into()),
                }
            }
            Event::Art(key, art) => {
                self.pending_art.remove(&key);
                if let Some(art) = art {
                    self.store_art(ui, key, art);
                }
            }
            Event::Audio(generation, result) if generation == self.generation => match result {
                Ok((decoder, loudness)) => {
                    if self.stream.is_none() {
                        match OutputStreamBuilder::open_default_stream() {
                            Ok(mut stream) => {
                                stream.log_on_drop(false);
                                self.stream = Some(stream);
                            }
                            Err(error) => {
                                ui.set_status(format!("Audio: {error}").into());
                                return;
                            }
                        }
                    }
                    if let Some(stream) = &self.stream {
                        let sink = Arc::new(Sink::connect_new(stream.mixer()));
                        // Desktop test runs can be silenced; there is no such switch on iOS.
                        #[cfg(not(target_os = "ios"))]
                        if std::env::var_os("SONIC_MUTE").is_some() {
                            sink.set_volume(0.0);
                        }
                        // Album leveling when playing an album in order, track leveling otherwise.
                        let album = !self.shuffled
                            && self.queue.iter().all(|t| {
                                self.queue.first().is_some_and(|first| t.parent_rating_key == first.parent_rating_key)
                            });
                        let level = loudness.map_or(1.0, |l| l.gain(album));
                        ui.set_leveling_note(plex::leveling_note(loudness, album).into());
                        sink.append(eq::Equalizer::new(decoder, self.equalizer.clone()).with_level(level));
                        if let Some(at) = self.resume_at.take() {
                            let resumed = sink.clone();
                            thread::spawn(move || {
                                let _ = resumed.try_seek(Duration::from_secs_f32(at));
                            });
                        }
                        if !self.playing {
                            sink.pause();
                        }
                        self.sink = Some(sink);
                        // The song really starts now; restart the lock screen clock.
                        self.publish_system();
                    }
                }
                Err(error) => {
                    ui.set_status(format!("Couldn't play: {error}").into());
                    self.playing = false;
                    ui.set_playing(false);
                }
            },
            Event::Audio(..) => {}
            Event::Enqueue(result) => match result {
                Ok(tracks) => self.enqueue(ui, tracks),
                Err(error) => ui.set_toast(format!("Couldn't add: {error}").into()),
            },
            Event::TracksLoaded(tracks, replace) => {
                if replace {
                    self.tracks.clear();
                }
                self.tracks.extend(tracks);
                // Fill in the open album only if it was still loading.
                if ui.get_tracks_loading() && self.tracks.contains_key(&self.open_album) {
                    ui.set_tracks_loading(false);
                    self.publish_tracks(ui);
                }
                self.save_tracks();
            }
            Event::NowArt(key, bytes) => {
                if self.now_art_pending == key {
                    self.now_art_pending.clear();
                }
                ios::set_artwork(&key, &bytes);
                self.publish_system();
            }
            Event::Lyrics(key, lines) => {
                self.lyrics_pending.remove(&key);
                self.lyrics_cache.insert(key.clone(), lines);
                if key == self.lyrics_key {
                    self.present_lyrics(ui);
                }
            }
            Event::FmRestored(session) => {
                self.fm_session = session;
                self.publish_lastfm(ui);
                self.send_scrobbles();
            }
            Event::FmPending(generation, result) if generation == self.fm_generation => match result {
                Ok(pending) => {
                    ui.set_fm_status("Approve Sonic on the Last.fm page in Safari, then come back here.".into());
                    let (tx, generation) = (self.tx.clone(), self.fm_generation);
                    thread::spawn(move || {
                        // Wait for approval in Safari (Last.fm says "not yet" until then).
                        let deadline = Instant::now() + Duration::from_secs(300);
                        let result = loop {
                            thread::sleep(Duration::from_secs(3));
                            match pending.finish() {
                                Err(error) if error.to_string().starts_with("Approve") && Instant::now() < deadline => continue,
                                other => break other,
                            }
                        };
                        let _ = tx.send(Event::FmConnected(generation, result));
                    });
                }
                Err(error) => {
                    ui.set_fm_pending(false);
                    ui.set_fm_status(error.to_string().into());
                }
            },
            Event::FmPending(..) => {}
            Event::FmConnected(generation, result) if generation == self.fm_generation => {
                ui.set_fm_pending(false);
                match result {
                    Ok(session) => {
                        let _ = session.save();
                        self.fm_session = Some(session);
                        self.publish_lastfm(ui);
                    }
                    Err(error) => ui.set_fm_status(error.to_string().into()),
                }
            }
            Event::FmConnected(..) => {}
            Event::FmUnsent(mut unsent, error) => {
                self.fm_sending = false;
                // Keep anything queued while sending; cap at a month of heavy listening.
                unsent.append(&mut self.fm_unsent);
                unsent.truncate(2000);
                self.fm_unsent = unsent;
                self.save_unsent();
                self.publish_lastfm(ui);
                if let (Some(error), false) = (error, self.fm_unsent.is_empty()) {
                    ui.set_fm_status(format!("{} Will retry.", error.trim_end_matches('.')).into());
                }
            }
        }
    }

    /// Called a few times a second: progress, listening time, and moving on when a
    /// song ends.
    fn tick(&mut self, ui: &MobileApp) {
        let elapsed = self.last_tick.elapsed();
        self.last_tick = Instant::now();
        if self.fm_last_retry.elapsed() >= SCROBBLE_RETRY {
            self.fm_last_retry = Instant::now();
            self.send_scrobbles();
        }
        // Coming back on screen: redraw everything (iOS skips drawing in the
        // background, so the play button could look stale) and re-check the state.
        let active = ios::is_active();
        if active && !self.was_active {
            if let Some(sink) = &self.sink {
                self.playing = !sink.is_paused() && !sink.empty();
            }
            ui.set_playing(self.playing);
            ui.set_repaint(!ui.get_repaint());
            ui.window().request_redraw();
        }
        self.was_active = active;
        let Some(sink) = &self.sink else { return };
        if sink.empty() {
            if self.playing {
                // Ended well before its length: the stream dropped out. Reopen it
                // where it stopped instead of skipping (a few times per song).
                let length = self.queue.get(self.current).map(|t| t.duration as f32 / 1000.0).unwrap_or(0.0);
                if self.last_position > 1.0 && self.last_position < length - 4.0 && self.resumes < 3 {
                    self.resumes += 1;
                    self.resume_at = Some(self.last_position);
                    self.sink = None;
                    self.reopen_current();
                    return;
                }
                self.step(ui, true);
            }
            return;
        }
        if self.playing && !sink.is_paused() {
            self.fm_listened += elapsed.min(Duration::from_secs(1));
            self.check_scrobble();
        }
        let Some(sink) = &self.sink else { return };
        let position = sink.get_pos().as_secs_f32();
        self.last_position = position;
        ui.set_position(position);
        if ui.get_lyrics_synced() {
            let lines = self.lyrics_cache.get(&self.lyrics_key).cloned().flatten().unwrap_or_default();
            let index = lyrics::current(&lines, position);
            if index != self.lyric_index {
                self.lyric_index = index;
                ui.set_lyric_index(index);
            }
        }
    }
}

fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn decode(data: &[u8]) -> Option<Art> {
    let image = image::load_from_memory(data).ok()?;
    let image = if image.width().max(image.height()) > COVER_SIZE {
        image.resize(COVER_SIZE, COVER_SIZE, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let pixels = image.into_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(pixels.as_raw(), pixels.width(), pixels.height()))
}

/// An album cover from Plex, resized by the server, still encoded.
fn plex_cover_bytes(plex: &Plex, thumb: &str, size: u32) -> Option<Vec<u8>> {
    let size = size.to_string();
    let response = plex
        .request("/photo/:/transcode")
        .ok()?
        .query(&[("url", thumb), ("width", &size), ("height", &size), ("minSize", "0"), ("upscale", "0")])
        .timeout(Duration::from_secs(8))
        .send()
        .ok()?
        .error_for_status()
        .ok()?;
    let mut data = Vec::new();
    response.take(4 * 1024 * 1024).read_to_end(&mut data).ok()?;
    Some(data)
}

/// Opens a song's audio and its loudness (measured ahead of time by Plex or tags).
fn open_audio(
    plex: Option<Plex>,
    track: &Entry,
) -> Result<(Decoder<BufReader<Box<dyn AudioInput>>>, Option<plex::Loudness>)> {
    let loudness = match &track.local_path {
        #[cfg(not(target_os = "ios"))]
        Some(path) => local::loudness(path),
        _ => plex.as_ref().and_then(|plex| plex.loudness(track)),
    };
    let (input, length): (Box<dyn AudioInput>, Option<u64>) = if let Some(path) = &track.local_path {
        let file = std::fs::File::open(path).context("Opening local file")?;
        let length = file.metadata()?.len();
        (Box::new(file), Some(length))
    } else {
        let response = plex.context("Plex is disconnected")?.track_audio(track)?;
        let length = response.content_length();
        let input = stream::BufferedDownload::new(response, length, Arc::new(AtomicBool::new(false)))?;
        (Box::new(input), length)
    };
    let mut decoder = Decoder::builder().with_data(BufReader::new(input)).with_seekable(true);
    if let Some(length) = length {
        decoder = decoder.with_byte_len(length);
    }
    Ok((decoder.build().context("Decoding audio")?, loudness))
}

/// Writes panics to Caches/Sonic/crash.log, to find what went wrong.
fn log_panics() {
    let Some(path) = cache_path("crash.log") else { return };
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        let message = format!(
            "[{}] Sonic {} panicked: {info}\n{}\n",
            now_seconds(),
            env!("CARGO_PKG_VERSION"),
            std::backtrace::Backtrace::force_capture()
        );
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = file.write_all(message.as_bytes());
        }
    }));
}

fn main() -> Result<()> {
    log_panics();
    #[cfg(target_os = "ios")]
    ios::start_audio_session();

    let ui = MobileApp::new()?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let (tx, rx): (Sender<Event>, Receiver<Event>) = channel();
    let album_model = Rc::new(VecModel::default());
    ui.set_albums(ModelRc::from(album_model.clone()));
    let settings: Settings = read_json(data_path("mobile.json")).unwrap_or_default();
    let app = Rc::new(RefCell::new(App {
        tx: tx.clone(),
        settings,
        plex: None,
        albums: Vec::new(),
        rows: Vec::new(),
        query: String::new(),
        tracks: HashMap::new(),
        open_album: String::new(),
        art: HashMap::new(),
        art_order: VecDeque::new(),
        pending_art: HashSet::new(),
        queue: Vec::new(),
        current: 0,
        generation: 0,
        stream: None,
        sink: None,
        playing: false,
        equalizer: Arc::new(eq::Settings::default()),
        album_model,
        lyrics_cache: HashMap::new(),
        lyrics_pending: HashSet::new(),
        lyrics_key: String::new(),
        lyrics_chars: 22,
        lyric_index: -1,
        now_art_pending: String::new(),
        fm_session: None,
        fm_generation: 0,
        fm_started: None,
        fm_listened: Duration::ZERO,
        fm_done: false,
        fm_unsent: read_json(data_path("scrobbles.json")).unwrap_or_default(),
        fm_sending: false,
        fm_last_retry: Instant::now(),
        last_tick: Instant::now(),
        shuffled: false,
        reload_all_loudness: false,
        last_position: 0.0,
        resumes: 0,
        resume_at: None,
        was_active: true,
    }));
    {
        let mut app = app.borrow_mut();
        app.publish_settings(&ui);
        app.apply_audio_settings();
        ui.set_plex_address(app.settings.server_address.as_str().into());
        let _ = plex::set_manual_address(&app.settings.server_address);
        if let Some(path) = cache_path("loudness.json") {
            plex::restore_loudness(&path);
        }
        if let Some(tracks) = read_json::<HashMap<String, Vec<Entry>>>(cache_path("tracks.json")) {
            app.tracks = tracks;
        }
        // Show the last known library straight away while Plex refreshes it.
        if let Some(albums) = read_json::<Vec<Entry>>(cache_path("albums.json")) {
            app.albums = albums;
            sort_albums(&mut app.albums);
            app.publish_albums(&ui);
        }
    }

    macro_rules! on {
        ($setter:ident, |$app:ident, $ui:ident $(, $arg:ident)*| $body:block) => {{
            let (app, weak) = (app.clone(), ui.as_weak());
            ui.$setter(move |$($arg),*| {
                let Some($ui) = weak.upgrade() else { return };
                #[allow(unused_mut)]
                let Ok(mut $app) = app.try_borrow_mut() else { return };
                $body
            });
        }};
    }

    on!(on_open_album, |app, ui, key| { app.open_album(&ui, key.as_str()); });
    on!(on_request_art, |app, ui, first, rows| {
        let _ = &ui;
        app.request_art(first.max(0) as usize, rows.max(1) as usize);
    });
    on!(on_search, |app, ui, query| {
        if app.query != query.as_str() {
            app.query = query.to_string();
            app.publish_rows(&ui);
        }
    });
    on!(on_play_album, |app, ui, shuffle| {
        let mut tracks = app.tracks.get(&app.open_album).cloned().unwrap_or_default();
        if shuffle {
            fastrand::shuffle(&mut tracks);
        }
        app.shuffled = shuffle;
        app.play_list(&ui, tracks, 0);
    });
    on!(on_play_track, |app, ui, index| {
        let tracks = app.tracks.get(&app.open_album).cloned().unwrap_or_default();
        app.shuffled = false;
        app.play_list(&ui, tracks, index.max(0) as usize);
    });
    on!(on_play_queue, |app, ui, row| {
        app.current += row.max(0) as usize;
        app.start_current(&ui);
    });
    on!(on_remove_queue, |app, ui, row| {
        let index = app.current + row.max(0) as usize;
        if index != app.current && index < app.queue.len() {
            app.queue.remove(index);
            app.publish_queue(&ui);
        }
    });
    on!(on_toggle_play, |app, ui| { app.toggle(&ui); });
    on!(on_seek, |app, ui, seconds| {
        app.seek(seconds);
        ui.set_position(seconds);
    });
    on!(on_lyrics_layout, |app, ui, chars| {
        let chars = chars.max(8) as usize;
        if chars != app.lyrics_chars {
            app.lyrics_chars = chars;
            app.present_lyrics(&ui);
        }
    });
    on!(on_next, |app, ui| { app.step(&ui, true); });
    on!(on_previous, |app, ui| { app.step(&ui, false); });
    on!(on_sign_in, |app, ui| {
        ui.set_connecting(true);
        ui.set_status("Approve Sonic in Safari, then come back here.".into());
        let (tx, wanted) = (app.tx.clone(), app.settings.libraries.clone());
        thread::spawn(move || {
            let result = Plex::sign_in().and_then(|plex| Ok((plex.clone(), plex.albums(&wanted)?)));
            let _ = tx.send(Event::Library(result));
        });
    });
    on!(on_sign_out, |app, ui| {
        let _ = Plex::forget();
        app.stop(&ui);
        app.plex = None;
        app.albums.clear();
        app.tracks.clear();
        app.queue.clear();
        for name in ["albums.json", "tracks.json", "loudness.json"] {
            if let Some(path) = cache_path(name) {
                let _ = std::fs::remove_file(path);
            }
        }
        app.publish_albums(&ui);
        app.publish_libraries(&ui);
        app.publish_queue(&ui);
        app.publish_now_playing(&ui);
        ui.set_signed_in(false);
        ui.set_status("Signed out. Sign in with Plex to see your music.".into());
    });
    on!(on_toggle_library, |app, ui, index| {
        let Some(plex) = app.plex.clone() else { return };
        let Some((key, _)) = plex.sections.get(index.max(0) as usize).cloned() else { return };
        let mut chosen = plex.chosen_sections(&app.settings.libraries);
        if let Some(at) = chosen.iter().position(|k| *k == key) {
            if chosen.len() == 1 {
                return;
            }
            chosen.remove(at);
        } else {
            chosen.push(key);
        }
        app.settings.libraries = if chosen.len() == plex.sections.len() { Vec::new() } else { chosen };
        app.save_settings();
        app.publish_libraries(&ui);
        app.reload_plex(&ui);
    });
    on!(on_apply_plex_address, |app, ui, address| {
        if let Err(error) = plex::set_manual_address(&address) {
            ui.set_status(error.to_string().into());
            return;
        }
        app.settings.server_address = address.trim().to_owned();
        app.save_settings();
        ui.set_plex_address(address.trim().into());
        ui.set_connecting(true);
        ui.set_status("Connecting…".into());
        let (tx, plex, wanted) = (app.tx.clone(), app.plex.clone(), app.settings.libraries.clone());
        thread::spawn(move || {
            let event = match plex {
                Some(plex) => Event::Library(
                    plex.refresh_connection().and_then(|plex| Ok((plex.clone(), plex.albums(&wanted)?))),
                ),
                None => match Plex::restore() {
                    Ok(Some(plex)) => Event::Library(plex.albums(&wanted).map(|albums| (plex, albums))),
                    Ok(None) => Event::NoPlex,
                    Err(error) => Event::Library(Err(error)),
                },
            };
            let _ = tx.send(event);
        });
    });
    on!(on_add_album_to_queue, |app, ui, key| {
        if let Some(tracks) = app.tracks.get(key.as_str()).cloned() {
            app.enqueue(&ui, tracks);
            return;
        }
        let Some(album) = app.albums.iter().find(|album| album.rating_key == key.as_str()).cloned() else { return };
        let (tx, plex) = (app.tx.clone(), app.plex.clone());
        thread::spawn(move || {
            let result = plex.context("Plex is disconnected").and_then(|plex| plex.tracks(&album));
            let _ = tx.send(Event::Enqueue(result));
        });
    });
    on!(on_add_track_to_queue, |app, ui, index| {
        let track = app.tracks.get(&app.open_album).and_then(|tracks| tracks.get(index.max(0) as usize)).cloned();
        if let Some(track) = track {
            app.enqueue(&ui, vec![track]);
        }
    });
    on!(on_open_playing_album, |app, ui| {
        let Some(key) = app.queue.get(app.current).map(|track| track.parent_rating_key.clone()) else { return };
        app.open_album(&ui, &key);
    });
    // Pull down on the library: reload albums, songs and loudness from Plex.
    on!(on_refresh_library, |app, ui| {
        if ui.get_connecting() || app.plex.is_none() {
            return;
        }
        app.reload_all_loudness = true;
        app.reload_plex(&ui);
    });
    on!(on_set_columns, |app, ui, columns| {
        app.settings.columns = columns.clamp(2, 4) as u32;
        app.save_settings();
        app.publish_rows(&ui);
    });
    on!(on_set_eq, |app, ui, preset| {
        app.settings.eq_preset = (preset.max(0) as usize).min(eq::PRESETS.len() - 1);
        app.save_settings();
        app.apply_audio_settings();
        app.publish_settings(&ui);
    });
    on!(on_toggle_normalizer, |app, ui| {
        app.settings.normalizer = !app.settings.normalizer;
        app.save_settings();
        app.apply_audio_settings();
        app.publish_settings(&ui);
    });
    on!(on_toggle_online_lyrics, |app, ui| {
        app.settings.online_lyrics = !app.settings.online_lyrics;
        app.save_settings();
        app.publish_settings(&ui);
    });
    on!(on_open_link, |app, ui, url| {
        let _ = (&app, &ui);
        ios::open_url(url.as_str());
    });
    on!(on_fm_connect, |app, ui, key, secret| {
        app.fm_generation += 1;
        ui.set_fm_pending(true);
        ui.set_fm_status("Opening Last.fm in Safari…".into());
        let (tx, generation) = (app.tx.clone(), app.fm_generation);
        let (key, secret) = (key.to_string(), secret.to_string());
        thread::spawn(move || {
            let _ = tx.send(Event::FmPending(generation, lastfm::ClientKeys::authorize(key, secret)));
        });
    });
    on!(on_fm_cancel, |app, ui| {
        app.fm_generation += 1;
        ui.set_fm_pending(false);
        ui.set_fm_status("".into());
    });
    on!(on_fm_disconnect, |app, ui| {
        let _ = lastfm::Session::forget();
        app.fm_generation += 1;
        app.fm_session = None;
        app.fm_unsent.clear();
        app.save_unsent();
        app.publish_lastfm(&ui);
        ui.set_fm_status("".into());
    });

    // Restore the saved Plex and Last.fm sign-ins in the background.
    ui.set_connecting(true);
    {
        let (plex_tx, wanted) = (tx.clone(), app.borrow().settings.libraries.clone());
        thread::spawn(move || {
            let tx = plex_tx;
            let event = match Plex::restore() {
                Ok(Some(plex)) => Event::Library(plex.albums(&wanted).map(|albums| (plex, albums))),
                Ok(None) => Event::NoPlex,
                Err(error) => Event::Library(Err(error)),
            };
            let _ = tx.send(event);
        });
        let tx = tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::FmRestored(lastfm::Session::restore().ok().flatten()));
        });
    }

    let timer = Timer::default();
    {
        let (app, weak) = (app.clone(), ui.as_weak());
        let mut started = false;
        timer.start(TimerMode::Repeated, Duration::from_millis(200), move || {
            let Some(ui) = weak.upgrade() else { return };
            let Ok(mut app) = app.try_borrow_mut() else { return };
            if !started {
                // The app object exists once the event loop runs.
                started = true;
                ios::start_remote_commands();
            }
            ios::open_pending();
            for command in ios::take_commands() {
                app.remote(&ui, command);
            }
            for event in rx.try_iter().take(16) {
                app.receive(&ui, event);
            }
            app.tick(&ui);
        });
    }
    ui.run()?;
    Ok(())
}
