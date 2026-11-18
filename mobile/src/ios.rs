//! iOS-only setup: an audio session that keeps playing in the background, on the
//! lock screen and with the silent switch on.
use objc2::{msg_send, runtime::{AnyClass, AnyObject, Bool}};
use objc2_foundation::NSString;
use std::ptr::null_mut;

#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {}

pub fn start_audio_session() {
    let Some(class) = AnyClass::get(c"AVAudioSession") else {
        return;
    };
    unsafe {
        let session: *mut AnyObject = msg_send![class, sharedInstance];
        if session.is_null() {
            return;
        }
        // The value of AVAudioSessionCategoryPlayback is its own name.
        let category = NSString::from_str("AVAudioSessionCategoryPlayback");
        let _: Bool = msg_send![session, setCategory: &*category, error: null_mut::<*mut AnyObject>()];
        let _: Bool = msg_send![session, setActive: Bool::YES, error: null_mut::<*mut AnyObject>()];
    }
}
