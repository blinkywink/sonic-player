//! Sonic for iPhone: albums, an album's songs, playback with a simple queue, and
//! Plex settings. Shares Sonic's Plex, streaming and local-file code with the
//! desktop app; on a desktop it runs in a phone-sized window for testing.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod credentials;
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
    pub fn open_pending() {}
}
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
    collections::{HashMap, HashSet},
    io::{BufReader, Read, Seek},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::Duration,
};

slint::include_modules!();

/// Covers are shown at most ~170 pt wide (about 510 px on a 3x screen); 360 px keeps
/// them sharp enough while keeping memory small.
const COVER_SIZE: u32 = 360;
const COVER_CACHE: usize = 120;

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
    Audio(u64, Result<Decoder<BufReader<Box<dyn AudioInput>>>>),
    Lyrics(String, Option<Vec<lyrics::Line>>),
    /// Full-size cover (encoded) for the lock screen, by album key.
    NowArt(String, Vec<u8>),
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

/// Lock screen artwork size (the system scales it down as needed).
const NOW_ART_SIZE: u32 = 1024;

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Settings {
    /// Plex music libraries to show; empty shows all.
    libraries: Vec<String>,
}

fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join("Library/Application Support/Sonic/mobile.json"))
}

fn load_settings() -> Settings {
    settings_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &Settings) {
    if let Some(path) = settings_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec(settings) {
            let _ = std::fs::write(path, bytes);
        }
    }
}

struct App {
    tx: Sender<Event>,
    settings: Settings,
    plex: Option<Plex>,
    albums: Vec<Entry>,
    tracks: HashMap<String, Vec<Entry>>,
    open_album: String,
    art: HashMap<String, Image>,
    art_order: Vec<String>,
    pending_art: HashSet<String>,
    queue: Vec<Entry>,
    current: usize,
    generation: u64,
    stream: Option<OutputStream>,
    sink: Option<Sink>,
    playing: bool,
    album_model: Rc<VecModel<AlbumItem>>,
    lyrics_cache: HashMap<String, Option<Vec<lyrics::Line>>>,
    lyrics_pending: HashSet<String>,
    lyrics_key: String,
    lyrics_chars: usize,
    lyric_index: i32,
    /// Album whose full-size lock screen cover is being fetched.
    now_art_pending: String,
}

impl App {
    fn publish_albums(&self, ui: &MobileApp) {
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
        let rows = (0..self.albums.len())
            .step_by(2)
            .map(|first| AlbumRow {
                first: first as i32,
                count: (self.albums.len() - first).min(2) as i32,
            })
            .collect::<Vec<_>>();
        ui.set_album_rows(ModelRc::new(VecModel::from(rows)));
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

    /// Requests covers for album rows `first .. first + count` (two albums per row).
    fn request_art(&mut self, first_row: usize, rows: usize) {
        let start = first_row * 2;
        let end = (start + rows * 2).min(self.albums.len());
        for album in self.albums.get(start..end).unwrap_or_default().to_vec() {
            if self.pending_art.len() >= 4 {
                break;
            }
            self.fetch_art(&album);
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
                (_, Some(plex)) if !thumb.is_empty() => plex_cover(&plex, &thumb),
                _ => None,
            };
            let _ = tx.send(Event::Art(key, art));
        });
    }

    fn store_art(&mut self, ui: &MobileApp, key: String, art: Art) {
        let image = Image::from_rgba8(art);
        if self.art_order.len() >= COVER_CACHE {
            let old = self.art_order.remove(0);
            self.art.remove(&old);
        }
        self.art_order.push(key.clone());
        self.art.insert(key.clone(), image.clone());
        if let Some(index) = self.albums.iter().position(|album| album.rating_key == key)
            && let Some(mut row) = self.album_model.row_data(index)
        {
            row.art = image.clone();
            self.album_model.set_row_data(index, row);
        }
        if self.open_album == key {
            ui.set_album_art(image);
        }
        self.publish_now_playing(ui);
        self.publish_queue(ui);
    }

    fn open_album(&mut self, ui: &MobileApp, key: &str) {
        let Some(album) = self.albums.iter().find(|a| a.rating_key == key).cloned() else {
            return;
        };
        self.open_album = key.to_owned();
        ui.set_album_title(album.title.as_str().into());
        ui.set_album_artist(album.parent_title.as_str().into());
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
                let result = plex
                    .context("Plex is disconnected")
                    .and_then(|plex| plex.tracks(&album));
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
        ui.set_position(0.0);
        self.publish_now_playing(ui);
        self.publish_queue(ui);
        self.publish_tracks(ui);
        self.fetch_lyrics(ui);
        self.fetch_now_art();
        if let Some(album) = self.albums.iter().find(|a| a.rating_key == track.parent_rating_key).cloned() {
            self.fetch_art(&album);
        }
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
            let tx = self.tx.clone();
            thread::spawn(move || {
                let _ = tx.send(Event::Lyrics(key, lyrics::find(&track, true)));
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
        if let Some(sink) = &self.sink {
            let _ = sink.try_seek(Duration::from_secs_f32(seconds.max(0.0)));
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
                self.current = self.queue.len();
                self.stop(ui);
                self.current = self.queue.len() - 1;
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

    fn receive(&mut self, ui: &MobileApp, event: Event) {
        match event {
            Event::Library(Ok((plex, albums))) => {
                ui.set_connecting(false);
                ui.set_signed_in(true);
                ui.set_status(format!("{} albums from Plex.", albums.len()).into());
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
                ui.set_status("Sign in with Plex in Settings to see your music.".into());
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
                Ok(decoder) => {
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
                        let sink = Sink::connect_new(stream.mixer());
                        // Desktop test runs can be silenced; there is no such switch on iOS.
                        #[cfg(not(target_os = "ios"))]
                        if std::env::var_os("SONIC_MUTE").is_some() {
                            sink.set_volume(0.0);
                        }
                        sink.append(decoder);
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
        }
    }

    /// Called a few times a second: progress, and moving on when a song ends.
    fn tick(&mut self, ui: &MobileApp) {
        let Some(sink) = &self.sink else { return };
        if sink.empty() {
            if self.playing {
                self.step(ui, true);
            }
            return;
        }
        let position = sink.get_pos().as_secs_f32();
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

fn plex_cover(plex: &Plex, thumb: &str) -> Option<Art> {
    decode(&plex_cover_bytes(plex, thumb, COVER_SIZE)?)
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

fn open_audio(plex: Option<Plex>, track: &Entry) -> Result<Decoder<BufReader<Box<dyn AudioInput>>>> {
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
    decoder.build().context("Decoding audio")
}

fn main() -> Result<()> {
    #[cfg(target_os = "ios")]
    ios::start_audio_session();

    let ui = MobileApp::new()?;
    let (tx, rx): (Sender<Event>, Receiver<Event>) = channel();
    let album_model = Rc::new(VecModel::default());
    ui.set_albums(ModelRc::from(album_model.clone()));
    let app = Rc::new(RefCell::new(App {
        tx: tx.clone(),
        settings: load_settings(),
        plex: None,
        albums: Vec::new(),
        tracks: HashMap::new(),
        open_album: String::new(),
        art: HashMap::new(),
        art_order: Vec::new(),
        pending_art: HashSet::new(),
        queue: Vec::new(),
        current: 0,
        generation: 0,
        stream: None,
        sink: None,
        playing: false,
        album_model,
        lyrics_cache: HashMap::new(),
        lyrics_pending: HashSet::new(),
        lyrics_key: String::new(),
        lyrics_chars: 22,
        lyric_index: -1,
        now_art_pending: String::new(),
    }));

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
    on!(on_play_album, |app, ui, shuffle| {
        let mut tracks = app.tracks.get(&app.open_album).cloned().unwrap_or_default();
        if shuffle {
            fastrand::shuffle(&mut tracks);
        }
        app.play_list(&ui, tracks, 0);
    });
    on!(on_play_track, |app, ui, index| {
        let tracks = app.tracks.get(&app.open_album).cloned().unwrap_or_default();
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
        let _ = &ui;
        app.seek(seconds);
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
        save_settings(&app.settings);
        app.publish_libraries(&ui);
        app.reload_plex(&ui);
    });

    // Restore the saved Plex sign-in in the background.
    ui.set_connecting(true);
    {
        let (tx, wanted) = (tx.clone(), app.borrow().settings.libraries.clone());
        thread::spawn(move || {
            let event = match Plex::restore() {
                Ok(Some(plex)) => Event::Library(plex.albums(&wanted).map(|albums| (plex, albums))),
                Ok(None) => Event::NoPlex,
                Err(error) => Event::Library(Err(error)),
            };
            let _ = tx.send(event);
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
