//! iOS glue: the playback audio session, the system media controls (lock screen,
//! Control Center, headphones, car) and the Now Playing info they show.

use crate::{NowPlaying, Remote};
use block2::RcBlock;
use objc2::{
    Encode, Encoding, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyClass, AnyObject, Bool},
};
use objc2_foundation::NSString;
use std::{cell::RefCell, ptr::null_mut, sync::Mutex};

#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {}
#[link(name = "UIKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "MediaPlayer", kind = "framework")]
unsafe extern "C" {
    static MPMediaItemPropertyTitle: &'static NSString;
    static MPMediaItemPropertyArtist: &'static NSString;
    static MPMediaItemPropertyAlbumTitle: &'static NSString;
    static MPMediaItemPropertyPlaybackDuration: &'static NSString;
    static MPMediaItemPropertyArtwork: &'static NSString;
    static MPNowPlayingInfoPropertyElapsedPlaybackTime: &'static NSString;
    static MPNowPlayingInfoPropertyPlaybackRate: &'static NSString;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

/// Commands from the system controls, picked up by the app's timer.
static COMMANDS: Mutex<Vec<Remote>> = Mutex::new(Vec::new());

/// Links to open in Safari; UIKit must be called on the main thread.
static URLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Opens a link in Safari (callable from any thread).
pub fn open_url(url: &str) {
    if let Ok(mut urls) = URLS.lock() {
        urls.push(url.to_owned());
    }
}

/// Opens queued links; called from the main thread's timer.
pub fn open_pending() {
    let urls = URLS.lock().map(|mut urls| std::mem::take(&mut *urls)).unwrap_or_default();
    let (Some(app_class), Some(url_class), Some(dict_class)) =
        (class(c"UIApplication"), class(c"NSURL"), class(c"NSDictionary"))
    else {
        return;
    };
    for url in urls {
        unsafe {
            let app: *mut AnyObject = msg_send![app_class, sharedApplication];
            let link: Option<Retained<AnyObject>> = msg_send![url_class, URLWithString: &*NSString::from_str(&url)];
            let options: Option<Retained<AnyObject>> = msg_send![dict_class, dictionary];
            let (Some(link), Some(options)) = (link, options) else { continue };
            if app.is_null() {
                continue;
            }
            let _: () = msg_send![app, openURL: &*link, options: &*options, completionHandler: null_mut::<AnyObject>()];
        }
    }
}

thread_local! {
    /// The lock screen artwork and the album it belongs to.
    static ARTWORK: RefCell<Option<(String, Retained<AnyObject>)>> = const { RefCell::new(None) };
}

fn class(name: &std::ffi::CStr) -> Option<&'static AnyClass> {
    AnyClass::get(name)
}

pub fn start_audio_session() {
    let Some(class) = class(c"AVAudioSession") else { return };
    unsafe {
        let session: *mut AnyObject = msg_send![class, sharedInstance];
        if session.is_null() {
            return;
        }
        let category = NSString::from_str("AVAudioSessionCategoryPlayback");
        let _: Bool = msg_send![session, setCategory: &*category, error: null_mut::<*mut AnyObject>()];
        let _: Bool = msg_send![session, setActive: Bool::YES, error: null_mut::<*mut AnyObject>()];
    }
}

/// Hooks up play/pause, next/previous and scrubbing from the system controls.
pub fn start_remote_commands() {
    let Some(center_class) = class(c"MPRemoteCommandCenter") else { return };
    unsafe {
        let center: *mut AnyObject = msg_send![center_class, sharedCommandCenter];
        if center.is_null() {
            return;
        }
        let commands: [(*mut AnyObject, Remote); 5] = [
            (msg_send![center, playCommand], Remote::Play),
            (msg_send![center, pauseCommand], Remote::Pause),
            (msg_send![center, togglePlayPauseCommand], Remote::Toggle),
            (msg_send![center, nextTrackCommand], Remote::Next),
            (msg_send![center, previousTrackCommand], Remote::Previous),
        ];
        for (command, remote) in commands {
            if command.is_null() {
                continue;
            }
            let handler = RcBlock::new(move |_event: *mut AnyObject| -> isize {
                push(remote);
                0 // MPRemoteCommandHandlerStatusSuccess
            });
            let _: *mut AnyObject = msg_send![command, addTargetWithHandler: &*handler];
            let _: () = msg_send![command, setEnabled: Bool::YES];
        }
        let seek: *mut AnyObject = msg_send![center, changePlaybackPositionCommand];
        if !seek.is_null() {
            let handler = RcBlock::new(|event: *mut AnyObject| -> isize {
                if event.is_null() {
                    return 1;
                }
                let seconds: f64 = msg_send![event, positionTime];
                push(Remote::Seek(seconds));
                0
            });
            let _: *mut AnyObject = msg_send![seek, addTargetWithHandler: &*handler];
            let _: () = msg_send![seek, setEnabled: Bool::YES];
        }
    }
}

fn push(remote: Remote) {
    if let Ok(mut commands) = COMMANDS.lock() {
        commands.push(remote);
    }
}

pub fn take_commands() -> Vec<Remote> {
    COMMANDS.lock().map(|mut commands| std::mem::take(&mut *commands)).unwrap_or_default()
}

/// Whether the app is on screen (iOS doesn't draw it while in the background).
pub fn is_active() -> bool {
    let Some(app_class) = class(c"UIApplication") else { return true };
    unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return true;
        }
        let state: isize = msg_send![app, applicationState];
        state == 0 // UIApplicationStateActive
    }
}

/// Album key whose artwork is currently loaded for the lock screen.
pub fn artwork_key() -> Option<String> {
    ARTWORK.with(|art| art.borrow().as_ref().map(|(key, _)| key.clone()))
}

/// Sets the lock screen artwork from encoded image bytes (JPEG or PNG).
pub fn set_artwork(key: &str, bytes: &[u8]) {
    let (Some(data_class), Some(image_class), Some(artwork_class)) =
        (class(c"NSData"), class(c"UIImage"), class(c"MPMediaItemArtwork"))
    else {
        return;
    };
    unsafe {
        let data: Option<Retained<AnyObject>> =
            msg_send![data_class, dataWithBytes: bytes.as_ptr().cast::<std::ffi::c_void>(), length: bytes.len()];
        let Some(data) = data else { return };
        let image: Option<Retained<AnyObject>> = msg_send![image_class, imageWithData: &*data];
        let Some(image) = image else { return };
        let size: CGSize = msg_send![&*image, size];
        let keep = image.clone();
        let handler = RcBlock::new(move |_size: CGSize| -> *mut AnyObject {
            Retained::as_ptr(&keep).cast_mut()
        });
        let allocated: Allocated<AnyObject> = msg_send![artwork_class, alloc];
        let artwork: Option<Retained<AnyObject>> =
            msg_send![allocated, initWithBoundsSize: size, requestHandler: &*handler];
        if let Some(artwork) = artwork {
            ARTWORK.with(|art| *art.borrow_mut() = Some((key.to_owned(), artwork)));
        }
    }
}

/// Shows (or clears) the song on the lock screen and in Control Center.
pub fn now_playing(info: Option<&NowPlaying>) {
    let Some(center_class) = class(c"MPNowPlayingInfoCenter") else { return };
    unsafe {
        let center: *mut AnyObject = msg_send![center_class, defaultCenter];
        if center.is_null() {
            return;
        }
        let Some(info) = info else {
            let _: () = msg_send![center, setNowPlayingInfo: null_mut::<AnyObject>()];
            return;
        };
        let (Some(dict_class), Some(number_class)) = (class(c"NSMutableDictionary"), class(c"NSNumber"))
        else {
            return;
        };
        let dict: Option<Retained<AnyObject>> = msg_send![dict_class, dictionary];
        let Some(dict) = dict else { return };
        let set = |key: &NSString, value: &AnyObject| {
            let _: () = msg_send![&*dict, setObject: value, forKey: key];
        };
        let number = |value: f64| -> Option<Retained<AnyObject>> { msg_send![number_class, numberWithDouble: value] };
        for (key, text) in [
            (MPMediaItemPropertyTitle, &info.title),
            (MPMediaItemPropertyArtist, &info.artist),
            (MPMediaItemPropertyAlbumTitle, &info.album),
        ] {
            if !text.is_empty() {
                set(key, &NSString::from_str(text));
            }
        }
        for (key, value) in [
            (MPMediaItemPropertyPlaybackDuration, info.duration),
            (MPNowPlayingInfoPropertyElapsedPlaybackTime, info.elapsed),
            (MPNowPlayingInfoPropertyPlaybackRate, if info.playing { 1.0 } else { 0.0 }),
        ] {
            if let Some(value) = number(value) {
                set(key, &value);
            }
        }
        ARTWORK.with(|art| {
            if let Some((key, artwork)) = art.borrow().as_ref()
                && *key == info.art_key
            {
                set(MPMediaItemPropertyArtwork, artwork);
            }
        });
        let _: () = msg_send![center, setNowPlayingInfo: &*dict];
    }
}
