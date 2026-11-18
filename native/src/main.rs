#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod credentials;
#[cfg(windows)]
mod desktop;
mod discord;
mod eq;
mod lastfm;
mod library;
mod local;
mod lyrics;
mod media;
mod plex;
mod stream;
mod update;

use anyhow::{Context, Result};
use library::{LibraryData, PlaybackSnapshot, Playlist, Preferences};
use plex::{Entry, Plex};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode,
    VecModel,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

slint::include_modules!();

type Art = SharedPixelBuffer<Rgba8Pixel>;
trait AudioInput: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> AudioInput for T {}
struct Audio {
    decoder: Decoder<std::io::BufReader<Box<dyn AudioInput>>>,
    download: Option<stream::DownloadStatus>,
}

enum Event {
    Library(Result<(Plex, Vec<Entry>)>),
    NoSavedPlex,
    PlexConnection(Result<Plex>),
    LocalLibrary(u64, Result<Vec<(Entry, Vec<Entry>)>>),
    FmPending(u64, Result<lastfm::Pending>),
    FmConnected(u64, Result<lastfm::Session>),
    FmError(u64, String),
    FmTest(u64, Result<()>),
    Tracks(u64, String, Result<Vec<Entry>>),
    AlbumAction(u64, String, i32, i32, Result<Vec<Entry>>),
    Artwork(u64, String, Option<Art>),
    PlayingArt(u64, Option<Art>),
    Audio(u64, Result<Audio>),
    SeekFinished(u64, Option<String>),
    Prefetched(u64, Result<Audio>),
    Lyrics(String, Option<Vec<lyrics::Line>>),
}

struct Prefetch {
    id: u64,
    key: String,
    cancel: Arc<AtomicBool>,
    audio: Option<Audio>,
    playback_generation: Option<u64>,
}

struct Handoff {
    track: Entry,
    started: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    download: Option<stream::DownloadStatus>,
    duration: Option<Duration>,
    skip_requested: bool,
}

struct Player {
    sink: Arc<Sink>,
    download: Option<stream::DownloadStatus>,
    _stream: OutputStream,
}

struct State {
    /// Track and position restored from the last session; used once, on its first play.
    resume: Option<(String, f32)>,
    /// Page shown on the previous tick, to refresh views when the user arrives on them.
    last_page: i32,
    /// Playlist cover mosaics, keyed by the album art keys they were built from.
    mosaics: RefCell<HashMap<Vec<String>, Image>>,
    theme_fade: ThemeFade,
    /// Rating key of the track whose lyrics are shown, its lines and the highlighted row.
    lyrics_key: String,
    lyrics: Vec<lyrics::Line>,
    lyrics_cache: HashMap<String, Option<Vec<lyrics::Line>>>,
    /// Lookups in flight, so the current and the preloaded next track are fetched once.
    lyrics_pending: HashSet<String>,
    lyrics_chars: usize,
    lyric_index: i32,
    plex: Option<Plex>,
    data: LibraryData,
    equalizer: Arc<eq::Settings>,
    eq_gains: [f32; 20],
    eq_preamp: f32,
    can_save: bool,
    art_theme: Option<[u8; 3]>,
    fm_generation: u64,
    fm_busy: bool,
    fm_pending: Option<lastfm::Pending>,
    fm_session: Option<lastfm::Session>,
    listened: Duration,
    started_at: Option<u64>,
    last_tick: Instant,
    last_save: Instant,
    playback_restored: bool,
    was_playing: bool,
    query: String,
    albums: Vec<Entry>,
    plex_albums: Vec<Entry>,
    local_albums: Vec<(Entry, Vec<Entry>)>,
    local_scan: u64,
    album_sort: i32,
    album_group: i32,
    album_columns: usize,
    album_rows: Rc<VecModel<AlbumRow>>,
    local_only: bool,
    loaded_tracks: HashMap<String, Vec<Entry>>,
    tracks_pending: HashSet<String>,
    songs: Rc<VecModel<SongHit>>,
    lookup: HashMap<String, usize>,
    visible: Vec<usize>,
    album_model: Rc<VecModel<Album>>,
    artwork: HashMap<String, Image>,
    pending_art: HashSet<String>,
    cache_generation: u64,
    missing_art: HashSet<String>,
    track_art_changed: bool,
    tracks: Vec<Entry>,
    queue: Vec<Entry>,
    current: usize,
    original_order: Vec<String>,
    history: Vec<Entry>,
    history_cursor: Option<usize>,
    shuffled: bool,
    repeat_mode: i32,
    loading: bool,
    muted: bool,
    generation: u64,
    seeking: bool,
    pending_seek: Option<Duration>,
    cancel_audio: Arc<AtomicBool>,
    prefetch: Option<Prefetch>,
    prefetch_id: u64,
    handoff: Option<Handoff>,
    player: Option<Player>,
    volume: f32,
}

impl State {
    fn filter(&mut self, query: &str) {
        let query = query.trim().to_lowercase();
        self.query = query.clone();
        let parts = query
            .split(search_separator)
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        let mut songs = Vec::new();
        let mut visible = Vec::new();
        for (index, album) in self.albums.iter().enumerate() {
            if self.local_only && !album.rating_key.starts_with("local-album:") {
                continue;
            }
            let album_description = format!("{} {}", album.title, album.parent_title);
            if parts.is_empty()
                || library_matches(&parts, &format!("{album_description} {}", album.year))
            {
                visible.push(index);
            }
            if !parts.is_empty()
                && let Some(tracks) = self.loaded_tracks.get(&album.rating_key)
            {
                for (track_index, track) in tracks.iter().enumerate() {
                    if library_matches(&parts, &format!("{} {album_description}", track.title)) {
                        songs.push(SongHit {
                            album_key: album.rating_key.as_str().into(),
                            track_index: track_index as i32,
                            number: if track.index > 0 {
                                track.index as i32
                            } else {
                                track_index as i32 + 1
                            },
                            title: track.title.as_str().into(),
                            description: format!("{} · {}", album.parent_title, album.title).into(),
                            time: clock(track.duration / 1000).into(),
                            art: self
                                .artwork
                                .get(&album.rating_key)
                                .cloned()
                                .unwrap_or_default(),
                        });
                    }
                }
            }
        }
        self.songs.set_vec(songs);
        self.visible = visible;
        self.visible.sort_by_cached_key(|&index| {
            let album = &self.albums[index];
            match self.album_sort {
                2 | 3 => (
                    0,
                    album.parent_title.to_lowercase(),
                    album.title.to_lowercase(),
                ),
                4 | 5 => (
                    album.year,
                    album.title.to_lowercase(),
                    album.parent_title.to_lowercase(),
                ),
                _ => (
                    0,
                    album.title.to_lowercase(),
                    album.parent_title.to_lowercase(),
                ),
            }
        });
        if matches!(self.album_sort, 1 | 3 | 4) {
            self.visible.reverse();
        }
        match self.album_group {
            1 => self
                .visible
                .sort_by_cached_key(|&index| self.albums[index].parent_title.to_lowercase()),
            2 => self
                .visible
                .sort_by_key(|&index| std::cmp::Reverse(self.albums[index].year)),
            _ => {}
        }
        self.rebuild_album_rows();
        self.album_model.set_vec(
            self.visible
                .iter()
                .map(|&index| {
                    let album = &self.albums[index];
                    Album {
                        key: album.rating_key.as_str().into(),
                        title: album.title.as_str().into(),
                        artist: album.parent_title.as_str().into(),
                        year: if album.year > 0 {
                            album.year.to_string().into()
                        } else {
                            "".into()
                        },
                        art: self
                            .artwork
                            .get(&album.rating_key)
                            .cloned()
                            .unwrap_or_default(),
                    }
                })
                .collect::<Vec<_>>(),
        );
    }

    fn publish_plex_libraries(&self, ui: &Sonic) {
        let Some(plex) = &self.plex else {
            ui.set_plex_libraries(ModelRc::default());
            return;
        };
        let chosen = plex.chosen_sections(&self.data.preferences.plex_libraries);
        ui.set_plex_libraries(ModelRc::new(VecModel::from(
            plex.sections
                .iter()
                .map(|(key, title)| PlexLibrary {
                    title: if title.is_empty() {
                        "Music".into()
                    } else {
                        title.as_str().into()
                    },
                    selected: chosen.contains(key),
                })
                .collect::<Vec<_>>(),
        )));
    }

    fn apply_plex_connection(&mut self, ui: &Sonic, plex: Plex) {
        ui.set_connection_type(if plex.is_local() {
            "Local network".into()
        } else {
            "Other network".into()
        });
        ui.set_streaming_endpoint(plex.server().to_string().trim_end_matches('/').into());
        self.plex = Some(plex);
        ui.set_connected(true);
    }

    fn invalidate_library_cache(&mut self, ui: &Sonic, tx: &Sender<Event>) {
        self.cache_generation += 1;
        self.tracks_pending.clear();
        self.loaded_tracks
            .retain(|key, _| key.starts_with("local-album:"));
        self.pending_art.clear();
        self.missing_art.clear();
        self.artwork.clear();
        let query = self.query.clone();
        self.filter(&query);
        ui.set_selected_art(Image::default());
        self.publish_queue(ui);
        self.track_art_changed = true;
        self.fetch_tracks(ui.get_selected_key().as_str(), tx);
    }

    fn fetch_tracks(&mut self, key: &str, tx: &Sender<Event>) {
        if self.loaded_tracks.contains_key(key) || self.tracks_pending.contains(key) {
            return;
        }
        let Some(plex) = self.plex.clone() else {
            return;
        };
        let Some(&index) = self.lookup.get(key) else {
            return;
        };
        let album = self.albums[index].clone();
        self.tracks_pending.insert(key.to_owned());
        let generation = self.cache_generation;
        let tx = tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Tracks(
                generation,
                album.rating_key.clone(),
                plex.tracks(&album),
            ));
        });
    }

    fn rebuild_album_rows(&self) {
        let mut rows = Vec::new();
        let mut offset = 0;
        while offset < self.visible.len() {
            let group_label = |index: usize| match self.album_group {
                1 => {
                    let artist = &self.albums[self.visible[index]].parent_title;
                    if artist.is_empty() {
                        "Unknown artist".into()
                    } else {
                        artist.clone()
                    }
                }
                2 => {
                    let year = self.albums[self.visible[index]].year;
                    if year == 0 {
                        "Unknown year".into()
                    } else {
                        year.to_string()
                    }
                }
                _ => String::new(),
            };
            let label = group_label(offset);
            let mut end = offset + 1;
            while end < self.visible.len() && group_label(end).eq_ignore_ascii_case(&label) {
                end += 1;
            }
            for start in (offset..end).step_by(self.album_columns) {
                let row_end = end.min(start + self.album_columns);
                rows.push(AlbumRow {
                    offset: start as i32,
                    count: (row_end - start) as i32,
                    has_year: self.visible[start..row_end]
                        .iter()
                        .any(|&index| self.albums[index].year > 0),
                    heading: if start == offset {
                        label.to_uppercase().into()
                    } else {
                        "".into()
                    },
                });
            }
            offset = end;
        }
        self.album_rows.set_vec(rows);
    }

    fn rebuild_library(&mut self, ui: &Sonic) {
        self.albums = self.plex_albums.clone();
        self.albums
            .extend(self.local_albums.iter().map(|(album, _)| album.clone()));
        self.lookup = self
            .albums
            .iter()
            .enumerate()
            .map(|(i, album)| (album.rating_key.clone(), i))
            .collect();
        let query = self.query.clone();
        self.filter(&query);
        ui.set_local_count(
            self.local_albums
                .iter()
                .map(|(_, tracks)| tracks.len() as i32)
                .sum(),
        );
    }

    fn scan_local(&mut self, ui: &Sonic, tx: &Sender<Event>) {
        self.local_scan += 1;
        let generation = self.local_scan;
        let tx = tx.clone();
        ui.set_local_status("Scanning music files…".into());
        thread::spawn(move || {
            let _ = tx.send(Event::LocalLibrary(generation, local::scan()));
        });
    }

    fn present_tracks(&mut self, ui: &Sonic, tracks: Vec<Entry>) {
        ui.set_tracks(track_model(&tracks));
        self.tracks = tracks;
    }

    fn publish_eq(&self, ui: &Sonic) {
        self.equalizer.update(&self.eq_gains, self.eq_preamp);
        ui.set_eq_gains(ModelRc::new(VecModel::from(self.eq_gains.to_vec())));
        ui.set_eq_preamp(self.eq_preamp);
        self.update_eq_curve(ui);
    }

    fn update_eq_curve(&self, ui: &Sonic) {
        let (line, area) = eq::curve(&self.eq_gains);
        ui.set_eq_line(line.into());
        ui.set_eq_area(area.into());
    }

    fn publish_playlists(&self, ui: &Sonic) {
        let names = self
            .data
            .playlists
            .iter()
            .map(|list| list.name.as_str().into())
            .collect::<Vec<_>>();
        ui.set_playlists(ModelRc::new(VecModel::from(names)));
        let cards = self
            .data
            .playlists
            .iter()
            .map(|playlist| {
                let artwork = if playlist.cover.is_empty() {
                    self.playlist_mosaic(playlist)
                } else {
                    decode_cover(&playlist.cover).map(Image::from_rgba8)
                };
                PlaylistCard {
                    name: playlist.name.as_str().into(),
                    count: playlist.items.len() as i32,
                    has_art: artwork.is_some(),
                    artwork: artwork.unwrap_or_default(),
                }
            })
            .collect::<Vec<_>>();
        ui.set_playlist_cards(ModelRc::new(VecModel::from(cards)));

        let last = self.data.playlists.len() as i32 - 1;
        let target = if ui.get_playlist_target() < 0 && last >= 0 {
            0
        } else {
            ui.get_playlist_target().min(last)
        };
        ui.set_playlist_target(target);
        self.select_playlist(ui, ui.get_playlist_current().min(last));
    }

    /// Up to four tracks from different albums, in playlist order, for its cover mosaic.
    fn mosaic_sources(playlist: &Playlist) -> Vec<&Entry> {
        let mut seen = HashSet::new();
        playlist
            .items
            .iter()
            .filter(|track| seen.insert(track_art_key(track)))
            .take(4)
            .collect()
    }

    /// A 2x2 cover built from the playlist's albums once their art has loaded.
    fn playlist_mosaic(&self, playlist: &Playlist) -> Option<Image> {
        let keys: Vec<String> = Self::mosaic_sources(playlist)
            .into_iter()
            .map(track_art_key)
            .collect();
        if keys.is_empty() {
            return None;
        }
        if let Some(image) = self.mosaics.borrow().get(&keys) {
            return Some(image.clone());
        }
        let covers: Vec<&Image> = keys
            .iter()
            .filter_map(|key| self.artwork.get(key))
            .collect();
        if covers.len() < keys.len() {
            return None;
        }
        let image = mosaic(&covers)?;
        self.mosaics.borrow_mut().insert(keys, image.clone());
        Some(image)
    }

    fn select_playlist(&self, ui: &Sonic, row: i32) {
        ui.set_playlist_current(row);
        if let Some(list) = self.data.playlists.get(row as usize) {
            ui.set_playlist_name(list.name.as_str().into());
            let cover = if list.cover.is_empty() {
                self.playlist_mosaic(list)
            } else {
                decode_cover(&list.cover).map(Image::from_rgba8)
            };
            ui.set_playlist_has_art(cover.is_some());
            ui.set_playlist_cover(cover.unwrap_or_default());
            ui.set_playlist_items(ModelRc::new(VecModel::from(
                list.items
                    .iter()
                    .map(|entry| self.queue_track(entry))
                    .collect::<Vec<_>>(),
            )));
        } else {
            ui.set_playlist_name("".into());
            ui.set_playlist_cover(Image::default());
            ui.set_playlist_has_art(false);
            ui.set_playlist_items(ModelRc::default());
        }
    }

    fn save(&mut self, ui: &Sonic, message: &str) {
        self.capture_preferences(ui);
        if !self.can_save {
            ui.set_status(
                "Saved data could not be read. Changes are only available this session.".into(),
            );
        } else {
            match library::save(&self.data) {
                Ok(()) => ui.set_status(message.into()),
                Err(error) => ui.set_status(format!("Could not save: {error}").into()),
            }
        }
    }

    fn capture_preferences(&mut self, ui: &Sonic) {
        self.data.preferences = Preferences {
            theme: ui.get_theme_value().to_string(),
            text_mode: ui.get_text_mode(),
            auto_art_theme: ui.get_auto_art_theme(),
            sidebar: ui.get_sidebar_width(),
            track_panel: ui.get_track_panel_width(),
            lock_layout: ui.get_layout_locked(),
            list_view: ui.get_list_mode(),
            local_enabled: ui.get_local_enabled(),
            volume: self.volume,
            muted: self.muted,
            shuffled: self.shuffled,
            repeat_mode: self.repeat_mode,
            eq_gains: self.eq_gains,
            eq_preamp: self.eq_preamp,
            eq_preset: ui.get_eq_preset_index(),
            normalizer: ui.get_normalizer_enabled(),
            online_lyrics: ui.get_online_lyrics(),
            plex_libraries: self.data.preferences.plex_libraries.clone(),
            font: ui.get_font_index(),
            discord_id: ui.get_discord_id().trim().to_owned(),
            discord_image: ui.get_discord_image().trim().to_owned(),
            streaming_local: ui.get_streaming_local(),
            streaming_remote: ui.get_streaming_remote(),
        };
    }

    fn restore_playback(&mut self, ui: &Sonic) {
        if self.playback_restored || !self.queue.is_empty() {
            return;
        }
        let Some(saved) = &self.data.playback else {
            return;
        };
        if saved.queue.is_empty()
            || saved
                .server_id
                .as_ref()
                .is_some_and(|id| !self.plex.as_ref().is_some_and(|plex| plex.server_id == *id))
            || (saved.server_id.is_none()
                && saved.queue.iter().any(|track| track.local_path.is_none()))
        {
            return;
        }
        let saved_position = saved.position;
        self.playback_restored = true;
        self.current = saved.current.min(saved.queue.len() - 1);
        self.queue.clone_from(&saved.queue);
        self.original_order.clone_from(&saved.original_order);
        self.publish_queue(ui);
        self.present_queued_track(ui);
        if let Some(track) = self.queue.get(self.current)
            && saved_position > 1.0
            && saved_position < track.duration as f32 / 1000.0 - 1.0
        {
            self.resume = Some((track.rating_key.clone(), saved_position));
            ui.set_position(saved_position);
            ui.set_elapsed(clock(saved_position as u64).into());
        }
    }

    /// Follows the current track: fetches its lyrics once and keeps the sung line highlighted.
    fn sync_lyrics(&mut self, ui: &Sonic, tx: &Sender<Event>) {
        let track = self.queue.get(self.current);
        let key = track.map(|t| t.rating_key.clone()).unwrap_or_default();
        if key != self.lyrics_key {
            self.lyrics_key.clone_from(&key);
            if self.lyrics_cache.contains_key(&key) || key.is_empty() {
                self.present_lyrics(ui);
            } else if let Some(track) = track.cloned() {
                self.lyrics.clear();
                ui.set_lyrics(ModelRc::default());
                ui.set_lyrics_status("Looking for lyrics…".into());
                ui.set_lyrics_loading(true);
                self.fetch_lyrics(ui, tx, track);
            }
        }
        // Preload the next track's lyrics so they're ready the moment it starts.
        if !self.lyrics_pending.is_empty() {
            return self.highlight_lyric(ui);
        }
        if let Some(next) = self.queue.get(self.current + 1).cloned()
            && !self.lyrics_cache.contains_key(&next.rating_key)
        {
            self.fetch_lyrics(ui, tx, next);
        }
        self.highlight_lyric(ui);
    }

    fn fetch_lyrics(&mut self, ui: &Sonic, tx: &Sender<Event>, track: Entry) {
        if !self.lyrics_pending.insert(track.rating_key.clone()) {
            return;
        }
        let (tx, online) = (tx.clone(), ui.get_online_lyrics());
        thread::spawn(move || {
            let _ = tx.send(Event::Lyrics(
                track.rating_key.clone(),
                lyrics::find(&track, online),
            ));
        });
    }

    fn highlight_lyric(&mut self, ui: &Sonic) {
        if ui.get_lyrics_synced() {
            let index = lyrics::current(&self.lyrics, ui.get_position());
            if index != self.lyric_index {
                self.lyric_index = index;
                ui.set_lyric_index(index);
            }
        }
    }

    fn present_lyrics(&mut self, ui: &Sonic) {
        let found = self.lyrics_cache.get(&self.lyrics_key).cloned().flatten();
        self.lyrics = found.unwrap_or_default();
        let synced = self.lyrics.first().is_some_and(|line| line.time.is_some());
        let slots = lyrics::layout(&self.lyrics, self.lyrics_chars);
        ui.set_lyrics(ModelRc::new(VecModel::from(
            self.lyrics
                .iter()
                .zip(slots)
                .map(|(line, (y, height))| LyricLine {
                    text: line.text.as_str().into(),
                    time: line.time.unwrap_or(-1.0),
                    y,
                    height,
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_lyrics_synced(synced);
        ui.set_lyrics_loading(false);
        self.lyric_index = -1;
        ui.set_lyric_index(-1);
        ui.set_lyrics_status(if self.lyrics_key.is_empty() || !self.lyrics.is_empty() {
            "".into()
        } else if ui.get_online_lyrics() {
            "No lyrics found for this track.".into()
        } else {
            "No lyrics found. Online lookup is off in Settings.".into()
        });
    }

    /// Keeps the current track (playing or paused) and removes everything else.
    fn clear_up_next(&mut self, ui: &Sonic) {
        let track = self.queue[self.current].clone();
        if !self.original_order.is_empty() {
            self.original_order = vec![track.rating_key.clone()];
        }
        self.queue = vec![track];
        self.current = 0;
        // publish_queue also cancels a preloaded hand-off to the removed next track.
        self.publish_queue(ui);
        ui.set_status("Up next cleared".into());
    }

    fn present_queued_track(&self, ui: &Sonic) {
        if let Some(track) = self.queue.get(self.current) {
            ui.set_track_key(track.rating_key.as_str().into());
            ui.set_track_title(track.title.as_str().into());
            ui.set_track_artist(track.grandparent_title.as_str().into());
            ui.set_duration(track.duration as f32 / 1000.0);
            ui.set_total(clock(track.duration / 1000).into());
            if let Some(art) = self
                .artwork
                .get(&track_art_key(track))
                .or_else(|| self.artwork.get(&track.parent_rating_key))
            {
                ui.set_playing_art(art.clone());
            }
        }
    }

    fn flush_session(&mut self, ui: &Sonic, exact: bool) -> Result<()> {
        if !self.can_save {
            return Ok(());
        }
        let previous = self.data.preferences.clone();
        self.capture_preferences(ui);
        let playback = if self.queue.is_empty() {
            if self.playback_restored {
                None
            } else {
                self.data.playback.clone()
            }
        } else {
            let server_id = if self.queue.iter().any(|track| track.local_path.is_none()) {
                let Some(plex) = &self.plex else {
                    return Ok(());
                };
                Some(plex.server_id.clone())
            } else {
                None
            };
            // Not started since reopening: keep the saved position.
            let started = self
                .player
                .as_ref()
                .is_some_and(|player| !player.sink.empty());
            let position = match &self.resume {
                Some((_, position)) if !started => *position,
                _ if exact => ui.get_position(),
                // Rounded while running so the file isn't rewritten every couple of seconds.
                _ => (ui.get_position() / 5.0).floor() * 5.0,
            };
            Some(PlaybackSnapshot {
                queue: self.queue.clone(),
                current: self.current,
                server_id,
                original_order: self.original_order.clone(),
                position,
            })
        };
        if self.data.playback != playback || previous != self.data.preferences {
            let old = std::mem::replace(&mut self.data.playback, playback);
            if let Err(error) = library::save(&self.data) {
                self.data.playback = old;
                self.data.preferences = previous;
                return Err(error);
            }
        }
        Ok(())
    }

    fn add_to_playlist(&mut self, ui: &Sonic, list: usize, track: i32) {
        let tracks = if track < 0 {
            &self.tracks[..]
        } else {
            let Some(track) = self.tracks.get(track as usize) else {
                return;
            };
            std::slice::from_ref(track)
        };
        self.save_tracks_to_playlist(ui, list, &tracks.to_vec());
    }

    fn save_tracks_to_playlist(&mut self, ui: &Sonic, list: usize, tracks: &[Entry]) {
        let Some(playlist) = self.data.playlists.get_mut(list) else {
            ui.set_status("Select a playlist first.".into());
            return;
        };
        if tracks.is_empty() {
            return;
        }
        if tracks.iter().any(|track| track.local_path.is_none()) {
            let Some(plex) = &self.plex else { return };
            if playlist
                .server_id
                .as_ref()
                .is_some_and(|id| *id != plex.server_id)
            {
                ui.set_status("That playlist belongs to a different Plex server.".into());
                return;
            }
            playlist.server_id = Some(plex.server_id.clone());
        }
        let mut present: HashSet<_> = playlist
            .items
            .iter()
            .map(|t| t.rating_key.clone())
            .collect();
        let added = tracks
            .iter()
            .filter(|t| present.insert(t.rating_key.clone()))
            .cloned()
            .collect::<Vec<_>>();
        if added.is_empty() {
            ui.set_status("Already in this playlist.".into());
            return;
        }
        playlist.items.extend(added);
        let message = format!("Added to {}", playlist.name);
        self.publish_playlists(ui);
        self.save(ui, &message);
    }

    fn refresh_theme(&self, ui: &Sonic) {
        let seed = parse_theme(ui.get_theme_value().as_str()).unwrap_or([15, 23, 42]);
        let bg = if ui.get_auto_art_theme() {
            self.art_theme.unwrap_or(seed)
        } else {
            seed
        };
        let theme = theme_colors(bg, ui.get_text_mode());
        ui.set_theme_swatch(color(seed));
        let target = [
            bg,
            theme.surface,
            theme.border,
            theme.hover,
            theme.selected,
            theme.text,
            theme.muted,
            theme.accent,
            theme.accent_text,
            theme.accent_ink,
        ];
        let fade = &self.theme_fade;
        let Some(from) = fade.displayed.get() else {
            apply_palette(ui, target);
            fade.displayed.set(Some(target));
            return;
        };
        if from == target {
            return;
        }
        // Blend from whatever is on screen (even mid-fade) to the new colors.
        let (weak, displayed, timer) = (
            ui.as_weak(),
            fade.displayed.clone(),
            Rc::downgrade(&fade.timer),
        );
        let start = Instant::now();
        fade.timer
            .start(TimerMode::Repeated, Duration::from_millis(16), move || {
                let t = (start.elapsed().as_secs_f32() / 0.45).min(1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                let mix: PaletteColors = std::array::from_fn(|row| {
                    std::array::from_fn(|channel| {
                        let (a, b) = (
                            f32::from(from[row][channel]),
                            f32::from(target[row][channel]),
                        );
                        (a + (b - a) * eased).round() as u8
                    })
                });
                if let Some(ui) = weak.upgrade() {
                    apply_palette(&ui, mix);
                }
                displayed.set(Some(mix));
                if t >= 1.0
                    && let Some(timer) = timer.upgrade()
                {
                    timer.stop();
                }
            });
    }

    fn use_tracks(
        &mut self,
        ui: &Sonic,
        tx: &Sender<Event>,
        mut tracks: Vec<Entry>,
        action: i32,
        target: i32,
    ) {
        if tracks.is_empty() {
            return;
        }
        match action {
            0 => self.load_queue(ui, tx, tracks, 0),
            1 => {
                let was_empty = self.queue.is_empty();
                if !self.original_order.is_empty() {
                    self.original_order
                        .extend(tracks.iter().map(|track| track.rating_key.clone()));
                }
                self.queue.extend(tracks);
                if was_empty {
                    // Like the original, the first queued track is loaded (paused) in the player.
                    self.current = 0;
                    self.present_queued_track(ui);
                }
                self.publish_queue(ui);
                ui.set_status("Added to queue".into());
            }
            2 => {
                fastrand::shuffle(&mut tracks);
                self.splice_play(ui, tx, tracks, 0);
            }
            3 if target >= 0 => self.save_tracks_to_playlist(ui, target as usize, &tracks),
            _ => {}
        }
    }

    fn refresh_track_art(&mut self, ui: &Sonic, tx: &Sender<Event>) {
        if self.track_art_changed && ui.get_dragged_track() < 0 && ui.get_playlist_drag() < 0 {
            self.track_art_changed = false;
            if ui.get_page() == 3 && ui.get_playlist_current() >= 0 {
                self.select_playlist(ui, ui.get_playlist_current());
            } else if ui.get_page() == 3 {
                // New album art may complete a playlist's cover mosaic.
                self.publish_playlists(ui);
            }
            if !self.queue.is_empty() {
                self.publish_queue(ui);
            }
        }
        // The sidebar mini queue is visible on every page while no album is open.
        let sidebar_queue = ui.get_selected_key().is_empty() && !self.queue.is_empty();
        if self.pending_art.len() >= 4 || !(matches!(ui.get_page(), 2 | 3) || sidebar_queue) {
            return;
        }
        let reachable = |playlist: &Playlist| {
            !playlist
                .server_id
                .as_ref()
                .is_some_and(|id| !self.plex.as_ref().is_some_and(|p| p.server_id == *id))
        };
        let tracks: Vec<Entry> = if ui.get_page() == 3 && ui.get_playlist_current() < 0 {
            // Playlists grid: art for each playlist's cover mosaic, then the mini queue.
            self.data
                .playlists
                .iter()
                .filter(|playlist| playlist.cover.is_empty() && reachable(playlist))
                .flat_map(Self::mosaic_sources)
                .chain(self.queue.get(self.current..).unwrap_or_default())
                .take(50)
                .cloned()
                .collect()
        } else if ui.get_page() != 3 {
            self.queue
                .get(self.current..)
                .unwrap_or_default()
                .iter()
                .take(50)
                .cloned()
                .collect()
        } else if let Some(playlist) = self.data.playlists.get(ui.get_playlist_current() as usize) {
            if !reachable(playlist) {
                return;
            }
            playlist.items.iter().take(50).cloned().collect()
        } else {
            return;
        };
        for track in &tracks {
            if self.pending_art.len() >= 4 {
                break;
            }
            let key = track_art_key(track);
            if self.artwork.contains_key(&key)
                || self.pending_art.contains(&key)
                || self.missing_art.contains(&key)
            {
                continue;
            }
            let thumb = if track.parent_thumb.is_empty() {
                &track.thumb
            } else {
                &track.parent_thumb
            };
            if thumb.is_empty() {
                continue;
            }
            let (path, plex, thumb, tx) = (
                track.local_path.clone(),
                self.plex.clone(),
                thumb.clone(),
                tx.clone(),
            );
            if path.is_none() && plex.is_none() {
                continue;
            }
            self.pending_art.insert(key.clone());
            let generation = self.cache_generation;
            thread::spawn(move || {
                let art = match path {
                    Some(path) => local::art(&path).and_then(|bytes| decode_cover(&bytes)),
                    None => plex.and_then(|p| cover(&p, &thumb)),
                };
                let _ = tx.send(Event::Artwork(generation, key, art));
            });
        }
    }

    fn queue_track(&self, track: &Entry) -> QueueTrack {
        let key = track_art_key(track);
        let art = self
            .artwork
            .get(&key)
            .or_else(|| self.artwork.get(&track.parent_rating_key));
        QueueTrack {
            title: track.title.as_str().into(),
            artist: track.grandparent_title.as_str().into(),
            album: track.parent_title.as_str().into(),
            album_key: key.into(),
            time: clock(track.duration / 1000).into(),
            number: track.index as i32,
            art: art.cloned().unwrap_or_default(),
        }
    }

    fn publish_queue(&self, ui: &Sonic) {
        if let Some(handoff) = &self.handoff
            && !self
                .next_index(true)
                .is_some_and(|index| self.queue[index] == handoff.track)
        {
            handoff.cancel.store(true, Ordering::Relaxed);
        }
        let tracks = self
            .queue
            .iter()
            .map(|track| self.queue_track(track))
            .collect::<Vec<_>>();
        ui.set_queue_lines(ModelRc::new(VecModel::from(queue_lines(
            &tracks,
            self.current,
        ))));
        ui.set_sidebar_lines(ModelRc::new(VecModel::from(sidebar_lines(
            &tracks,
            self.current,
        ))));
        ui.set_queue(ModelRc::new(VecModel::from(tracks)));
        let total = self
            .queue
            .iter()
            .skip(self.current)
            .map(|track| track.duration)
            .sum::<u64>()
            / 1000;
        // Same wording as the original header: "1H 6M" or "42M".
        let label = if total >= 3600 {
            format!("{}H {}M", total / 3600, (total / 60) % 60)
        } else {
            format!("{}M", total / 60)
        };
        ui.set_queue_duration(label.into());
        ui.set_current_track(if self.queue.is_empty() {
            -1
        } else {
            self.current as i32
        });
    }

    fn queue_album(&mut self, ui: &Sonic, tx: &Sender<Event>, index: usize) {
        self.splice_play(ui, tx, self.tracks.clone(), index);
    }

    fn splice_play(&mut self, ui: &Sonic, tx: &Sender<Event>, tracks: Vec<Entry>, selected: usize) {
        let Some(track) = tracks.get(selected) else {
            return;
        };
        if track.local_path.is_none() && self.plex.is_none() {
            ui.set_playback_error("Connect to Plex to play this track.".into());
            return;
        }
        self.scrobble(tx);
        let current = self.current.min(self.queue.len());
        let end = (current + 1).min(self.queue.len());
        self.queue.splice(current..end, tracks);
        self.original_order.clear();
        self.start(ui, tx, current + selected);
        self.publish_queue(ui);
    }

    fn load_queue(&mut self, ui: &Sonic, tx: &Sender<Event>, tracks: Vec<Entry>, index: usize) {
        let Some(selected) = tracks.get(index) else {
            return;
        };
        if selected.local_path.is_none() && self.plex.is_none() {
            ui.set_playback_error("Connect to Plex to play this track.".into());
            return;
        }
        self.scrobble(tx);
        self.queue = tracks;
        self.original_order.clear();
        self.start(ui, tx, index);
        self.publish_queue(ui);
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.queue.iter().position(|track| track.rating_key == key)
    }

    fn scrobble(&mut self, tx: &Sender<Event>) {
        let started = self.started_at.take();
        let listened = std::mem::take(&mut self.listened);
        self.was_playing = false;
        let (Some(started), Some(session), Some(track)) = (
            started,
            self.fm_session.clone(),
            self.queue.get(self.current).cloned(),
        ) else {
            return;
        };
        if track.duration <= 30_000
            || listened < Duration::from_millis((track.duration / 2).min(240_000))
        {
            return;
        }
        let (tx, generation) = (tx.clone(), self.fm_generation);
        thread::spawn(move || {
            if let Err(error) = session.scrobble(&track, started) {
                let _ = tx.send(Event::FmError(generation, error.to_string()));
            }
        });
    }

    fn next_index(&self, finished: bool) -> Option<usize> {
        if self.queue.is_empty() {
            None
        } else if finished && self.repeat_mode == 2 {
            Some(self.current)
        } else if self.current + 1 < self.queue.len() {
            Some(self.current + 1)
        } else if self.repeat_mode == 1 {
            Some(0)
        } else {
            None
        }
    }

    fn advance(&mut self, ui: &Sonic, tx: &Sender<Event>, finished: bool) {
        if let Some(index) = self.next_index(finished) {
            if !finished
                && self.next_index(true) == Some(index)
                && let Some(handoff) = &mut self.handoff
                && handoff.track == self.queue[index]
                && let Some(player) = &self.player
                && !player.sink.is_paused()
            {
                if !handoff.skip_requested && !handoff.started.load(Ordering::Acquire) {
                    handoff.skip_requested = true;
                    player.sink.skip_one();
                }
                return;
            }
            self.start(ui, tx, index);
        } else if finished {
            ui.set_status("End of queue".into());
        }
    }

    fn cancel_handoff(&mut self) {
        if let Some(handoff) = self.handoff.take() {
            handoff.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn sync_handoff(&mut self, ui: &Sonic, tx: &Sender<Event>) {
        let Some(handoff) = &self.handoff else { return };
        let index = self
            .next_index(true)
            .filter(|&index| self.queue[index] == handoff.track);
        if handoff.started.load(Ordering::Acquire) {
            if let Some(index) = index {
                let handoff = self.handoff.take();
                self.start_track(ui, tx, index, handoff);
            } else if let Some(index) = self.next_index(true) {
                self.start(ui, tx, index);
            } else {
                self.cancel_handoff();
                if let Some(player) = &self.player {
                    player.sink.stop();
                }
                ui.set_playing(false);
            }
        } else if index.is_none() || handoff.cancel.load(Ordering::Relaxed) {
            self.cancel_handoff();
        }
    }

    fn stop_and_clear(&mut self, ui: &Sonic) {
        self.cancel_handoff();
        self.discard_prefetch();
        self.cancel_audio.store(true, Ordering::Relaxed);
        self.generation += 1;
        self.seeking = false;
        self.pending_seek = None;
        self.loading = false;
        ui.set_track_loading(false);
        ui.set_playback_error("".into());
        self.queue.clear();
        self.playback_restored = true;
        self.original_order.clear();
        self.history.clear();
        self.history_cursor = None;
        self.current = 0;
        if let Some(player) = &mut self.player {
            player.sink.stop();
            player.sink = Arc::new(Sink::connect_new(player._stream.mixer()));
            player.download = None;
        }
        ui.set_playing(false);
        ui.set_track_key("".into());
        ui.set_track_title("—".into());
        ui.set_track_artist("No track".into());
        ui.set_playing_art(Image::default());
        self.art_theme = None;
        self.refresh_theme(ui);
        ui.set_duration(0.0);
        ui.set_position(0.0);
        ui.set_total("0:00".into());
        ui.set_elapsed("0:00".into());
        ui.set_status("Queue cleared".into());
        self.publish_queue(ui);
    }

    fn apply_volume(&self, ui: &Sonic) {
        let volume = if self.muted { 0.0 } else { self.volume };
        if let Some(player) = &self.player {
            player.sink.set_volume(volume);
        }
        ui.set_volume_level(volume * 100.0);
    }

    fn record_history(&mut self, track: &Entry) {
        if self
            .history_cursor
            .and_then(|index| self.history.get(index))
            .is_some_and(|entry| entry.rating_key == track.rating_key)
        {
            return;
        }
        if self
            .history
            .last()
            .is_none_or(|entry| entry.rating_key != track.rating_key)
        {
            self.history.push(track.clone());
        }
        self.history_cursor = self.history.len().checked_sub(1);
    }

    fn previous_from_history(&mut self, ui: &Sonic, tx: &Sender<Event>) -> bool {
        let Some(index) = self.history_cursor.and_then(|cursor| cursor.checked_sub(1)) else {
            return false;
        };
        let Some(track) = self.history.get(index).cloned() else {
            return false;
        };
        self.scrobble(tx);
        self.history_cursor = Some(index);
        self.original_order.insert(0, track.rating_key.clone());
        self.queue.insert(0, track);
        self.current += 1;
        self.start(ui, tx, 0);
        self.publish_queue(ui);
        true
    }

    fn discard_prefetch(&mut self) {
        if let Some(prefetch) = self.prefetch.take() {
            prefetch.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn preload_next(&mut self, ui: &Sonic, tx: &Sender<Event>) {
        if !ui.get_playing() {
            return;
        }
        let Some(track) = self
            .next_index(true)
            .and_then(|index| self.queue.get(index))
            .cloned()
        else {
            self.discard_prefetch();
            return;
        };
        if self.handoff.is_some() {
            return;
        }
        let ready = self
            .prefetch
            .as_ref()
            .is_some_and(|prefetch| prefetch.key == track.rating_key && prefetch.audio.is_some());
        if ready
            && let Some(player) = &self.player
            && !player.sink.empty()
        {
            let mut prefetch = self.prefetch.take().unwrap();
            let audio = prefetch.audio.take().unwrap();
            let started = Arc::new(AtomicBool::new(false));
            self.handoff = Some(Handoff {
                track,
                started: started.clone(),
                cancel: prefetch.cancel.clone(),
                download: audio.download,
                duration: audio.decoder.total_duration(),
                skip_requested: false,
            });
            player.sink.append(stream::NextSource {
                source: eq::Equalizer::new(audio.decoder, self.equalizer.clone()),
                started,
                cancelled: prefetch.cancel,
                previous: player.download.clone(),
            });
        } else {
            self.prefetch_track(tx, track);
        }
    }

    fn prefetch_track(&mut self, tx: &Sender<Event>, track: Entry) {
        if self
            .prefetch
            .as_ref()
            .is_some_and(|prefetch| prefetch.key == track.rating_key)
        {
            return;
        }
        self.discard_prefetch();
        self.prefetch_id += 1;
        let id = self.prefetch_id;
        let cancel = Arc::new(AtomicBool::new(false));
        self.prefetch = Some(Prefetch {
            id,
            key: track.rating_key.clone(),
            cancel: cancel.clone(),
            audio: None,
            playback_generation: None,
        });
        let (tx, plex) = (tx.clone(), self.plex.clone());
        thread::spawn(move || {
            let audio = load_audio(plex, track, cancel.clone());
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(Event::Prefetched(id, audio));
            }
        });
    }

    fn start(&mut self, ui: &Sonic, tx: &Sender<Event>, index: usize) {
        self.cancel_handoff();
        self.start_track(ui, tx, index, None);
    }

    fn start_track(
        &mut self,
        ui: &Sonic,
        tx: &Sender<Event>,
        index: usize,
        handoff: Option<Handoff>,
    ) {
        let Some(track) = self.queue.get(index).cloned() else {
            return;
        };
        let plex = self.plex.clone();
        if track.local_path.is_none() && plex.is_none() {
            ui.set_playback_error("Connect to Plex to play this track.".into());
            return;
        }
        self.scrobble(tx);
        self.cancel_audio.store(true, Ordering::Relaxed);
        self.cancel_audio = Arc::new(AtomicBool::new(false));
        self.generation += 1;
        self.seeking = false;
        self.pending_seek = None;
        self.current = index;
        self.record_history(&track);
        self.playback_restored = true;
        self.loading = handoff.is_none();
        ui.set_track_loading(self.loading);
        ui.set_playback_error("".into());
        ui.set_current_track(index as i32);
        if handoff.is_none() {
            if let Some(player) = &mut self.player {
                player.sink.stop();
                player.sink = Arc::new(Sink::connect_new(player._stream.mixer()));
                player.download = None;
            }
            ui.set_playing(false);
        }
        ui.set_track_key(track.rating_key.as_str().into());
        ui.set_track_title(track.title.as_str().into());
        ui.set_track_artist(track.grandparent_title.as_str().into());
        ui.set_position(0.0);
        ui.set_duration(0.0);
        ui.set_elapsed("0:00".into());
        ui.set_total(clock(track.duration / 1000).into());
        ui.set_status("Loading track…".into());
        let cancel = self.cancel_audio.clone();
        let generation = self.generation;
        ui.set_playing_art(Image::default());
        let thumb = if track.parent_thumb.is_empty() {
            &track.thumb
        } else {
            &track.parent_thumb
        };
        if track.local_path.is_some() && !track.thumb.is_empty() {
            let (art_tx, path) = (tx.clone(), track.local_path.clone());
            thread::spawn(move || {
                let art = path
                    .and_then(|p| local::art(&p))
                    .and_then(|data| decode_cover_at_size(&data, 640));
                let _ = art_tx.send(Event::PlayingArt(generation, art));
            });
        } else if !thumb.is_empty()
            && let Some(plex) = &plex
        {
            let (art_plex, art_tx, thumb) = (plex.clone(), tx.clone(), thumb.clone());
            thread::spawn(move || {
                let _ = art_tx.send(Event::PlayingArt(
                    generation,
                    cover_at_size(&art_plex, &thumb, 640),
                ));
            });
        }
        if let Some(handoff) = handoff {
            self.cancel_audio = handoff.cancel;
            if let Some(player) = &mut self.player {
                player.download = handoff.download;
            }
            self.audio_started(ui, tx, handoff.duration);
            if self
                .player
                .as_ref()
                .is_some_and(|player| player.sink.is_paused())
            {
                ui.set_playing(false);
            }
            return;
        }
        if let Some(prefetch) = self.prefetch.as_mut().filter(|prefetch| {
            prefetch.key == track.rating_key && prefetch.playback_generation.is_none()
        }) {
            if let Some(audio) = prefetch.audio.take() {
                self.cancel_audio = prefetch.cancel.clone();
                self.prefetch = None;
                let _ = tx.send(Event::Audio(generation, Ok(audio)));
            } else {
                prefetch.playback_generation = Some(generation);
                self.cancel_audio = prefetch.cancel.clone();
            }
            return;
        }
        self.discard_prefetch();
        let tx = tx.clone();
        thread::spawn(move || {
            let audio = load_audio(plex, track, cancel.clone());
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(Event::Audio(generation, audio));
            }
        });
    }

    fn receive(&mut self, ui: &Sonic, tx: &Sender<Event>, event: Event) {
        match event {
            Event::FmPending(generation, result) if generation == self.fm_generation => {
                self.fm_busy = false;
                match result {
                    Ok(pending) => {
                        self.fm_pending = Some(pending);
                        ui.set_lastfm_pending(true);
                        ui.set_lastfm_status(
                            "Approve Sonic on Last.fm, then finish sign-in.".into(),
                        );
                    }
                    Err(error) => ui.set_lastfm_status(error.to_string().into()),
                }
            }
            Event::FmConnected(generation, result) if generation == self.fm_generation => {
                self.fm_busy = false;
                match result {
                    Ok(session) => {
                        let status = if generation != 0 && session.save().is_err() {
                            format!(
                                "Connected as {} for this session. Sign-in could not be saved.",
                                session.username
                            )
                        } else {
                            format!("Connected as {}", session.username)
                        };
                        ui.set_lastfm_status(status.into());
                        ui.set_lastfm_connected(true);
                        ui.set_lastfm_pending(false);
                        self.fm_pending = None;
                        self.fm_session = Some(session);
                    }
                    Err(error) => ui.set_lastfm_status(error.to_string().into()),
                }
            }
            Event::FmTest(generation, result) if generation == self.fm_generation => match result {
                Ok(()) => {
                    if let Some(session) = &self.fm_session {
                        ui.set_lastfm_status(
                            format!("Connected as {} · Verified", session.username).into(),
                        );
                    }
                }
                Err(error) => ui.set_lastfm_status(format!("Test failed: {error}").into()),
            },
            Event::FmError(generation, message) if generation == self.fm_generation => {
                ui.set_lastfm_status(message.into());
            }
            Event::NoSavedPlex => {
                ui.set_connecting(false);
            }
            Event::PlexConnection(result) => {
                ui.set_connecting(false);
                ui.set_refreshing_connection(false);
                match result {
                    Ok(plex) => {
                        self.apply_plex_connection(ui, plex);
                        if !self.loading {
                            self.discard_prefetch();
                        }
                        self.invalidate_library_cache(ui, tx);
                        ui.set_connection_status("Connection refreshed.".into());
                    }
                    Err(error) => ui.set_connection_status(
                        format!("Could not refresh connection: {error}").into(),
                    ),
                }
            }
            Event::Library(result) => {
                ui.set_connecting(false);
                match result {
                    Ok((plex, albums)) => {
                        self.apply_plex_connection(ui, plex);
                        self.publish_plex_libraries(ui);
                        self.plex_albums = albums;
                        self.rebuild_library(ui);
                        self.restore_playback(ui);
                        ui.set_status(format!("{} albums", self.albums.len()).into());
                    }
                    Err(err) => ui.set_status(format!("Plex: {err}").into()),
                }
            }
            Event::LocalLibrary(generation, result)
                if generation == self.local_scan && ui.get_local_enabled() =>
            {
                match result {
                    Ok(albums) => {
                        self.loaded_tracks
                            .retain(|key, _| !key.starts_with("local-album:"));
                        self.loaded_tracks.extend(
                            albums
                                .iter()
                                .map(|(album, tracks)| (album.rating_key.clone(), tracks.clone())),
                        );
                        self.local_albums = albums;
                        self.rebuild_library(ui);
                        ui.set_local_status(
                            format!("{} tracks loaded.", ui.get_local_count()).into(),
                        );
                    }
                    Err(error) => ui.set_local_status(format!("Local files: {error}").into()),
                }
            }
            Event::AlbumAction(generation, key, action, target, result)
                if generation == self.cache_generation =>
            {
                match result {
                    Ok(tracks) => {
                        self.loaded_tracks.insert(key, tracks.clone());
                        self.use_tracks(ui, tx, tracks, action, target);
                    }
                    Err(error) => ui.set_status(format!("Tracks: {error}").into()),
                }
            }
            Event::Tracks(generation, key, result) if generation == self.cache_generation => {
                if !self.tracks_pending.remove(&key) {
                    return;
                }
                let selected = ui.get_selected_key().as_str() == key;
                match result {
                    Ok(tracks) => {
                        self.loaded_tracks.insert(key, tracks.clone());
                        if !self.query.is_empty() {
                            let query = self.query.clone();
                            self.filter(&query);
                        }
                        if selected {
                            self.present_tracks(ui, tracks);
                        }
                    }
                    Err(error) if selected => ui.set_status(format!("Tracks: {error}").into()),
                    Err(_) => {}
                }
            }
            Event::Artwork(generation, key, pixels) if generation == self.cache_generation => {
                if !self.pending_art.remove(&key) {
                    return;
                }
                if let Some(pixels) = pixels {
                    let art = Image::from_rgba8(pixels);
                    if ui.get_selected_key().as_str() == key {
                        ui.set_selected_art(art.clone());
                    }
                    let evicted = if self.artwork.len() >= 160 {
                        let evicted = self
                            .artwork
                            .keys()
                            .find(|cached| {
                                self.lookup
                                    .get(*cached)
                                    .is_some_and(|index| !self.visible.contains(index))
                            })
                            .or_else(|| self.artwork.keys().next())
                            .cloned();
                        if let Some(key) = &evicted {
                            self.artwork.remove(key);
                        }
                        evicted
                    } else {
                        None
                    };
                    self.artwork.insert(key.clone(), art.clone());
                    if self
                        .queue
                        .get(self.current)
                        .is_some_and(|track| track_art_key(track) == key)
                        && ui.get_playing_art().size().width == 0
                    {
                        ui.set_playing_art(art.clone());
                    }
                    if matches!(ui.get_page(), 2 | 3) || ui.get_selected_key().is_empty() {
                        self.track_art_changed = true;
                    }
                    if let Some(evicted) = evicted
                        && let Some(index) = self.album_row(&evicted)
                        && let Some(mut row) = self.album_model.row_data(index)
                    {
                        row.art = Image::default();
                        self.album_model.set_row_data(index, row);
                    }
                    if let Some(index) = self.album_row(&key)
                        && let Some(mut row) = self.album_model.row_data(index)
                    {
                        row.art = art;
                        self.album_model.set_row_data(index, row);
                    }
                } else {
                    self.missing_art.insert(key);
                }
            }
            Event::Lyrics(key, lines) => {
                self.lyrics_pending.remove(&key);
                self.lyrics_cache.insert(key.clone(), lines);
                if key == self.lyrics_key {
                    self.present_lyrics(ui);
                }
            }
            Event::PlayingArt(generation, pixels) if generation == self.generation => {
                if let Some(pixels) = pixels {
                    self.art_theme = sampled_theme(&pixels);
                    self.refresh_theme(ui);
                    ui.set_playing_art(Image::from_rgba8(pixels));
                }
            }
            Event::Prefetched(id, result) => {
                if let Some(prefetch) = &mut self.prefetch
                    && prefetch.id == id
                {
                    if let Some(generation) = prefetch.playback_generation {
                        self.prefetch = None;
                        self.receive(ui, tx, Event::Audio(generation, result));
                    } else if let Ok(audio) = result {
                        prefetch.audio = Some(audio);
                    } else {
                        self.prefetch = None;
                    }
                }
            }
            Event::SeekFinished(generation, error) if generation == self.generation => {
                self.seeking = false;
                if let Some(position) = self.pending_seek.take() {
                    self.seek(tx, position);
                } else if let Some(error) = error {
                    ui.set_playback_error(format!("Seek: {error}").into());
                }
            }
            Event::Audio(generation, result) if generation == self.generation => {
                self.loading = false;
                ui.set_track_loading(false);
                let result = result.and_then(|audio| {
                    let duration = audio.decoder.total_duration();
                    self.play(audio)?;
                    Ok(duration)
                });
                match result {
                    Ok(duration) => self.audio_started(ui, tx, duration),
                    Err(err) => ui.set_playback_error(format!("Playback: {err}").into()),
                }
            }
            _ => {}
        }
    }

    fn audio_started(&mut self, ui: &Sonic, tx: &Sender<Event>, duration: Option<Duration>) {
        if let Some((key, position)) = self.resume.take()
            && self
                .queue
                .get(self.current)
                .is_some_and(|track| track.rating_key == key)
        {
            self.seek(tx, Duration::from_secs_f32(position));
        }
        let track = &mut self.queue[self.current];
        if track.duration == 0
            && let Some(duration) = duration
        {
            track.duration = duration.as_millis() as u64;
        }
        ui.set_duration(track.duration as f32 / 1000.0);
        ui.set_total(clock(track.duration / 1000).into());
        ui.set_status(if track.local_path.is_some() {
            "Playing local file".into()
        } else {
            "Playing from Plex".into()
        });
        ui.set_playing(true);
        self.publish_queue(ui);
        self.started_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        if let Some(session) = self.fm_session.clone() {
            let track = self.queue[self.current].clone();
            let tx = tx.clone();
            let generation = self.fm_generation;
            thread::spawn(move || {
                if let Err(error) = session.now_playing(&track) {
                    let _ = tx.send(Event::FmError(generation, error.to_string()));
                }
            });
        }
    }

    fn album_row(&self, key: &str) -> Option<usize> {
        let index = self.lookup.get(key)?;
        self.visible.iter().position(|candidate| candidate == index)
    }

    fn seek(&mut self, tx: &Sender<Event>, position: Duration) {
        let Some(player) = self.player.as_ref().filter(|player| !player.sink.empty()) else {
            return;
        };
        if self.seeking {
            self.pending_seek = Some(position);
            return;
        }
        self.seeking = true;
        let sink = player.sink.clone();
        let tx = tx.clone();
        let generation = self.generation;
        let equalizer = self.equalizer.clone();
        thread::spawn(move || {
            // Fade out briefly first so the jump doesn't pop; the EQ fades back in after.
            equalizer.begin_seek();
            if !sink.is_paused() {
                thread::sleep(Duration::from_millis(20));
            }
            let error = sink.try_seek(position).err().map(|error| error.to_string());
            equalizer.end_seek();
            let _ = tx.send(Event::SeekFinished(generation, error));
        });
    }

    fn play(&mut self, audio: Audio) -> Result<()> {
        if self.player.is_none() {
            let stream = OutputStreamBuilder::open_default_stream()
                .context("Opening the default audio device")?;
            self.player = Some(Player {
                sink: Arc::new(Sink::connect_new(stream.mixer())),
                download: None,
                _stream: stream,
            });
        }
        if let Some(player) = &mut self.player {
            player.download = audio.download;
            player
                .sink
                .set_volume(if self.muted { 0.0 } else { self.volume });
            player
                .sink
                .append(eq::Equalizer::new(audio.decoder, self.equalizer.clone()));
            player.sink.play();
        }
        Ok(())
    }
}

fn load_audio(plex: Option<Plex>, track: Entry, cancel: Arc<AtomicBool>) -> Result<Audio> {
    let (input, length, download): (Box<dyn AudioInput>, _, _) =
        if let Some(path) = &track.local_path {
            let file = File::open(path).context("Opening local file")?;
            let length = file.metadata()?.len();
            (Box::new(file), Some(length), None)
        } else {
            let response = plex.context("Plex is disconnected")?.track_audio(&track)?;
            let length = response.content_length();
            let input = stream::BufferedDownload::new(response, length, cancel)?;
            let download = input.status();
            (Box::new(input), length, Some(download))
        };
    let mut decoder = Decoder::builder()
        .with_data(std::io::BufReader::new(input))
        .with_seekable(true);
    if let Some(length) = length {
        decoder = decoder.with_byte_len(length);
    }
    Ok(Audio {
        decoder: decoder.build().context("Decoding audio")?,
        download,
    })
}

/// Album covers in grids and lists. 320 px is sharper than any tile at 150% scaling
/// and keeps the cover cache small; Now Playing loads its own larger copy.
const THUMB_SIZE: u32 = 320;

fn cover(plex: &Plex, thumb: &str) -> Option<Art> {
    cover_at_size(plex, thumb, THUMB_SIZE)
}

fn cover_at_size(plex: &Plex, thumb: &str, size: u32) -> Option<Art> {
    let size_query = size.to_string();
    let response = plex
        .request("/photo/:/transcode")
        .ok()?
        .query(&[
            ("url", thumb),
            ("width", size_query.as_str()),
            ("height", size_query.as_str()),
            ("minSize", "0"),
            ("upscale", "0"),
        ])
        .timeout(Duration::from_secs(8))
        .send()
        .ok()?
        .error_for_status()
        .ok()?;
    let mut data = Vec::new();
    response.take(2 * 1024 * 1024).read_to_end(&mut data).ok()?;
    decode_cover_at_size(&data, size)
}

fn decode_cover(data: &[u8]) -> Option<Art> {
    decode_cover_at_size(data, THUMB_SIZE)
}

fn decode_cover_at_size(data: &[u8], size: u32) -> Option<Art> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let pixels = if image.width().max(image.height()) > size {
        image
            // Triangle is several times faster than Lanczos and looks the same when shrinking.
            .resize(size, size, image::imageops::FilterType::Triangle)
            .into_rgba8()
    } else {
        image.into_rgba8()
    };
    Some(SharedPixelBuffer::clone_from_slice(
        pixels.as_raw(),
        pixels.width(),
        pixels.height(),
    ))
}

fn local_entries(paths: impl IntoIterator<Item = PathBuf>) -> Vec<Entry> {
    paths
        .into_iter()
        .filter_map(|path| {
            let local_path = path.canonicalize().ok()?;
            if !local_path.is_file() {
                return None;
            }
            Entry::local(local_path)
        })
        .collect()
}

fn track_art_key(track: &Entry) -> String {
    if !track.parent_rating_key.is_empty() {
        track.parent_rating_key.clone()
    } else if !track.parent_title.is_empty() {
        format!(
            "album:{:?}:{:?}",
            track.parent_title, track.grandparent_title
        )
    } else {
        track.rating_key.clone()
    }
}

/// Row sizes (visual height, gap below) for one queue presentation, in logical pixels.
struct QueueLayout {
    header: (i32, i32),
    grouped_row: (i32, i32),
    single_row: (i32, i32),
    group_gap: i32,
    /// The Queue page stops grouping when an album appears in more than one run.
    flatten_repeats: bool,
}

/// Matches the original Queue page: 48px album header + 9px, 36px rows 4px apart,
/// 52px single rows, 21px between groups.
const QUEUE_PAGE: QueueLayout = QueueLayout {
    header: (48, 9),
    grouped_row: (36, 4),
    single_row: (52, 21),
    group_gap: 21,
    flatten_repeats: true,
};

/// Sidebar mini queue: a little tighter than the original (40px header + 4px,
/// 32px rows 2px apart, 44px single rows, 10px between groups).
const QUEUE_SIDEBAR: QueueLayout = QueueLayout {
    header: (40, 4),
    grouped_row: (32, 2),
    single_row: (44, 10),
    group_gap: 10,
    flatten_repeats: false,
};

fn queue_lines(tracks: &[QueueTrack], current: usize) -> Vec<QueueLine> {
    layout_queue(tracks, current, &QUEUE_PAGE)
}

fn sidebar_lines(tracks: &[QueueTrack], current: usize) -> Vec<QueueLine> {
    layout_queue(tracks, current, &QUEUE_SIDEBAR)
}

fn layout_queue(tracks: &[QueueTrack], current: usize, layout: &QueueLayout) -> Vec<QueueLine> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut seen = HashSet::new();
    let mut repeated = false;
    let mut index = current;
    while let Some(track) = tracks.get(index) {
        let start = index;
        index += 1;
        while tracks
            .get(index)
            .is_some_and(|next| next.album_key == track.album_key)
        {
            index += 1;
        }
        repeated |= !seen.insert(track.album_key.as_str());
        runs.push((start, index));
    }
    let flat = layout.flatten_repeats && repeated;
    let line =
        |index: i32, (height, gap): (i32, i32), grouped, track: &QueueTrack, number| QueueLine {
            index,
            height: height + gap,
            gap,
            grouped,
            track: track.clone(),
            number,
        };
    let mut lines = Vec::with_capacity(tracks.len().saturating_sub(current) + runs.len());
    for (start, end) in runs {
        let grouped = !flat && end - start > 1;
        if grouped {
            lines.push(line(-1, layout.header, true, &tracks[start], 0));
        }
        for (index, track) in tracks.iter().enumerate().take(end).skip(start) {
            let number = if track.number > 0 {
                track.number
            } else {
                (index - start + 1) as i32
            };
            let size = match (grouped, flat) {
                (true, _) if index + 1 == end => (layout.grouped_row.0, layout.group_gap),
                (true, _) => layout.grouped_row,
                // A repeated-album queue is one flat list with 4px gaps, like the original.
                (false, true) => (layout.single_row.0, 4),
                (false, false) => layout.single_row,
            };
            lines.push(line(index as i32, size, grouped, track, number));
        }
    }
    lines
}

fn queue_drop_index(lines: impl IntoIterator<Item = QueueLine>, from: i32, distance: f32) -> i32 {
    let mut centers = Vec::new();
    let mut position = 0.0;
    let mut origin = None;
    for line in lines {
        let center = position + line.height as f32 / 2.0;
        position += line.height as f32;
        if line.index < 0 {
            continue;
        }
        if line.index == from {
            origin = Some(center);
        }
        centers.push((line.index, center));
    }
    let Some(origin) = origin else {
        return from;
    };
    centers
        .into_iter()
        .min_by(|(_, a), (_, b)| {
            (a - origin - distance)
                .abs()
                .total_cmp(&(b - origin - distance).abs())
        })
        .map(|(index, _)| index)
        .unwrap_or(from)
}

fn track_model(tracks: &[Entry]) -> ModelRc<Track> {
    ModelRc::new(VecModel::from(
        tracks
            .iter()
            .map(|track| Track {
                key: track.rating_key.as_str().into(),
                number: track.index as i32,
                title: track.title.as_str().into(),
                time: clock(track.duration / 1000).into(),
            })
            .collect::<Vec<_>>(),
    ))
}

fn parse_theme(value: &str) -> Option<[u8; 3]> {
    let value = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if !value.is_ascii() {
        return None;
    }
    match value.len() {
        3 => {
            let mut rgb = [0; 3];
            for (i, byte) in rgb.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&value[i..i + 1], 16).ok()? * 17;
            }
            Some(rgb)
        }
        6 => Some([
            u8::from_str_radix(&value[..2], 16).ok()?,
            u8::from_str_radix(&value[2..4], 16).ok()?,
            u8::from_str_radix(&value[4..], 16).ok()?,
        ]),
        _ => None,
    }
}

/// Background, surface, border, hover, selected, text, muted, accent, accent text, accent ink.
type PaletteColors = [[u8; 3]; 10];

#[derive(Default)]
struct ThemeFade {
    timer: Rc<Timer>,
    displayed: Rc<std::cell::Cell<Option<PaletteColors>>>,
}

fn apply_palette(ui: &Sonic, colors: PaletteColors) {
    let [
        bg,
        surface,
        border,
        hover,
        selected,
        text,
        muted,
        accent,
        accent_text,
        accent_ink,
    ] = colors.map(color);
    let palette = ui.global::<Palette>();
    palette.set_background(bg);
    palette.set_panel(surface);
    palette.set_surface(surface);
    palette.set_border(border);
    palette.set_hover(hover);
    palette.set_selected(selected);
    palette.set_text(text);
    palette.set_muted(muted);
    palette.set_accent(accent);
    palette.set_accent_text(accent_text);
    palette.set_accent_ink(accent_ink);
}

/// Tiles up to four covers 2x2 (1 cover is used as is; 2 or 3 repeat to fill the grid).
fn mosaic(covers: &[&Image]) -> Option<Image> {
    const TILE: u32 = 240;
    let tiles: Vec<image::RgbaImage> = covers
        .iter()
        .filter_map(|cover| {
            let pixels = cover.to_rgba8()?;
            let source = image::RgbaImage::from_raw(
                pixels.width(),
                pixels.height(),
                pixels.as_bytes().to_vec(),
            )?;
            // Center-crop to a square, then scale to one tile.
            let side = source.width().min(source.height());
            let square = image::imageops::crop_imm(
                &source,
                (source.width() - side) / 2,
                (source.height() - side) / 2,
                side,
                side,
            )
            .to_image();
            Some(image::imageops::resize(
                &square,
                TILE,
                TILE,
                image::imageops::FilterType::Triangle,
            ))
        })
        .collect();
    if tiles.len() == 1 {
        return Some(covers[0].clone());
    }
    let order: [usize; 4] = match tiles.len() {
        2 => [0, 1, 1, 0],
        3 => [0, 1, 2, 0],
        4 => [0, 1, 2, 3],
        _ => return None,
    };
    let mut canvas = image::RgbaImage::new(TILE * 2, TILE * 2);
    for (slot, &tile) in order.iter().enumerate() {
        let (x, y) = ((slot as u32 % 2) * TILE, (slot as u32 / 2) * TILE);
        image::imageops::replace(&mut canvas, &tiles[tile], i64::from(x), i64::from(y));
    }
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        canvas.as_raw(),
        canvas.width(),
        canvas.height(),
    );
    Some(Image::from_rgba8(buffer))
}

fn color(rgb: [u8; 3]) -> slint::Color {
    slint::Color::from_rgb_u8(rgb[0], rgb[1], rgb[2])
}

fn mix(a: [u8; 3], b: [u8; 3], strength: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 * (1.0 - strength) + b[i] as f32 * strength).round() as u8)
}

fn search_separator(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '-' | '_' | '.' | ',' | '\'' | '"')
}

fn library_matches(query: &[&str], value: &str) -> bool {
    let normalized = value
        .to_lowercase()
        .chars()
        .filter(|&ch| !search_separator(ch))
        .collect::<String>();
    query.iter().all(|part| normalized.contains(part))
}

fn to_hsl(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|component| f64::from(component) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let range = max - min;
    if range == 0.0 {
        return [0.0, 0.0, lightness];
    }
    let saturation = if lightness > 0.5 {
        range / (2.0 - max - min)
    } else {
        range / (max + min)
    };
    let hue = if max == r {
        ((g - b) / range).rem_euclid(6.0)
    } else if max == g {
        (b - r) / range + 2.0
    } else {
        (r - g) / range + 4.0
    };
    [hue * 60.0, saturation, lightness]
}

fn from_hsl(hue: f64, saturation: f64, lightness: f64) -> [u8; 3] {
    let a = saturation * lightness.min(1.0 - lightness);
    [0.0, 8.0, 4.0].map(|offset| {
        let k = (offset + hue / 30.0) % 12.0;
        let component = lightness - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0);
        (component * 255.0).round() as u8
    })
}

fn luminance(rgb: [u8; 3]) -> f32 {
    let [r, g, b] = rgb.map(|x| {
        let x = f32::from(x) / 255.0;
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

struct ThemeColors {
    surface: [u8; 3],
    border: [u8; 3],
    hover: [u8; 3],
    selected: [u8; 3],
    text: [u8; 3],
    muted: [u8; 3],
    accent: [u8; 3],
    accent_text: [u8; 3],
    accent_ink: [u8; 3],
}

fn contrast(first: [u8; 3], second: [u8; 3]) -> f32 {
    let (a, b) = (luminance(first), luminance(second));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn contrast_color(
    preferred: [u8; 3],
    fallback: [u8; 3],
    backgrounds: &[[u8; 3]],
    minimum: f32,
) -> [u8; 3] {
    // Keep the theme tint where possible and only move as far as needed.
    (0..=100)
        .map(|step| mix(preferred, fallback, step as f32 / 100.0))
        .find(|&candidate| {
            backgrounds
                .iter()
                .all(|&bg| contrast(candidate, bg) >= minimum)
        })
        .unwrap_or(fallback)
}

fn theme_colors(bg: [u8; 3], text_mode: i32) -> ThemeColors {
    const LIGHT_TEXT: [u8; 3] = [232, 238, 244];
    const DARK_TEXT: [u8; 3] = [26, 34, 52];
    let [hue, saturation, lightness] = to_hsl(bg);
    // A narrow, subtle range: surfaces and borders sit close to the page, and the
    // accent (selected and active states) is only a gentle step away from it.
    let step = |amount: f64| if lightness > 0.55 { -amount } else { amount };
    let original_surface = from_hsl(hue, saturation, (lightness + step(0.06)).clamp(0.06, 0.94));
    let border = from_hsl(hue, saturation, (lightness + step(0.08)).clamp(0.10, 0.90));
    let accent = from_hsl(
        hue,
        (saturation + 0.06).min(0.60),
        (lightness + step(0.15)).clamp(0.16, 0.84),
    );
    let light = match text_mode {
        1 => true,
        2 => false,
        _ => luminance(bg) < 0.45 && contrast([255; 3], bg) >= 4.5,
    };
    let (preferred, muted_base, extreme) = if light {
        (LIGHT_TEXT, [156, 168, 184], [255; 3])
    } else {
        (DARK_TEXT, [74, 85, 104], [0; 3])
    };
    let text = if text_mode == 0 {
        contrast_color(preferred, extreme, &[bg], 4.5)
    } else {
        preferred
    };
    let readable_surface = |candidate| {
        if text_mode == 0 {
            contrast_color(candidate, bg, &[text], 4.5)
        } else {
            candidate
        }
    };
    let surface = readable_surface(original_surface);
    let hover = readable_surface(border);
    // Selected rows and buttons: the same hue as the surfaces, just a small step brighter.
    let selected = readable_surface(from_hsl(
        hue,
        saturation,
        (lightness + step(0.10)).clamp(0.10, 0.90),
    ));
    let backgrounds = [bg, surface, hover, selected];
    let muted = if text_mode == 0 {
        contrast_color(muted_base, text, &backgrounds, 4.5)
    } else {
        muted_base
    };
    let accent_extreme = if contrast([255; 3], accent) >= contrast([0; 3], accent) {
        [255; 3]
    } else {
        [0; 3]
    };
    ThemeColors {
        surface,
        border,
        hover,
        selected,
        text,
        muted,
        accent,
        accent_text: contrast_color(text, accent_extreme, &[accent], 4.5),
        accent_ink: contrast_color(accent, text, &backgrounds, 4.5),
    }
}

fn sampled_theme(image: &Art) -> Option<[u8; 3]> {
    let source =
        image::RgbaImage::from_raw(image.width(), image.height(), image.as_bytes().to_vec())?;
    let sample = image::imageops::resize(&source, 64, 64, image::imageops::FilterType::Triangle);
    let mut buckets = [[0u32; 4]; 4096];
    for image::Rgba([r, g, b, a]) in sample.pixels().copied() {
        if a < 128 {
            continue;
        }
        let key = (usize::from(r >> 4) << 8) | (usize::from(g >> 4) << 4) | usize::from(b >> 4);
        let bucket = &mut buckets[key];
        bucket[0] += 1;
        bucket[1] += u32::from(r);
        bucket[2] += u32::from(g);
        bucket[3] += u32::from(b);
    }
    let average = |bucket: &[u32; 4]| -> [u8; 3] {
        let count = bucket[0].max(1);
        std::array::from_fn(|channel| ((bucket[channel + 1] + count / 2) / count) as u8)
    };
    // Equal counts keep the first bucket, matching the original numeric key order.
    let dominant = buckets.iter().rev().max_by_key(|bucket| bucket[0])?;
    if dominant[0] == 0 {
        return None;
    }
    // Prefer the most striking color that still covers a real part of the cover:
    // weight each color by how much of the art it fills and by its vividness,
    // skipping near-black and near-white pixels that read as grey.
    let total: u32 = buckets.iter().map(|bucket| bucket[0]).sum();
    let vivid = buckets
        .iter()
        .filter(|bucket| bucket[0] * 200 >= total)
        .map(|bucket| {
            let color = average(bucket);
            let max = color.iter().copied().max().unwrap_or(0);
            let min = color.iter().copied().min().unwrap_or(0);
            let chroma = f64::from(max - min) / 255.0;
            let usable = max > 40 && min < 225;
            let score = if usable {
                f64::from(bucket[0]).sqrt() * chroma.powi(2)
            } else {
                0.0
            };
            (score, chroma, color)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0));
    let (vivid, [hue, saturation, lightness]) = match vivid {
        Some((_, chroma, color)) if chroma >= 0.22 => (true, to_hsl(color)),
        // Black-and-white or muted art: keep its dominant tone.
        _ => (false, to_hsl(average(dominant))),
    };
    // Album themes stay dark and subtle: a deep, softened version of the color,
    // so every album lands in a similar, calm range.
    let saturation = if vivid {
        (saturation * 0.5).clamp(0.22, 0.42)
    } else {
        saturation.min(0.3)
    };
    Some(from_hsl(
        hue,
        saturation.min(0.4),
        lightness.clamp(0.09, 0.15),
    ))
}

fn window_on_screen(ui: &Sonic) -> bool {
    #[cfg(windows)]
    return desktop::window_shown(ui);
    #[cfg(not(windows))]
    return !ui.window().is_minimized();
}

fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn main() -> Result<()> {
    #[cfg(windows)]
    let Some(instance) = desktop::Instance::acquire()? else {
        return Ok(());
    };
    update::start();
    let ui = Sonic::new()?;
    let (data, can_save) = match library::load() {
        Ok(data) => (data, true),
        Err(error) => {
            ui.set_status(format!("Could not read saved data: {error}").into());
            (LibraryData::default(), false)
        }
    };
    let preferences = data.preferences.clone();
    ui.set_theme_value(preferences.theme.as_str().into());
    ui.set_text_mode(preferences.text_mode.clamp(0, 2));
    ui.set_font_index(preferences.font.clamp(0, 5));
    ui.set_normalizer_enabled(preferences.normalizer);
    ui.set_online_lyrics(preferences.online_lyrics);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_eq_preset_index(preferences.eq_preset.clamp(0, 6));
    ui.set_discord_id(preferences.discord_id.as_str().into());
    ui.set_discord_image(preferences.discord_image.as_str().into());
    ui.set_auto_art_theme(preferences.auto_art_theme);
    // The first native prototype used a 168 px default. Migrate that default to the
    // original Sonic sidebar while retaining explicitly resized widths.
    let sidebar = if preferences.sidebar == 168.0 {
        260.0
    } else {
        preferences.sidebar
    };
    ui.set_sidebar_width(sidebar.clamp(200.0, 420.0));
    ui.set_track_panel_width(preferences.track_panel.clamp(190.0, 335.0));
    ui.set_layout_locked(preferences.lock_layout);
    ui.set_list_mode(preferences.list_view);
    ui.set_local_enabled(preferences.local_enabled);
    if let Ok(folder) = local::folder() {
        ui.set_local_folder(folder.display().to_string().into());
    }
    ui.set_streaming_local(preferences.streaming_local.clamp(0, 3));
    ui.set_streaming_remote(preferences.streaming_remote.clamp(0, 3));
    ui.set_shuffled(preferences.shuffled);
    ui.set_repeat_mode(preferences.repeat_mode.clamp(0, 2));
    let model = Rc::new(VecModel::default());
    let songs = Rc::new(VecModel::default());
    ui.set_song_hits(ModelRc::from(songs.clone()));
    let album_rows = Rc::new(VecModel::default());
    ui.set_album_rows(ModelRc::from(album_rows.clone()));
    ui.set_albums(ModelRc::from(model.clone()));
    let state = Rc::new(RefCell::new(State {
        last_page: -1,
        resume: None,
        mosaics: RefCell::new(HashMap::new()),
        theme_fade: ThemeFade::default(),
        lyrics_key: String::new(),
        lyrics: Vec::new(),
        lyrics_cache: HashMap::new(),
        lyrics_pending: HashSet::new(),
        lyrics_chars: 34,
        lyric_index: -1,
        plex: None,
        data,
        equalizer: Arc::new(eq::Settings::default()),
        eq_gains: preferences.eq_gains.map(|gain| gain.clamp(-12.0, 12.0)),
        eq_preamp: preferences.eq_preamp.clamp(-12.0, 12.0),
        can_save,
        art_theme: None,
        fm_generation: 0,
        fm_busy: false,
        fm_pending: None,
        fm_session: None,
        listened: Duration::ZERO,
        started_at: None,
        last_tick: Instant::now(),
        last_save: Instant::now(),
        playback_restored: false,
        was_playing: false,
        query: String::new(),
        albums: Vec::new(),
        plex_albums: Vec::new(),
        local_albums: Vec::new(),
        local_scan: 0,
        album_sort: 2,
        album_group: 0,
        album_columns: 1,
        album_rows,
        local_only: false,
        loaded_tracks: HashMap::new(),
        tracks_pending: HashSet::new(),
        songs,
        lookup: HashMap::new(),
        visible: Vec::new(),
        album_model: model,
        artwork: HashMap::new(),
        pending_art: HashSet::new(),
        cache_generation: 0,
        missing_art: HashSet::new(),
        track_art_changed: false,
        tracks: Vec::new(),
        queue: Vec::new(),
        current: 0,
        original_order: Vec::new(),
        history: Vec::new(),
        history_cursor: None,
        shuffled: preferences.shuffled,
        repeat_mode: preferences.repeat_mode.clamp(0, 2),
        loading: false,
        muted: preferences.muted,
        generation: 0,
        seeking: false,
        pending_seek: None,
        cancel_audio: Arc::new(AtomicBool::new(false)),
        prefetch: None,
        prefetch_id: 0,
        handoff: None,
        player: None,
        volume: preferences.volume.clamp(0.0, 1.0),
    }));
    state.borrow().publish_playlists(&ui);
    state.borrow().publish_eq(&ui);
    state
        .borrow()
        .equalizer
        .set_normalizer(preferences.normalizer);
    state.borrow().refresh_theme(&ui);
    state.borrow().apply_volume(&ui);
    let (tx, rx) = mpsc::channel();
    {
        ui.set_local_picker(true);
        ui.on_open_files({
            let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
            move || {
                let Some(ui) = weak.upgrade() else { return };
                let paths = rfd::FileDialog::new()
                    .set_title("Open audio in Sonic")
                    .add_filter(
                        "Audio",
                        &["mp3", "flac", "wav", "m4a", "aac", "opus", "ogg"],
                    )
                    .pick_files();
                if let Some(paths) = paths {
                    let tracks = local_entries(paths);
                    state.borrow_mut().load_queue(&ui, &tx, tracks, 0);
                    ui.set_page(2);
                }
            }
        });
    }

    ui.on_sign_in({
        let state = state.clone();
        let weak = ui.as_weak();
        let tx = tx.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_connecting(true);
            ui.set_status("Approve Sonic in the Plex sign-in window…".into());
            let tx = tx.clone();
            let wanted = state.borrow().data.preferences.plex_libraries.clone();
            thread::spawn(move || {
                let result =
                    Plex::sign_in().and_then(|plex| Ok((plex.clone(), plex.albums(&wanted)?)));
                let _ = tx.send(Event::Library(result));
            });
        }
    });
    ui.on_search({
        let state = state.clone();
        move |query| state.borrow_mut().filter(query.as_str())
    });
    ui.on_close_album({
        let (state, weak) = (state.clone(), ui.as_weak());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            state.borrow_mut().tracks.clear();
            ui.set_selected_key("".into());
            ui.set_album_title("".into());
            ui.set_album_year("".into());
            ui.set_album_artist("".into());
            ui.set_tracks(ModelRc::default());
        }
    });
    ui.on_select_album({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |key| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let Some(&index) = state.lookup.get(key.as_str()) else {
                return;
            };
            let album = state.albums[index].clone();
            state.tracks.clear();
            ui.set_selected_key(key.clone());
            ui.set_album_title(album.title.as_str().into());
            ui.set_album_year(if album.year > 0 {
                album.year.to_string().into()
            } else {
                "".into()
            });
            ui.set_album_artist(album.parent_title.as_str().into());
            ui.set_selected_art(state.artwork.get(key.as_str()).cloned().unwrap_or_default());
            if let Some(tracks) = state.loaded_tracks.get(key.as_str()).cloned() {
                state.present_tracks(&ui, tracks);
                return;
            }
            ui.set_tracks(ModelRc::default());
            state.fetch_tracks(key.as_str(), &tx);
        }
    });
    ui.on_prefetch_album_tracks({
        let (state, tx) = (state.clone(), tx.clone());
        move |key| {
            let mut state = state.borrow_mut();
            if state.tracks_pending.len() < 3 {
                state.fetch_tracks(key.as_str(), &tx);
            }
        }
    });
    ui.on_play_search_hit({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |key, index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut state = state.borrow_mut();
            if let Some(tracks) = state.loaded_tracks.get(key.as_str()).cloned() {
                state.splice_play(&ui, &tx, tracks, index as usize);
            }
        }
    });
    ui.on_search_hit_action({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |key, index, action, target| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let Some(tracks) = state.loaded_tracks.get(key.as_str()).cloned() else {
                return;
            };
            let index = index as usize;
            let Some(track) = tracks.get(index).cloned() else {
                return;
            };
            if action == 2 {
                // Shuffle the album and start from the chosen song, like the album track menu.
                let mut order: Vec<_> = (0..tracks.len()).collect();
                fastrand::shuffle(&mut order);
                let selected = order
                    .iter()
                    .position(|&row| row == index)
                    .unwrap_or_default();
                let shuffled = order.into_iter().map(|row| tracks[row].clone()).collect();
                state.splice_play(&ui, &tx, shuffled, selected);
            } else {
                state.use_tracks(&ui, &tx, vec![track], action, target);
            }
        }
    });
    ui.on_request_visible_art({
        let (state, tx) = (state.clone(), tx.clone());
        move |top, height, row_height, year_height, columns| {
            let mut state = state.borrow_mut();
            let plex = state.plex.clone();
            let columns = columns.max(1) as usize;
            if columns != state.album_columns {
                state.album_columns = columns;
                state.rebuild_album_rows();
            }
            let row_height = row_height.max(1.0);
            let mut y = 0.0;
            let mut visible = Vec::new();
            for row in state.album_rows.iter() {
                let next = y
                    + row_height
                    + if row.has_year { year_height } else { 0.0 }
                    + if row.heading.is_empty() { 0.0 } else { 27.0 };
                if next >= top && y <= top + height + row_height {
                    visible.extend(row.offset as usize..(row.offset + row.count) as usize);
                }
                if y > top + height + row_height {
                    break;
                }
                y = next;
            }
            for index in visible
                .into_iter()
                .map(|row| state.visible[row])
                .collect::<Vec<_>>()
            {
                if state.pending_art.len() >= 4 {
                    break;
                }
                let album = &state.albums[index];
                if (album.thumb.is_empty() && album.local_path.is_none())
                    || state.artwork.contains_key(&album.rating_key)
                    || state.missing_art.contains(&album.rating_key)
                    || state.pending_art.contains(&album.rating_key)
                {
                    continue;
                }
                let (key, thumb, path) = (
                    album.rating_key.clone(),
                    album.thumb.clone(),
                    album.local_path.clone(),
                );
                state.pending_art.insert(key.clone());
                let generation = state.cache_generation;
                let (plex, tx) = (plex.clone(), tx.clone());
                thread::spawn(move || {
                    let art = if let Some(path) = path {
                        local::art(&path).and_then(|data| decode_cover(&data))
                    } else {
                        plex.and_then(|p| cover(&p, &thumb))
                    };
                    let _ = tx.send(Event::Artwork(generation, key, art));
                });
            }
        }
    });
    ui.on_sort_albums({
        let state = state.clone();
        move |sort| {
            let mut state = state.borrow_mut();
            state.album_sort = sort;
            let query = state.query.clone();
            state.filter(&query);
        }
    });
    ui.on_group_albums({
        let state = state.clone();
        move |group| {
            let mut state = state.borrow_mut();
            state.album_group = group;
            let query = state.query.clone();
            state.filter(&query);
        }
    });
    ui.on_filter_local({
        let state = state.clone();
        move |local| {
            let mut state = state.borrow_mut();
            state.local_only = local;
            let query = state.query.clone();
            state.filter(&query);
        }
    });
    ui.on_start_local({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_local_enabled(!ui.get_local_enabled());
            let mut state = state.borrow_mut();
            if ui.get_local_enabled() {
                state.save(&ui, "Local files enabled");
                state.scan_local(&ui, &tx);
            } else {
                if ui.get_selected_key().as_str().starts_with("local-album:") {
                    state.tracks.clear();
                    ui.set_selected_key("".into());
                    ui.set_album_title("".into());
                    ui.set_album_year("".into());
                    ui.set_album_artist("".into());
                    ui.set_tracks(ModelRc::default());
                }
                state.local_scan += 1;
                state.local_only = false;
                state.local_albums.clear();
                state
                    .loaded_tracks
                    .retain(|key, _| !key.starts_with("local-album:"));
                state.rebuild_library(&ui);
                ui.set_local_status("".into());
                state.save(&ui, "Local files disabled");
            }
        }
    });
    ui.on_open_local_folder(|| {
        if let Ok(folder) = local::folder() {
            let _ = open::that_detached(folder);
        }
    });
    ui.on_refresh_library({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            if ui.get_local_enabled() {
                state.scan_local(&ui, &tx);
            }
            if !ui.get_connecting()
                && let Some(plex) = state.plex.clone()
            {
                ui.set_connecting(true);
                let tx = tx.clone();
                let wanted = state.data.preferences.plex_libraries.clone();
                thread::spawn(move || {
                    let result = plex.albums(&wanted).map(|albums| (plex, albums));
                    let _ = tx.send(Event::Library(result));
                });
            }
        }
    });
    ui.on_album_action({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |album, action, target| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            if let Some(tracks) = state.loaded_tracks.get(album.as_str()).cloned() {
                state.use_tracks(&ui, &tx, tracks, action, target);
            } else if let Some(plex) = state.plex.clone()
                && let Some(index) = state.lookup.get(album.as_str()).copied()
            {
                let album = state.albums[index].clone();
                let generation = state.cache_generation;
                let tx = tx.clone();
                thread::spawn(move || {
                    let result = plex.tracks(&album);
                    let _ = tx.send(Event::AlbumAction(
                        generation,
                        album.rating_key,
                        action,
                        target,
                        result,
                    ));
                });
            }
        }
    });
    ui.on_track_action({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |index, action, target| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let index = index as usize;
            let Some(track) = state.tracks.get(index).cloned() else {
                return;
            };
            if action == 2 {
                let mut indices: Vec<_> = (0..state.tracks.len()).collect();
                fastrand::shuffle(&mut indices);
                let selected = indices
                    .iter()
                    .position(|&row| row == index)
                    .unwrap_or_default();
                let tracks = indices
                    .into_iter()
                    .map(|row| state.tracks[row].clone())
                    .collect();
                state.splice_play(&ui, &tx, tracks, selected);
                return;
            }
            let tracks = vec![track];
            state.use_tracks(&ui, &tx, tracks, action, target);
        }
    });
    ui.on_queue_track_action({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |index, action, target| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let index = index as usize;
            let Some(track) = state.queue.get(index).cloned() else {
                return;
            };
            if action == 2 {
                let grouped = ui
                    .get_queue_lines()
                    .iter()
                    .any(|row| row.index == index as i32 && row.grouped);
                let mut start = index;
                let mut end = index + 1;
                if grouped {
                    let key = track_art_key(&track);
                    while start > state.current && track_art_key(&state.queue[start - 1]) == key {
                        start -= 1;
                    }
                    while end < state.queue.len() && track_art_key(&state.queue[end]) == key {
                        end += 1;
                    }
                }
                let mut rows: Vec<_> = (start..end).collect();
                fastrand::shuffle(&mut rows);
                let selected = rows
                    .iter()
                    .position(|&row| row == index)
                    .unwrap_or_default();
                let tracks = rows
                    .into_iter()
                    .map(|row| state.queue[row].clone())
                    .collect();
                state.splice_play(&ui, &tx, tracks, selected);
            } else {
                state.use_tracks(&ui, &tx, vec![track], action, target);
            }
        }
    });
    ui.on_prefetch_library_track({
        let (state, tx) = (state.clone(), tx.clone());
        move |album, index| {
            let mut state = state.borrow_mut();
            if !state.queue.is_empty() {
                return;
            }
            let tracks = if album.is_empty() {
                Some(&state.tracks)
            } else {
                state.loaded_tracks.get(album.as_str())
            };
            if let Some(track) = tracks
                .and_then(|tracks| tracks.get(index as usize))
                .cloned()
            {
                state.prefetch_track(&tx, track);
            }
        }
    });
    ui.on_select_track({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            state.queue_album(&ui, &tx, index as usize);
        }
    });
    ui.on_toggle_play({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            if let Some(player) = &state.player
                && !player.sink.empty()
            {
                if player.sink.is_paused() {
                    player.sink.play();
                } else {
                    player.sink.pause();
                }
                ui.set_playing(!player.sink.is_paused());
            } else if !state.loading {
                if state.queue.is_empty() {
                    state.queue_album(&ui, &tx, 0);
                } else {
                    let index = state.current;
                    state.start(&ui, &tx, index);
                }
            }
        }
    });
    ui.on_next_track({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            if let Some(ui) = weak.upgrade() {
                state.borrow_mut().advance(&ui, &tx, false);
            }
        }
    });
    ui.on_previous_track({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut state = state.borrow_mut();
            if state.previous_from_history(&ui, &tx) {
                return;
            }
            if ui.get_duration() > 0.0
                && ui.get_position() >= ui.get_duration() / 2.0
                && state
                    .player
                    .as_ref()
                    .is_some_and(|player| !player.sink.empty())
            {
                drop(state);
                ui.invoke_seek(0.0);
            } else {
                let index = state.current.saturating_sub(1);
                state.start(&ui, &tx, index);
            }
        }
    });
    ui.on_seek({
        let (state, tx) = (state.clone(), tx.clone());
        move |seconds| {
            if let Ok(position) = Duration::try_from_secs_f32(seconds.max(0.0)) {
                state.borrow_mut().seek(&tx, position);
            }
        }
    });
    ui.on_select_queue({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |index| {
            if let Some(ui) = weak.upgrade() {
                state.borrow_mut().start(&ui, &tx, index as usize);
            }
        }
    });
    ui.on_queue_drop_index({
        let weak = ui.as_weak();
        move |index, distance| {
            weak.upgrade()
                .map(|ui| queue_drop_index(ui.get_queue_lines().iter(), index, distance))
                .unwrap_or(index)
        }
    });
    ui.on_move_queue({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |row, delta| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let from = row as usize;
            let to = row.saturating_add(delta) as usize;
            if from == to || from >= state.queue.len() || to >= state.queue.len() {
                return;
            }
            let current = state
                .queue
                .get(state.current)
                .map(|track| track.rating_key.clone());
            let entry = state.queue.remove(from);
            state.queue.insert(to, entry);
            state.original_order.clear();
            if current.as_deref() != Some(state.queue[0].rating_key.as_str()) {
                state.current = current
                    .as_deref()
                    .and_then(|key| state.index_of(key))
                    .unwrap_or_default();
                state.start(&ui, &tx, 0);
            } else {
                state.current = 0;
            }
            state.publish_queue(&ui);
        }
    });
    ui.on_remove_queue({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |row| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let row = row as usize;
            if row >= state.queue.len() {
                return;
            }
            if row == state.current {
                state.scrobble(&tx);
            }
            state.queue.remove(row);
            state.original_order.clear();
            if state.queue.is_empty() {
                state.stop_and_clear(&ui);
            } else {
                if row == state.current {
                    let next = row.min(state.queue.len() - 1);
                    state.start(&ui, &tx, next);
                } else if row < state.current {
                    state.current -= 1;
                }
                state.publish_queue(&ui);
            }
        }
    });
    ui.on_clear_queue({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            if let Some(ui) = weak.upgrade() {
                let mut state = state.borrow_mut();
                if state.queue.get(state.current).is_some() {
                    state.clear_up_next(&ui);
                } else {
                    state.scrobble(&tx);
                    state.stop_and_clear(&ui);
                }
            }
        }
    });
    ui.on_shuffle({
        let (state, weak) = (state.clone(), ui.as_weak());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            state.shuffled = !state.shuffled;
            let mut order: Vec<_> = (0..state.queue.len()).collect();
            if state.shuffled {
                state.original_order = state
                    .queue
                    .iter()
                    .map(|track| track.rating_key.clone())
                    .collect();
                fastrand::shuffle(&mut order);
            } else if !state.original_order.is_empty() {
                let mut positions = HashMap::<&str, std::collections::VecDeque<usize>>::new();
                for (index, key) in state.original_order.iter().enumerate() {
                    positions.entry(key.as_str()).or_default().push_back(index);
                }
                order.sort_by_cached_key(|&row| {
                    positions
                        .get_mut(state.queue[row].rating_key.as_str())
                        .and_then(|slots| slots.pop_front())
                        .unwrap_or(usize::MAX)
                });
            }
            if let Some(index) = order.iter().position(|&row| row == state.current) {
                state.current = index;
            }
            state.queue = order
                .into_iter()
                .map(|row| state.queue[row].clone())
                .collect();
            ui.set_shuffled(state.shuffled);
            state.publish_queue(&ui);
        }
    });
    ui.on_repeat({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            state.repeat_mode = (state.repeat_mode + 1) % 3;
            ui.set_repeat_mode(state.repeat_mode);
            state.sync_handoff(&ui, &tx);
        }
    });
    ui.on_mute({
        let (state, weak) = (state.clone(), ui.as_weak());
        move || {
            if let Some(ui) = weak.upgrade() {
                let mut state = state.borrow_mut();
                if !state.muted && state.volume == 0.0 {
                    state.volume = 0.8;
                } else {
                    state.muted = !state.muted;
                }
                state.apply_volume(&ui);
            }
        }
    });
    ui.on_volume({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |volume| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            state.volume = volume.clamp(0.0, 1.0);
            state.muted = false;
            state.apply_volume(&ui);
        }
    });
    ui.on_playlist_selected({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |row| {
            if let Some(ui) = weak.upgrade() {
                state.borrow().select_playlist(&ui, row);
            }
        }
    });
    ui.on_create_playlist({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.trim();
            if name.is_empty() {
                ui.set_playlist_target(-1);
                return;
            }
            let mut state = state.borrow_mut();
            let index = state.data.playlists.len() as i32;
            state.data.playlists.push(Playlist {
                name: name.to_owned(),
                ..Playlist::default()
            });
            ui.set_playlist_target(index);
            ui.set_creating_playlist(false);
            state.publish_playlists(&ui);
            state.save(&ui, "Playlist created");
        }
    });
    ui.on_rename_playlist({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |name| {
            let Some(ui) = weak.upgrade() else { return };
            if name.trim().is_empty() {
                return;
            }
            let mut state = state.borrow_mut();
            if let Some(playlist) = state
                .data
                .playlists
                .get_mut(ui.get_playlist_current() as usize)
            {
                playlist.name = name.trim().to_owned();
                state.publish_playlists(&ui);
                state.save(&ui, "Playlist renamed");
            }
        }
    });
    ui.on_delete_playlist_at({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let Ok(index) = usize::try_from(index) else {
                return;
            };
            if index >= state.data.playlists.len() {
                return;
            }
            state.data.playlists.remove(index);
            let current = ui.get_playlist_current();
            ui.set_playlist_current(match current {
                c if c == index as i32 => -1,
                c if c > index as i32 => c - 1,
                c => c,
            });
            if ui.get_playlist_target() >= index as i32 {
                ui.set_playlist_target(ui.get_playlist_target() - 1);
            }
            state.publish_playlists(&ui);
            state.save(&ui, "Playlist deleted");
        }
    });
    ui.on_add_to_playlist({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |list, track| {
            if let Some(ui) = weak.upgrade() {
                state
                    .borrow_mut()
                    .add_to_playlist(&ui, list as usize, track);
            }
        }
    });
    ui.on_choose_playlist_cover({
        let (state, weak) = (state.clone(), ui.as_weak());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Image", &["png", "jpg", "jpeg", "webp"])
                .pick_file()
            else {
                return;
            };
            let Ok(file) = File::open(path) else { return };
            let mut data = Vec::new();
            if file.take(8 * 1024 * 1024).read_to_end(&mut data).is_err() {
                return;
            }
            let Some(pixels) = decode_cover(&data) else {
                return;
            };
            let mut encoded = Vec::new();
            use image::ImageEncoder;
            if image::codecs::png::PngEncoder::new(&mut encoded)
                .write_image(
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .is_err()
            {
                return;
            }
            let mut state = state.borrow_mut();
            if let Some(playlist) = state
                .data
                .playlists
                .get_mut(ui.get_playlist_current() as usize)
            {
                playlist.cover = encoded;
                state.publish_playlists(&ui);
                state.save(&ui, "Playlist cover saved");
            }
        }
    });
    ui.on_playlist_queue_action({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |index, shuffle| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let Some(playlist) = state.data.playlists.get(index as usize) else {
                return;
            };
            if let Some(id) = &playlist.server_id
                && !state
                    .plex
                    .as_ref()
                    .is_some_and(|plex| plex.server_id == *id)
            {
                ui.set_playback_error("Connect to this playlist's Plex server first.".into());
                return;
            }
            let mut entries = playlist.items.clone();
            if shuffle {
                fastrand::shuffle(&mut entries);
                state.load_queue(&ui, &tx, entries, 0);
            } else {
                state.use_tracks(&ui, &tx, entries, 1, 0);
            }
        }
    });
    ui.on_play_playlist({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |row| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let Some(playlist) = state.data.playlists.get(ui.get_playlist_current() as usize)
            else {
                return;
            };
            if let Some(id) = &playlist.server_id
                && !state
                    .plex
                    .as_ref()
                    .is_some_and(|plex| plex.server_id == *id)
            {
                ui.set_status("Connect to this playlist’s Plex server first.".into());
                return;
            }
            let entries = playlist.items.clone();
            state.load_queue(&ui, &tx, entries, row as usize);
        }
    });
    ui.on_remove_playlist_track({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |row| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let index = ui.get_playlist_current();
            if let Some(playlist) = state.data.playlists.get_mut(index as usize)
                && (row as usize) < playlist.items.len()
            {
                playlist.items.remove(row as usize);
                state.select_playlist(&ui, index);
                state.save(&ui, "Removed from playlist");
            }
        }
    });
    ui.on_move_playlist_track({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |row, delta| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let index = ui.get_playlist_current();
            let (from, to) = (row as usize, row.saturating_add(delta) as usize);
            if let Some(playlist) = state.data.playlists.get_mut(index as usize)
                && from < playlist.items.len()
                && to < playlist.items.len()
            {
                let item = playlist.items.remove(from);
                playlist.items.insert(to, item);
                state.select_playlist(&ui, index);
                state.save(&ui, "Playlist order saved");
            }
        }
    });
    ui.on_save_theme({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |input| {
            let Some(ui) = weak.upgrade() else { return };
            if let Some([r, g, b]) = parse_theme(input.as_str()) {
                ui.set_theme_value(format!("#{r:02x}{g:02x}{b:02x}").into());
                let mut state = state.borrow_mut();
                state.refresh_theme(&ui);
                state.save(&ui, "Theme saved");
            } else {
                ui.set_status("Enter a color such as #204070.".into());
            }
        }
    });
    ui.on_pick_theme_color({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            #[cfg(windows)]
            {
                let seed = parse_theme(ui.get_theme_value().as_str()).unwrap_or([15, 23, 42]);
                if let Some([r, g, b]) = desktop::pick_color(&ui, seed) {
                    ui.invoke_save_theme(format!("#{r:02x}{g:02x}{b:02x}").into());
                }
            }
            #[cfg(not(windows))]
            let _ = ui;
        }
    });
    ui.on_refresh_theme({
        let (state, weak) = (state.clone(), ui.as_weak());
        move || {
            if let Some(ui) = weak.upgrade() {
                state.borrow().refresh_theme(&ui);
            }
        }
    });
    ui.on_refresh_connection({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_connecting() {
                return;
            }
            let Some(plex) = state.borrow().plex.clone() else {
                return;
            };
            ui.set_connecting(true);
            ui.set_refreshing_connection(true);
            ui.set_connection_status("".into());
            let tx = tx.clone();
            thread::spawn(move || {
                let _ = tx.send(Event::PlexConnection(plex.refresh_connection()));
            });
        }
    });
    ui.on_reset_art({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            state.borrow_mut().invalidate_library_cache(&ui, &tx);
            ui.set_cache_status("Cache cleared.".into());
            ui.invoke_refresh_library();
        }
    });
    ui.on_sign_out({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_connecting() {
                return;
            }
            let mut state = state.borrow_mut();
            state.scrobble(&tx);
            state.stop_and_clear(&ui);
            let forget = Plex::forget();
            state.plex = None;
            state.cache_generation += 1;
            ui.set_connection_status("".into());
            state.plex_albums.clear();
            state.tracks_pending.clear();
            state
                .loaded_tracks
                .retain(|key, _| key.starts_with("local-album:"));
            state.albums.clear();
            state.lookup.clear();
            state.tracks.clear();
            state.artwork.clear();
            state.pending_art.clear();
            state.missing_art.clear();
            state.query.clear();
            state.rebuild_library(&ui);
            ui.set_connected(false);
            ui.set_selected_key("".into());
            ui.set_album_title("".into());
            ui.set_album_year("".into());
            ui.set_album_artist("".into());
            ui.set_tracks(ModelRc::default());
            ui.set_status(match forget {
                Ok(()) => "Connect Plex to browse your music.".into(),
                Err(err) => format!(
                    "Signed out for this session. Could not remove the saved Plex login: {err}"
                )
                .into(),
            });
        }
    });
    ui.on_toggle_plex_library({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut state = state.borrow_mut();
                let Some(plex) = state.plex.clone() else {
                    return;
                };
                let Some((key, _)) = plex.sections.get(index as usize).cloned() else {
                    return;
                };
                let mut chosen = plex.chosen_sections(&state.data.preferences.plex_libraries);
                if let Some(at) = chosen.iter().position(|k| *k == key) {
                    if chosen.len() == 1 {
                        ui.set_status("Keep at least one music library selected.".into());
                        return;
                    }
                    chosen.remove(at);
                } else {
                    chosen.push(key);
                }
                // Everything selected is saved as "all", so libraries added later show up too.
                state.data.preferences.plex_libraries = if chosen.len() == plex.sections.len() {
                    Vec::new()
                } else {
                    chosen
                };
                state.publish_plex_libraries(&ui);
                state.save(&ui, "Music libraries saved");
            }
            // Reload albums from the new selection.
            ui.invoke_refresh_library();
        }
    });
    ui.on_lyrics_layout({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |chars| {
            let Some(ui) = weak.upgrade() else { return };
            let Ok(mut state) = state.try_borrow_mut() else {
                return;
            };
            let chars = chars.max(8) as usize;
            if chars != state.lyrics_chars {
                state.lyrics_chars = chars;
                state.present_lyrics(&ui);
            }
        }
    });
    ui.on_toggle_online_lyrics({
        let (state, weak) = (state.clone(), ui.as_weak());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_online_lyrics(!ui.get_online_lyrics());
            let mut state = state.borrow_mut();
            // Let tracks without lyrics be looked up again with the new setting.
            state.lyrics_cache.retain(|_, lines| lines.is_some());
            state.lyrics_key.clear();
            state.save(&ui, "Lyrics setting saved");
        }
    });
    ui.on_toggle_normalizer({
        let (state, weak) = (state.clone(), ui.as_weak());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_normalizer_enabled(!ui.get_normalizer_enabled());
            state
                .borrow()
                .equalizer
                .set_normalizer(ui.get_normalizer_enabled());
        }
    });
    ui.on_eq_band({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |index, gain| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            if let Some(band) = state.eq_gains.get_mut(index as usize) {
                *band = gain.clamp(-12.0, 12.0);
                ui.get_eq_gains().set_row_data(index as usize, *band);
                ui.set_eq_preset_index(6);
                state.update_eq_curve(&ui);
                state.equalizer.update(&state.eq_gains, state.eq_preamp);
            }
        }
    });
    ui.on_eq_level({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |gain| {
            let mut state = state.borrow_mut();
            state.eq_preamp = gain.clamp(-12.0, 12.0);
            if let Some(ui) = weak.upgrade() {
                ui.set_eq_preamp(state.eq_preamp);
            }
            state.equalizer.set_preamp(state.eq_preamp);
        }
    });
    ui.on_eq_preset({
        let (state, weak) = (state.clone(), ui.as_weak());
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let Some(preset) = eq::PRESETS.get(index as usize) else {
                return;
            };
            state.eq_gains = *preset;
            state.publish_eq(&ui);
        }
    });
    ui.on_connect_lastfm({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move |key, secret| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            if state.fm_busy || state.fm_session.is_some() {
                return;
            }
            if key.trim().is_empty() || secret.trim().is_empty() {
                ui.set_lastfm_status("Enter your Last.fm API key and secret.".into());
                return;
            }
            state.fm_busy = true;
            state.fm_generation += 1;
            let generation = state.fm_generation;
            state.fm_pending = None;
            ui.set_lastfm_pending(false);
            ui.set_lastfm_status("Opening Last.fm sign-in…".into());
            let tx = tx.clone();
            let (key, secret) = (key.to_string(), secret.to_string());
            thread::spawn(move || {
                let _ = tx.send(Event::FmPending(
                    generation,
                    lastfm::ClientKeys::authorize(key, secret),
                ));
            });
        }
    });
    ui.on_approve_lastfm({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            let Some(pending) = state.fm_pending.clone() else {
                return;
            };
            if state.fm_busy {
                return;
            }
            state.fm_busy = true;
            let generation = state.fm_generation;
            let tx = tx.clone();
            ui.set_lastfm_status("Checking Last.fm approval…".into());
            thread::spawn(move || {
                let _ = tx.send(Event::FmConnected(generation, pending.finish()));
            });
        }
    });
    ui.on_disconnect_lastfm({
        let (state, weak) = (state.clone(), ui.as_weak());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            state.fm_generation += 1;
            let forget = lastfm::Session::forget();
            state.fm_session = None;
            state.fm_pending = None;
            state.fm_busy = false;
            ui.set_lastfm_connected(false);
            ui.set_lastfm_pending(false);
            ui.set_lastfm_status(match forget {
                Ok(()) => "Disconnected".into(),
                Err(error) => format!(
                    "Disconnected for this session. Could not clear the saved login: {error}"
                )
                .into(),
            });
        }
    });
    ui.on_test_lastfm({
        let (state, weak, tx) = (state.clone(), ui.as_weak(), tx.clone());
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let state = state.borrow();
            let Some(session) = state.fm_session.clone() else {
                return;
            };
            ui.set_lastfm_status("Testing connection…".into());
            let (tx, generation) = (tx.clone(), state.fm_generation);
            thread::spawn(move || {
                let _ = tx.send(Event::FmTest(generation, session.test()));
            });
        }
    });
    {
        let tx = tx.clone();
        thread::spawn(move || match lastfm::Session::restore() {
            Ok(Some(session)) => {
                let _ = tx.send(Event::FmConnected(0, Ok(session)));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = tx.send(Event::FmError(0, error.to_string()));
            }
        });
    }
    if preferences.local_enabled {
        state.borrow_mut().scan_local(&ui, &tx);
    }
    let (discord, discord_messages) = discord::start();
    ui.on_discord_test({
        let (weak, discord) = (ui.as_weak(), discord.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let id = ui.get_discord_id().trim().to_owned();
            if id.is_empty() {
                ui.set_discord_status("Enter your Application ID first.".into());
                return;
            }
            ui.set_discord_status("Connecting…".into());
            let _ = discord.send(discord::Update {
                id,
                image: ui.get_discord_image().trim().to_owned(),
                presence: None,
                test: true,
            });
        }
    });
    let mut last_presence = (String::new(), String::new(), 0, false);
    ui.set_connecting(true);
    {
        let tx = tx.clone();
        let wanted = preferences.plex_libraries.clone();
        thread::spawn(move || {
            let event = match Plex::restore() {
                Ok(Some(plex)) => Event::Library(plex.albums(&wanted).map(|albums| (plex, albums))),
                Ok(None) => Event::NoSavedPlex,
                Err(error) => Event::Library(Err(error)),
            };
            let _ = tx.send(event);
        });
    }
    // Visualizer: each band is scaled against its own recent peak so quiet and loud
    // tracks both move the bars; bars rise immediately and fall back smoothly.
    let viz_timer = Timer::default();
    {
        const FLOORS: [f32; 4] = [0.02, 0.012, 0.006, 0.003];
        let levels = Rc::new(VecModel::from(vec![0.0f32; 4]));
        ui.set_viz_levels(ModelRc::from(levels.clone()));
        let (state, weak) = (state.clone(), ui.as_weak());
        let mut peaks = FLOORS;
        let mut shown = [0.0f32; 4];
        viz_timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
            let Some(ui) = weak.upgrade() else { return };
            // Nothing to animate while paused or while the window is in the tray/minimized.
            if !ui.get_playing() || !window_on_screen(&ui) {
                return;
            }
            let Ok(state) = state.try_borrow() else {
                return;
            };
            let raw = state.equalizer.levels();
            for band in 0..4 {
                peaks[band] = (peaks[band] * 0.996).max(raw[band]).max(FLOORS[band]);
                let target = (raw[band] / peaks[band]).sqrt().clamp(0.0, 1.0);
                shown[band] = if target > shown[band] {
                    target
                } else {
                    shown[band] * 0.55 + target * 0.45
                };
                levels.set_row_data(band, shown[band]);
            }
        });
    }
    let exit_state = state.clone();
    let startup_tx = tx.clone();
    let timer = Timer::default();
    let mut media = media::Media::default();
    let weak = ui.as_weak();
    #[cfg(windows)]
    ui.show()?;
    // The native window only exists once the event loop runs, so the tray (which also
    // provides close-to-tray and restoring the window from a second launch) and the
    // window icons are set up on the first timer tick that has a window handle.
    #[cfg(windows)]
    let mut tray: Option<desktop::Tray> = None;
    #[cfg(windows)]
    let mut window_icons_set = false;
    timer.start(TimerMode::Repeated, Duration::from_millis(120), move || {
        let Some(ui) = weak.upgrade() else { return };
        #[cfg(windows)]
        {
            if tray.is_none() {
                tray = desktop::Tray::attach(&ui);
            }
            if let Some(tray) = &tray {
                instance.activate_if_requested(tray);
            }
            if !window_icons_set {
                window_icons_set = desktop::set_window_icons(&ui);
            }
        }
        let mut state = state.borrow_mut();
        let now = Instant::now();
        if now.duration_since(state.last_save) >= Duration::from_secs(2) {
            state.last_save = now;
            if let Err(error) = state.flush_session(&ui, false) {
                ui.set_status(format!("Could not save the session: {error}").into());
            }
        }
        let delta = now - state.last_tick;
        state.last_tick = now;
        if state.was_playing && ui.get_playing() {
            state.listened += delta.min(Duration::from_millis(600));
        }
        state.was_playing = ui.get_playing();
        state.sync_handoff(&ui, &tx);
        for event in rx.try_iter().take(12) {
            state.receive(&ui, &tx, event);
        }
        if let Some(player) = &state.player {
            if player.sink.empty() {
                state.cancel_handoff();
                if ui.get_playing() {
                    ui.set_playing(false);
                    state.scrobble(&tx);
                    if let Some(error) = state
                        .player
                        .as_ref()
                        .and_then(|player| player.download.as_ref())
                        .and_then(stream::DownloadStatus::error)
                    {
                        ui.set_playback_error(format!("Plex playback: {error}").into());
                    } else {
                        state.advance(&ui, &tx, true);
                    }
                }
            } else {
                let seconds = player.sink.get_pos().as_secs_f32();
                ui.set_position(seconds);
                ui.set_elapsed(clock(seconds as u64).into());
            }
        }
        state.sync_lyrics(&ui, &tx);
        if ui.get_page() != state.last_page {
            state.last_page = ui.get_page();
            if state.last_page == 3 && ui.get_playlist_current() < 0 {
                // Covers for playlist mosaics may already be cached.
                state.publish_playlists(&ui);
            }
        }
        let presence = (
            ui.get_discord_id().trim().to_owned(),
            ui.get_discord_image().trim().to_owned(),
            state.generation,
            ui.get_playing(),
        );
        if presence != last_presence {
            let update = if presence.3 && !presence.0.is_empty() {
                state
                    .queue
                    .get(state.current)
                    .map(|track| discord::Presence {
                        title: track.title.clone(),
                        artist: track.grandparent_title.clone(),
                        album: track.parent_title.clone(),
                    })
            } else {
                None
            };
            let _ = discord.send(discord::Update {
                id: presence.0.clone(),
                image: presence.1.clone(),
                presence: update,
                test: false,
            });
            last_presence = presence;
        }
        if let Some(message) = discord_messages.try_iter().last() {
            ui.set_discord_status(message.into());
        }
        state.preload_next(&ui, &tx);
        state.refresh_track_art(&ui, &tx);
        media.update(&ui, state.queue.get(state.current));
    });
    let tracks = local_entries(std::env::args_os().skip(1).map(PathBuf::from));
    if tracks.is_empty() {
        exit_state.borrow_mut().restore_playback(&ui);
    } else {
        exit_state
            .borrow_mut()
            .load_queue(&ui, &startup_tx, tracks, 0);
        ui.set_page(2);
    }
    ui.run()?;
    exit_state.borrow_mut().flush_session(&ui, true)?;
    Ok(())
}

#[cfg(test)]
mod parity_tests {
    use super::*;

    #[test]
    fn original_default_theme_colors() {
        let [hue, saturation, lightness] = to_hsl([15, 23, 42]);
        assert_eq!(from_hsl(hue, saturation, lightness + 0.10), [28, 44, 80]);
        assert_eq!(from_hsl(hue, saturation, lightness + 0.16), [36, 56, 102]);
        assert_eq!(
            from_hsl(hue, (saturation + 0.15).min(0.8), lightness + 0.28),
            [38, 75, 162]
        );
    }

    #[test]
    fn album_themes_keep_controls_and_labels_readable() {
        for r in (0..=255).step_by(17) {
            for g in (0..=255).step_by(17) {
                for b in (0..=255).step_by(17) {
                    let bg = [r, g, b];
                    let theme = theme_colors(bg, 0);
                    assert!(contrast(theme.accent_text, theme.accent) >= 4.5);
                    for surface in [bg, theme.surface, theme.hover, theme.selected] {
                        assert!(contrast(theme.text, surface) >= 4.5);
                        assert!(contrast(theme.muted, surface) >= 4.5);
                        assert!(contrast(theme.accent_ink, surface) >= 4.5);
                    }
                }
            }
        }
    }

    #[test]
    fn original_search_ignores_punctuation_and_matches_all_words() {
        assert!(library_matches(&["blink", "182"], "Blink-182"));
        assert!(!library_matches(&["blink", "182"], "Blink"));
    }
}

#[cfg(test)]
mod drag_tests {
    use super::*;
    use i_slint_backend_testing::ElementQuery;
    use slint::{
        LogicalPosition, PhysicalSize,
        platform::{PointerEventButton, WindowEvent},
    };

    fn drag(window: &slint::Window, start: LogicalPosition, distance: f32) {
        let end = LogicalPosition::new(start.x, start.y + distance);
        let button = PointerEventButton::Left;
        window.dispatch_event(WindowEvent::PointerMoved { position: start });
        window.dispatch_event(WindowEvent::PointerPressed {
            position: start,
            button,
        });
        window.dispatch_event(WindowEvent::PointerMoved { position: end });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: end,
            button,
        });
    }

    fn click(window: &slint::Window, position: LogicalPosition, button: PointerEventButton) {
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    fn center(element: &i_slint_backend_testing::ElementHandle) -> LogicalPosition {
        let origin = element.absolute_position();
        let size = element.size();
        LogicalPosition::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0)
    }

    fn grips(ui: &Sonic) -> Vec<i_slint_backend_testing::ElementHandle> {
        let mut handles = ElementQuery::from_root(ui)
            .match_accessible_label("Drag to reorder")
            .find_all();
        handles.sort_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y));
        handles
    }

    #[test]
    fn pointer_drag_reorders_tracks_and_playback_errors_can_be_dismissed() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = Sonic::new().unwrap();
        ui.window().set_size(PhysicalSize::new(1220, 760));
        ui.show().unwrap();
        let tracks =
            [("A", "First"), ("A", "Second"), ("B", "Third")].map(|(album, title)| QueueTrack {
                title: title.into(),
                album: album.into(),
                album_key: album.into(),
                ..QueueTrack::default()
            });
        let lines = queue_lines(&tracks, 0);
        let group_title = &lines[0];
        assert_eq!(
            (group_title.index, group_title.track.album.as_str()),
            (-1, "A")
        );
        ui.set_queue(ModelRc::new(VecModel::from(tracks.to_vec())));
        ui.set_queue_lines(ModelRc::new(VecModel::from(lines.clone())));
        ui.set_current_track(0);
        ui.set_page(2);
        ui.on_queue_drop_index(move |index, distance| {
            queue_drop_index(lines.iter().cloned(), index, distance)
        });
        let queue_move = Rc::new(std::cell::Cell::new(None));
        let result = queue_move.clone();
        ui.on_move_queue(move |row, delta| result.set(Some((row, delta))));
        // Space plays and pauses from anywhere, including after clicking a row.
        let toggles = Rc::new(std::cell::Cell::new(0));
        let counted = toggles.clone();
        ui.on_toggle_play(move || counted.set(counted.get() + 1));
        let space = |ui: &Sonic| {
            ui.window()
                .dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
            ui.window()
                .dispatch_event(WindowEvent::KeyReleased { text: " ".into() });
        };
        space(&ui);
        assert_eq!(toggles.get(), 1);
        // Only album-grouped rows get a drag handle, like the original Queue page.
        let handles = grips(&ui);
        assert_eq!(handles.len(), 2);
        let from = center(&handles[0]);
        drag(ui.window(), from, center(&handles[1]).y - from.y);
        assert_eq!(queue_move.get(), Some((0, 1)));

        // At rest a grouped row shows its duration; hovering swaps in the remove button.
        let removes = |ui: &Sonic| {
            ElementQuery::from_root(ui)
                .match_accessible_label("Remove from queue")
                .find_all()
                .into_iter()
                .filter(|e| e.absolute_position().x > 260.0)
                .count()
        };
        ui.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(1150.0, 700.0),
        });
        assert_eq!(removes(&ui), 0);
        ui.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(from.x + 200.0, from.y),
        });
        assert_eq!(removes(&ui), 1);
        // Right-clicking a Queue row opens Sonic's own menu with all three actions.
        ui.set_playlists(ModelRc::new(VecModel::from(vec![
            slint::SharedString::from("Fixture"),
        ])));
        let saved = Rc::new(std::cell::Cell::new(None));
        let result = saved.clone();
        ui.on_queue_track_action(move |row, action, playlist| {
            result.set(Some((row, action, playlist)))
        });
        let row = LogicalPosition::new(from.x + 200.0, from.y);
        click(
            ui.window(),
            LogicalPosition::new(row.x, center(&handles[1]).y),
            PointerEventButton::Left,
        );
        space(&ui);
        assert_eq!(
            toggles.get(),
            2,
            "Space still toggles after a row has focus"
        );
        click(ui.window(), row, PointerEventButton::Right);
        let item = |label: &str| {
            ElementQuery::from_root(&ui)
                .match_accessible_label(label)
                .find_first()
                .unwrap_or_else(|| panic!("menu item {label}"))
        };
        for label in ["Add to queue", "Shuffle play", "Add to playlist"] {
            item(label);
        }
        // A real pointer click on the item runs it and closes the menu.
        click(
            ui.window(),
            center(&item("Add to queue")),
            PointerEventButton::Left,
        );
        assert_eq!(saved.get(), Some((0, 1, -1)));
        assert!(
            ElementQuery::from_root(&ui)
                .match_accessible_label("Shuffle play")
                .find_first()
                .is_none()
        );
        // "Add to playlist" opens the picker, and choosing a playlist saves the row there.
        click(ui.window(), row, PointerEventButton::Right);
        click(
            ui.window(),
            center(&item("Add to playlist")),
            PointerEventButton::Left,
        );
        item("Fixture").invoke_accessible_default_action();
        assert_eq!(saved.get(), Some((0, 3, 0)));

        ui.set_page(3);
        ui.set_playlist_current(0);
        ui.set_playlist_name("Fixture".into());
        ui.set_playlist_items(ModelRc::new(VecModel::from(tracks.to_vec())));
        item("Playlist options").invoke_accessible_default_action();
        item("Edit playlist").invoke_accessible_default_action();
        let playlist_move = Rc::new(std::cell::Cell::new(None));
        let result = playlist_move.clone();
        ui.on_move_playlist_track(move |row, delta| result.set(Some((row, delta))));
        let handles = grips(&ui);
        assert_eq!(handles.len(), 3);
        let from = center(&handles[2]);
        drag(ui.window(), from, center(&handles[0]).y - from.y);
        assert_eq!(playlist_move.get(), Some((2, -2)));

        ui.set_page(0);
        ui.set_playback_error("Playback: test failure".into());
        ElementQuery::from_root(&ui)
            .match_accessible_label("Dismiss")
            .find_first()
            .expect("playback error is visible outside Settings")
            .invoke_accessible_default_action();
        assert!(ui.get_playback_error().is_empty());

        ui.set_sidebar_width(335.0);
        ui.window().set_size(PhysicalSize::new(800, 650));
        let now_playing = ElementQuery::from_root(&ui)
            .match_accessible_label("Now Playing")
            .find_all()
            .into_iter()
            .find(|item| item.absolute_position().y > 550.0)
            .expect("Now Playing stays available in the compact player");
        assert!(center(&now_playing).x < 800.0);
        now_playing.invoke_accessible_default_action();
        assert_eq!(ui.get_page(), 1);

        // Drive the real gallery with a small fixture, including its resize callback.
        ui.set_page(0);
        ui.set_albums(ModelRc::new(VecModel::from(
            (0..6)
                .map(|index| Album {
                    key: index.to_string().into(),
                    title: format!("Album {index}").into(),
                    ..Album::default()
                })
                .collect::<Vec<_>>(),
        )));
        let set_rows = |ui: &Sonic, columns: i32| {
            ui.set_album_rows(ModelRc::new(VecModel::from(
                (0..6)
                    .step_by(columns as usize)
                    .map(|offset| AlbumRow {
                        offset,
                        count: columns.min(6 - offset),
                        has_year: false,
                        heading: "".into(),
                    })
                    .collect::<Vec<_>>(),
            )));
        };
        set_rows(&ui, 2);
        let weak = ui.as_weak();
        ui.on_request_visible_art(move |_, _, _, _, columns| {
            if let Some(ui) = weak.upgrade() {
                set_rows(&ui, columns);
            }
        });
        ui.set_connected(true);
        let album = |index| {
            ElementQuery::from_root(&ui)
                .match_accessible_label(&format!("Album {index}"))
                .find_first()
                .unwrap()
        };
        let initial = center(&album(0));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
        assert!(center(&album(2)).y > initial.y);
        ui.window().set_size(PhysicalSize::new(1220, 760));
        let _ = center(&album(0));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
        assert_eq!(center(&album(2)).y, center(&album(0)).y);

        let weak = ui.as_weak();
        ui.on_select_album(move |key| weak.unwrap().set_selected_key(key));
        let actions = Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = actions.clone();
        ui.on_album_action(move |key, action, _| observed.borrow_mut().push((key, action)));
        let location = center(&album(2));
        drag(ui.window(), location, 0.0);
        assert_eq!(ui.get_selected_key(), "2");
        assert!(actions.borrow().is_empty());
        drag(ui.window(), location, 0.0);
        assert_eq!(actions.borrow().as_slice(), &[("2".into(), 0)]);
        // Album tiles use the same menu: Shuffle play sends action 2 for that album.
        click(ui.window(), location, PointerEventButton::Right);
        let shuffle = ElementQuery::from_root(&ui)
            .match_accessible_label("Shuffle play")
            .find_first()
            .expect("album menu");
        click(ui.window(), center(&shuffle), PointerEventButton::Left);
        assert_eq!(actions.borrow().last(), Some(&("2".into(), 2)));

        let query = Rc::new(std::cell::RefCell::new(slint::SharedString::default()));
        let typed = query.clone();
        ui.on_search(move |text| *typed.borrow_mut() = text);
        let search = ElementQuery::from_root(&ui)
            .match_accessible_label("Search songs and albums…")
            .find_first()
            .expect("themed search input");
        drag(ui.window(), center(&search), 0.0);
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: "a".into() });
        assert_eq!(query.borrow().as_str(), "a");
        // While typing in search, Space types a space instead of pausing.
        let before = toggles.get();
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
        assert_eq!(toggles.get(), before);
        assert_eq!(query.borrow().as_str(), "a ");

        ui.set_page(4);
        let weak = ui.as_weak();
        ui.on_eq_level(move |gain| weak.unwrap().set_eq_preamp(gain));
        let preamp = ElementQuery::from_root(&ui)
            .match_accessible_label("Preamp gain")
            .find_first()
            .expect("preamp slider in Playback settings");
        let start = preamp.absolute_position();
        let size = preamp.size();
        for (fraction, expected) in [(0.25, -6.0), (0.75, 6.0)] {
            let position =
                LogicalPosition::new(start.x + size.width * fraction, start.y + size.height / 2.0);
            drag(ui.window(), position, 0.0);
            assert_eq!(ui.get_eq_preamp(), expected);
        }
    }
}
