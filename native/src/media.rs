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
    last: Option<MediaPlayback>,
}

impl Media {
    pub fn update(&mut self, ui: &Sonic, track: Option<&Entry>) {
        if !self.attempted && ui.get_playing() {
            self.controls = connect(ui);
            self.attempted = true;
        }
        let Some(controls) = &mut self.controls else {
            return;
        };
        if let Some(track) = track
            && track.rating_key != self.key
        {
            let metadata = MediaMetadata {
                title: Some(&track.title),
                artist: Some(&track.grandparent_title),
                album: Some(&track.parent_title),
                duration: Some(Duration::from_millis(track.duration)),
                cover_url: None,
            };
            if controls.set_metadata(metadata).is_ok() {
                self.key.clone_from(&track.rating_key);
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
