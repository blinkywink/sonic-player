//! Listen along ("jam"): apps signed in to the same Plex account share one queue
//! and play state through a tiny relay (relay/relay.py, on the Plex server's
//! machine). Each app streams the music from Plex itself; only the queue, the
//! current song, play/pause and the position travel through the relay.

use crate::plex::Entry;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// The shared playback: what everyone in the jam plays.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Playback {
    pub queue: Vec<Entry>,
    pub current: usize,
    pub playing: bool,
    /// Seconds into the current song when the state was set...
    pub position: f32,
    /// ...and when that was (relay clock, ms), so everyone can work out "now".
    #[serde(default)]
    pub position_at: u64,
    /// Which app set it.
    #[serde(default)]
    pub by: String,
}

impl Playback {
    /// Song keys, to tell whether the queue changed.
    pub fn keys(&self) -> Vec<String> {
        self.queue.iter().map(|track| track.rating_key.clone()).collect()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Member {
    pub client: String,
    pub name: String,
    /// A small JPEG, base64.
    #[serde(default)]
    pub avatar: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Snapshot {
    pub version: u64,
    pub server_now: u64,
    pub state: Option<Playback>,
    #[serde(default)]
    pub members: Vec<Member>,
}

impl Snapshot {
    /// Where the shared song is right now, in seconds, given when this snapshot
    /// arrived.
    pub fn position_now(&self, received: Instant) -> f32 {
        let Some(state) = &self.state else { return 0.0 };
        if !state.playing {
            return state.position;
        }
        let since_set = self.server_now.saturating_sub(state.position_at) as f32 / 1000.0;
        state.position + since_set + received.elapsed().as_secs_f32()
    }
}

pub enum Message {
    /// The jam changed (or this is the first look at it).
    Snapshot(Snapshot, Instant),
    /// Connection news for the jam bar ("" when all is well).
    Status(String),
}

struct Inner {
    http: reqwest::blocking::Client,
    candidates: Vec<String>,
    base: Mutex<Option<String>>,
    token: String,
    client: String,
    me: Mutex<(String, String)>,
    joined: AtomicBool,
    stopped: AtomicBool,
    version: AtomicU64,
    send: Box<dyn Fn(Message) + Send + Sync>,
}

/// A connection to the jam. Watching starts right away (so the app can show who's
/// listening); `join` makes you part of it.
#[derive(Clone)]
pub struct Jam(Arc<Inner>);

impl Jam {
    /// `candidates` are relay addresses to try, best first (e.g. the Plex machine
    /// on the home network, then a public address).
    pub fn start(
        candidates: Vec<String>,
        token: String,
        client: String,
        name: String,
        avatar: String,
        send: impl Fn(Message) + Send + Sync + 'static,
    ) -> Self {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(4))
            .timeout(Duration::from_secs(40))
            .build()
            .unwrap_or_default();
        let jam = Self(Arc::new(Inner {
            http,
            candidates,
            base: Mutex::new(None),
            token,
            client,
            me: Mutex::new((name, avatar)),
            joined: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            version: AtomicU64::new(0),
            send: Box::new(send),
        }));
        let watcher = jam.clone();
        thread::spawn(move || watcher.watch());
        jam
    }

    pub fn joined(&self) -> bool {
        self.0.joined.load(Ordering::Relaxed)
    }

    pub fn client(&self) -> &str {
        &self.0.client
    }

    pub fn join(&self) {
        self.0.joined.store(true, Ordering::Relaxed);
        let jam = self.clone();
        thread::spawn(move || jam.check_in());
    }

    pub fn leave(&self) {
        self.0.joined.store(false, Ordering::Relaxed);
        let jam = self.clone();
        thread::spawn(move || {
            let body = serde_json::json!({ "client": jam.0.client });
            jam.post("/jam/leave", &body);
        });
    }

    /// Your name or picture changed.
    pub fn set_me(&self, name: String, avatar: String) {
        if let Ok(mut me) = self.0.me.lock() {
            *me = (name, avatar);
        }
        if self.joined() {
            let jam = self.clone();
            thread::spawn(move || jam.check_in());
        }
    }

    /// Shares your playback with the jam.
    pub fn push(&self, playback: Playback) {
        if !self.joined() {
            return;
        }
        let jam = self.clone();
        thread::spawn(move || {
            let body = serde_json::json!({ "client": jam.0.client, "state": playback });
            jam.post("/jam/state", &body);
        });
    }

    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::Relaxed);
        if self.joined() {
            self.leave();
        }
    }

    fn check_in(&self) {
        let (name, avatar) = self.0.me.lock().map(|me| me.clone()).unwrap_or_default();
        let body = serde_json::json!({ "client": self.0.client, "name": name, "avatar": avatar });
        self.post("/jam/member", &body);
    }

    fn post(&self, path: &str, body: &serde_json::Value) {
        let Some(base) = self.base() else { return };
        let result = self
            .0
            .http
            .post(format!("{base}{path}"))
            .header("X-Plex-Token", &self.0.token)
            .timeout(Duration::from_secs(10))
            .json(body)
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json::<Snapshot>());
        match result {
            Ok(snapshot) => self.deliver(snapshot),
            Err(_) => (self.0.send)(Message::Status("Can't reach the jam server right now.".into())),
        }
    }

    fn deliver(&self, snapshot: Snapshot) {
        // Any change counts (a restarted relay starts its versions over).
        let previous = self.0.version.swap(snapshot.version, Ordering::AcqRel);
        if snapshot.version != previous {
            (self.0.send)(Message::Snapshot(snapshot, Instant::now()));
        }
    }

    /// The relay address that answers, found by trying the candidates in order.
    fn base(&self) -> Option<String> {
        if let Some(base) = self.0.base.lock().ok().and_then(|base| base.clone()) {
            return Some(base);
        }
        let found = self.0.candidates.iter().find(|candidate| {
            self.0
                .http
                .get(format!("{candidate}/jam/ping"))
                .timeout(Duration::from_secs(3))
                .send()
                .is_ok_and(|response| response.status().is_success())
        })?;
        if let Ok(mut base) = self.0.base.lock() {
            *base = Some(found.clone());
        }
        Some(found.clone())
    }

    /// Long-polls the jam, delivering each change; checks in every 20 seconds
    /// while joined; finds the relay again after the network changes.
    fn watch(&self) {
        let mut last_check_in = Instant::now() - Duration::from_secs(60);
        let mut failures = 0u32;
        while !self.0.stopped.load(Ordering::Relaxed) {
            if self.joined() && last_check_in.elapsed() > Duration::from_secs(20) {
                last_check_in = Instant::now();
                self.check_in();
            }
            let Some(base) = self.base() else {
                (self.0.send)(Message::Status(
                    "No jam server found. Check the jam server address in Settings.".into(),
                ));
                thread::sleep(Duration::from_secs(10));
                continue;
            };
            let since = self.0.version.load(Ordering::Acquire);
            let result = self
                .0
                .http
                .get(format!("{base}/jam/state"))
                .query(&[("since", since.to_string()), ("client", self.0.client.clone())])
                .header("X-Plex-Token", &self.0.token)
                .send()
                .and_then(|response| response.error_for_status())
                .and_then(|response| response.json::<Snapshot>());
            match result {
                Ok(snapshot) => {
                    if failures > 0 {
                        (self.0.send)(Message::Status(String::new()));
                    }
                    failures = 0;
                    self.deliver(snapshot);
                }
                Err(_) => {
                    failures += 1;
                    if failures >= 2 {
                        // Maybe the network changed: look for the relay again.
                        if let Ok(mut base) = self.0.base.lock() {
                            *base = None;
                        }
                        (self.0.send)(Message::Status("Reconnecting to the jam…".into()));
                    }
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }
}

/// Relay addresses to try: the Plex machine directly (fast on the home network),
/// then the address from Settings (works from anywhere).
pub fn candidates(plex_server: Option<url::Url>, configured: &str) -> Vec<String> {
    let mut list = Vec::new();
    if let Some(host) = plex_server.as_ref().and_then(|url| url.host_str().map(str::to_owned))
        && !host.ends_with(".plex.direct")
    {
        list.push(format!("http://{host}:8790"));
    }
    let configured = configured.trim().trim_end_matches('/');
    if !configured.is_empty() {
        list.push(if configured.contains("://") { configured.to_owned() } else { format!("https://{configured}") });
    }
    list
}

/// A random id for this install of the app.
pub fn new_client_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Turns a picture into a small square JPEG (96 px) for the jam, base64.
pub fn avatar_from_image(bytes: &[u8]) -> Option<String> {
    let image = image::load_from_memory(bytes).ok()?;
    let small = image.resize_to_fill(96, 96, image::imageops::FilterType::Triangle).into_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 82)
        .encode_image(&small)
        .ok()?;
    Some(base64_encode(&jpeg))
}

/// A member's picture as pixels, if they set one.
pub fn avatar_pixels(avatar: &str) -> Option<image::RgbaImage> {
    let bytes = base64_decode(avatar)?;
    Some(image::load_from_memory(&bytes).ok()?.into_rgba8())
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=') {
        let value = ALPHABET.iter().position(|&c| c == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"hello jam!", &[0, 255, 128, 7]] {
            assert_eq!(base64_decode(&base64_encode(data)).as_deref(), Some(data));
        }
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn the_shared_position_moves_while_playing() {
        let snapshot = Snapshot {
            version: 2,
            server_now: 10_000,
            state: Some(Playback { playing: true, position: 30.0, position_at: 8_000, ..Default::default() }),
            members: Vec::new(),
        };
        let at = snapshot.position_now(Instant::now());
        assert!((at - 32.0).abs() < 0.2, "{at}");
    }
}
