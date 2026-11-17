use crate::{Sonic, plex::Entry};
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::ComponentHandle;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use std::time::Duration;

#[derive(Default)]
pub struct Media {
    controls: Option<MediaControls>,
    attempted: bool,
    key: String,
    /// Track whose cover has been handed to Windows (art arrives after the metadata).
    art_key: String,
    last: Option<MediaPlayback>,
}

impl Media {
    pub fn update(&mut self, ui: &Sonic, track: Option<&Entry>) {
        // Register with Windows as soon as there is something to play, so media keys and
        // the system media flyout work even when Sonic starts with a paused track.
        if !self.attempted && (ui.get_playing() || track.is_some()) {
            self.controls = connect(ui);
            self.attempted = true;
        }
        let Some(controls) = &mut self.controls else {
            return;
        };
        if let Some(track) = track {
            let cover = (track.rating_key != self.art_key)
                .then(|| cover_file(ui))
                .flatten();
            if track.rating_key != self.key || cover.is_some() {
                let url = cover.as_ref().map(|path| format!("file://{}", path.display()));
                let metadata = MediaMetadata {
                    title: Some(&track.title),
                    artist: Some(&track.grandparent_title),
                    album: Some(&track.parent_title),
                    duration: Some(Duration::from_millis(track.duration)),
                    cover_url: url.as_deref(),
                };
                if controls.set_metadata(metadata).is_ok() {
                    if track.rating_key != self.key {
                        // New track: its cover still has to be sent once it loads.
                        self.art_key.clear();
                    }
                    self.key.clone_from(&track.rating_key);
                    if cover.is_some() {
                        self.art_key.clone_from(&track.rating_key);
                    }
                }
            }
        }
        let progress = Some(MediaPosition(Duration::from_secs(
            ui.get_position().max(0.0) as u64,
        )));
        let current = if track.is_none() {
            MediaPlayback::Stopped
        } else if ui.get_playing() {
            MediaPlayback::Playing { progress }
        } else {
            MediaPlayback::Paused { progress }
        };
        if self.last.as_ref() != Some(&current) && controls.set_playback(current.clone()).is_ok() {
            self.last = Some(current);
        }
    }
}

/// Writes the now-playing cover to a temp file for the system media flyout.
fn cover_file(ui: &Sonic) -> Option<std::path::PathBuf> {
    let art = ui.get_playing_art().to_rgba8()?;
    if art.width() == 0 || art.height() == 0 {
        return None;
    }
    let path = std::env::temp_dir().join(format!("sonic-cover-{}.png", std::process::id()));
    image::save_buffer(&path, art.as_bytes(), art.width(), art.height(), image::ColorType::Rgba8).ok()?;
    Some(path)
}

fn connect(ui: &Sonic) -> Option<MediaControls> {
    #[cfg(windows)]
    let hwnd = {
        let handle = ui.window().window_handle();
        let RawWindowHandle::Win32(handle) = handle.window_handle().ok()?.as_raw() else {
            return None;
        };
        Some(handle.hwnd.get() as *mut _)
    };
    #[cfg(not(windows))]
    let hwnd = None;
    let mut controls = MediaControls::new(PlatformConfig {
        dbus_name: "sonic",
        display_name: "Sonic",
        hwnd,
    })
    .ok()?;
    let window = ui.as_weak();
    controls
        .attach(move |event| {
            let _ = window.upgrade_in_event_loop(move |ui| match event {
                MediaControlEvent::Play if !ui.get_playing() => ui.invoke_toggle_play(),
                MediaControlEvent::Pause if ui.get_playing() => ui.invoke_toggle_play(),
                MediaControlEvent::Toggle => ui.invoke_toggle_play(),
                MediaControlEvent::Next => ui.invoke_next_track(),
                MediaControlEvent::Previous => ui.invoke_previous_track(),
                MediaControlEvent::Stop => {
                    if ui.get_playing() {
                        ui.invoke_toggle_play();
                    }
                    if ui.get_duration() > 0.0 {
                        ui.invoke_seek(0.0);
                    }
                }
                MediaControlEvent::SetPosition(position) => {
                    ui.invoke_seek(position.0.as_secs_f32())
                }
                MediaControlEvent::Seek(direction) => {
                    let delta = if direction == SeekDirection::Forward {
                        10.0
                    } else {
                        -10.0
                    };
                    ui.invoke_seek((ui.get_position() + delta).clamp(0.0, ui.get_duration()));
                }
                _ => {}
            });
        })
        .ok()?;
    Some(controls)
}
