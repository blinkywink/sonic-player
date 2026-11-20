//! Streams a song into a temporary file while it plays. The download keeps track
//! of which byte ranges it has, so it can:
//! - reconnect from the exact byte where a connection dropped (switching networks
//!   mid-song, a tunnel, a router hiccup) instead of starting over;
//! - jump straight to where you seek instead of waiting for everything before it;
//! - fetch big songs with a second connection from the middle, to finish sooner;
//! - keep the finished song in the disk cache, so it plays offline next time.

use crate::plex::{Entry, Plex};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const LIMIT: u64 = 256 * 1024 * 1024;
/// Songs at least this big are also fetched from the middle on a second connection.
const SPLIT_AT: u64 = 6 * 1024 * 1024;
/// How long a dropped download keeps trying to reconnect before giving up.
const RETRY_FOR: Duration = Duration::from_secs(120);
/// Reading this far ahead of the download moves the download there instead of waiting.
const JUMP_DISTANCE: u64 = 512 * 1024;

/// Reopens the song from a byte offset (an HTTP range request).
pub type Reopen = Arc<dyn Fn(u64) -> io::Result<Box<dyn Read + Send>> + Send + Sync>;

/// Anything the decoder can read audio from.
pub trait Input: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> Input for T {}

/// Downloaded byte ranges, sorted and merged.
#[derive(Default)]
struct Ranges(Vec<(u64, u64)>);

impl Ranges {
    fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        self.0.push((start, end));
        self.0.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.0.len());
        for &(s, e) in &self.0 {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.0 = merged;
    }

    /// The end of the downloaded range containing `position`.
    fn end_from(&self, position: u64) -> Option<u64> {
        self.0.iter().find(|(s, e)| *s <= position && position < *e).map(|(_, e)| *e)
    }

    /// The first missing byte at or after `from`.
    fn gap_after(&self, from: u64, length: u64) -> Option<u64> {
        let mut position = from;
        for &(s, e) in &self.0 {
            if e <= position {
                continue;
            }
            if s > position {
                return Some(position);
            }
            position = e;
        }
        (position < length).then_some(position)
    }

    fn complete(&self, length: u64) -> bool {
        length == 0 || self.0.first() == Some(&(0, length))
    }
}

#[derive(Default)]
struct State {
    ranges: Ranges,
    finished: bool,
    error: Option<io::Error>,
    /// Where the main connection is downloading now.
    cursor: u64,
    /// A position the player is waiting on, far from the download.
    jump: Option<u64>,
    /// Total size, learned at the end when the server didn't say.
    ended_at: Option<u64>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    writer: Mutex<File>,
    length: Option<u64>,
    cancelled: Arc<AtomicBool>,
    abandoned: AtomicBool,
    reopen: Option<Reopen>,
}

impl Shared {
    fn stopped(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed) || self.abandoned.load(Ordering::Relaxed)
    }

    fn write(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        {
            let mut file = self.writer.lock().unwrap();
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(bytes)?;
        }
        self.state.lock().unwrap().ranges.insert(offset, offset + bytes.len() as u64);
        self.changed.notify_all();
        Ok(())
    }

    fn covered(&self, position: u64) -> bool {
        self.state.lock().unwrap().ranges.end_from(position).is_some()
    }

    /// The next missing byte, after `from` or else from the start.
    fn next_gap(&self, from: u64, length: u64) -> Option<u64> {
        let state = self.state.lock().unwrap();
        state.ranges.gap_after(from, length).or_else(|| state.ranges.gap_after(0, length))
    }

    /// Opens the song at `offset`, retrying with growing pauses (finding a new
    /// server address in between, via Plex) until it works or time runs out.
    fn connect(&self, offset: u64, mut last_error: io::Error) -> io::Result<Box<dyn Read + Send>> {
        let Some(reopen) = &self.reopen else { return Err(last_error) };
        let started = Instant::now();
        let mut pause = Duration::from_millis(300);
        while started.elapsed() < RETRY_FOR {
            if self.stopped() {
                return Err(io::Error::other("Download stopped"));
            }
            match reopen(offset) {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = error,
            }
            thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_secs(4));
        }
        Err(last_error)
    }
}

#[derive(Clone)]
pub struct DownloadStatus(Arc<Shared>);

impl DownloadStatus {
    pub fn error(&self) -> Option<String> {
        self.0.state.lock().unwrap().error.as_ref().map(ToString::to_string)
    }
}

pub struct BufferedDownload {
    reader: File,
    position: u64,
    shared: Arc<Shared>,
}

impl BufferedDownload {
    /// A plain download (no reconnecting), as used by tests.
    pub fn new(
        response: impl Read + Send + 'static,
        length: Option<u64>,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        Self::with_options(response, length, cancelled, None, None)
    }

    /// A download that can reconnect and jump (`reopen`), and is copied to `cache`
    /// once complete.
    pub fn with_options(
        response: impl Read + Send + 'static,
        length: Option<u64>,
        cancelled: Arc<AtomicBool>,
        reopen: Option<Reopen>,
        cache: Option<PathBuf>,
    ) -> io::Result<Self> {
        if length.is_some_and(|length| length > LIMIT) {
            return Err(io::Error::other("Track exceeds the 256 MB buffer limit"));
        }
        let temp = tempfile::NamedTempFile::new()?;
        if let Some(length) = length {
            temp.as_file().set_len(length)?;
        }
        let reader = temp.reopen()?;
        let writer = temp.reopen()?;
        // Without a known size there's nothing to jump to or split.
        let reopen = reopen.filter(|_| length.is_some());
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            writer: Mutex::new(writer),
            length,
            cancelled,
            abandoned: AtomicBool::new(false),
            reopen,
        });
        if let (Some(length), Some(_)) = (length, &shared.reopen)
            && length >= SPLIT_AT
        {
            let helper = shared.clone();
            thread::spawn(move || second_half(&helper, length / 2, length));
        }
        let worker = shared.clone();
        let response: Box<dyn Read + Send> = Box::new(response);
        thread::spawn(move || main_download(&worker, response, temp, cache));
        Ok(Self { reader, position: 0, shared })
    }

    pub fn status(&self) -> DownloadStatus {
        DownloadStatus(self.shared.clone())
    }

    /// Waits until the byte at `position` is downloaded; returns where that
    /// downloaded stretch ends (or `None` at the end of the song).
    fn available(&self, position: u64) -> io::Result<Option<u64>> {
        let shared = &self.shared;
        let mut state = shared.state.lock().unwrap();
        loop {
            if shared.cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::other("Track selection changed"));
            }
            if let Some(end) = state.ranges.end_from(position) {
                return Ok(Some(end));
            }
            if shared.length.is_some_and(|length| position >= length)
                || state.ended_at.is_some_and(|end| position >= end)
            {
                return Ok(None);
            }
            if state.finished {
                return match &state.error {
                    Some(error) => Err(io::Error::new(error.kind(), error.to_string())),
                    None => Ok(None),
                };
            }
            // Far from where the download is: move the download here.
            if shared.reopen.is_some()
                && state.jump.is_none()
                && (position < state.cursor || position > state.cursor + JUMP_DISTANCE)
            {
                state.jump = Some(position);
            }
            state = shared.changed.wait_timeout(state, Duration::from_millis(100)).unwrap().0;
        }
    }
}

/// The main connection: downloads from the start (or wherever playback jumps),
/// reconnects after errors, fills any gaps, then saves the song to the cache.
fn main_download(shared: &Shared, mut stream: Box<dyn Read + Send>, temp: tempfile::NamedTempFile, cache: Option<PathBuf>) {
    let result = (|| -> io::Result<()> {
        let mut offset = 0u64;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            if shared.stopped() {
                return Ok(());
            }
            let jump = shared.state.lock().unwrap().jump.take();
            if let Some(target) = jump
                && shared.reopen.is_some()
                && !shared.covered(target)
            {
                offset = target;
                stream = shared.connect(target, io::Error::other("Jump"))?;
            }
            match stream.read(&mut buffer) {
                Ok(0) => {
                    let Some(length) = shared.length else {
                        shared.state.lock().unwrap().ended_at = Some(offset);
                        return Ok(());
                    };
                    match shared.next_gap(offset, length) {
                        None => return Ok(()),
                        Some(gap) if shared.reopen.is_some() => {
                            offset = gap;
                            stream = shared.connect(gap, io::Error::other("Gap"))?;
                        }
                        Some(_) => {
                            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Incomplete Plex track"));
                        }
                    }
                }
                Ok(count) => {
                    let count = match shared.length {
                        Some(length) => count.min(length.saturating_sub(offset) as usize),
                        None => count,
                    };
                    if offset + count as u64 > LIMIT {
                        return Err(io::Error::other("Track exceeds the 256 MB buffer limit"));
                    }
                    shared.write(offset, &buffer[..count])?;
                    offset += count as u64;
                    shared.state.lock().unwrap().cursor = offset;
                    // Reached data the second connection already has: skip ahead.
                    if let Some(length) = shared.length
                        && (offset >= length || shared.covered(offset))
                    {
                        match shared.next_gap(offset, length) {
                            None => return Ok(()),
                            Some(gap) if shared.reopen.is_some() => {
                                offset = gap;
                                stream = shared.connect(gap, io::Error::other("Gap"))?;
                            }
                            Some(_) => {}
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // The connection dropped: carry on from the same byte.
                Err(error) => stream = shared.connect(offset, error)?,
            }
        }
    })();
    let complete = {
        let mut state = shared.state.lock().unwrap();
        let complete = result.is_ok()
            && !shared.stopped()
            && match shared.length {
                Some(length) => state.ranges.complete(length),
                None => true,
            };
        state.error = result.err();
        state.finished = true;
        complete
    };
    shared.changed.notify_all();
    if complete && let Some(target) = cache {
        crate::cache::store(temp.path(), &target);
    }
    drop(temp);
}

/// The second connection for big songs: fetches from the middle until it reaches
/// data that's already there. Any trouble just ends it; the main one fills in.
fn second_half(shared: &Shared, start: u64, length: u64) {
    let Some(reopen) = &shared.reopen else { return };
    let Ok(mut stream) = reopen(start) else { return };
    let mut offset = start;
    let mut buffer = vec![0u8; 64 * 1024];
    while !shared.stopped() && offset < length && !shared.covered(offset) {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                let count = count.min((length - offset) as usize);
                if shared.write(offset, &buffer[..count]).is_err() {
                    return;
                }
                offset += count as u64;
            }
        }
    }
}

impl Read for BufferedDownload {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let Some(end) = self.available(self.position)? else { return Ok(0) };
        let limit = buffer.len().min((end - self.position) as usize);
        self.reader.seek(SeekFrom::Start(self.position))?;
        let count = self.reader.read(&mut buffer[..limit])?;
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for BufferedDownload {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let length = || -> io::Result<u64> {
            if let Some(length) = self.shared.length {
                return Ok(length);
            }
            // Unknown size: wait for the end.
            let mut state = self.shared.state.lock().unwrap();
            while !state.finished {
                state = self.shared.changed.wait_timeout(state, Duration::from_millis(100)).unwrap().0;
            }
            Ok(state.ended_at.unwrap_or(0))
        };
        let position = match target {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::End(offset) => length()?.checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        };
        self.position = position.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid track seek"))?;
        Ok(self.position)
    }
}

impl Drop for BufferedDownload {
    fn drop(&mut self) {
        self.shared.abandoned.store(true, Ordering::Relaxed);
    }
}

/// Opens a song for playback: from the disk cache when it's there (instant, no
/// network), otherwise streamed from Plex with reconnecting, jumping and caching.
/// Local files are opened directly.
pub fn open_track(
    plex: Option<Plex>,
    track: &Entry,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<(Box<dyn Input>, Option<u64>, Option<DownloadStatus>)> {
    use anyhow::Context;
    if let Some(path) = &track.local_path {
        let file = File::open(path).context("Opening local file")?;
        let length = file.metadata()?.len();
        return Ok((Box::new(file), Some(length), None));
    }
    if let Some(path) = crate::cache::cached(&track.rating_key)
        && let Ok(file) = File::open(&path)
    {
        let length = file.metadata()?.len();
        return Ok((Box::new(file), Some(length), None));
    }
    let plex = plex.context("Plex is disconnected")?;
    let response = plex.track_audio(track)?;
    let length = response.content_length();
    let reopen: Reopen = {
        let (plex, track) = (plex.clone(), track.clone());
        Arc::new(move |offset| {
            plex.track_audio_from(&track, offset)
                .map(|response| Box::new(response) as Box<dyn Read + Send>)
                .map_err(io::Error::other)
        })
    };
    let input = BufferedDownload::with_options(
        response,
        length,
        cancel,
        Some(reopen),
        crate::cache::path(&track.rating_key),
    )?;
    let status = input.status();
    Ok((Box::new(input), length, Some(status)))
}

pub struct NextSource<S> {
    pub source: S,
    pub started: Arc<AtomicBool>,
    pub cancelled: Arc<AtomicBool>,
    pub previous: Option<DownloadStatus>,
}

impl<S: rodio::Source> Iterator for NextSource<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cancelled.load(Ordering::Relaxed) {
            return None;
        }
        if !self.started.load(Ordering::Relaxed)
            && self
                .previous
                .as_ref()
                .is_some_and(|download| download.error().is_some())
        {
            return None;
        }
        let sample = self.source.next()?;
        self.started.store(true, Ordering::Release);
        Some(sample)
    }
}

impl<S: rodio::Source> rodio::Source for NextSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> u16 {
        self.source.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
    fn try_seek(&mut self, position: Duration) -> Result<(), rodio::source::SeekError> {
        self.source.try_seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, sync::mpsc};

    #[test]
    fn wav_decodes_before_download_finishes() {
        const PCM_BYTES: u32 = 160_000;
        let mut first_chunk = b"RIFF".to_vec();
        first_chunk.extend((36 + PCM_BYTES).to_le_bytes());
        first_chunk.extend(b"WAVEfmt ");
        first_chunk.extend(16u32.to_le_bytes());
        first_chunk.extend(1u16.to_le_bytes());
        first_chunk.extend(1u16.to_le_bytes());
        first_chunk.extend(16_000u32.to_le_bytes());
        first_chunk.extend(32_000u32.to_le_bytes());
        first_chunk.extend(2u16.to_le_bytes());
        first_chunk.extend(16u16.to_le_bytes());
        first_chunk.extend(b"data");
        first_chunk.extend(PCM_BYTES.to_le_bytes());
        first_chunk.extend([0; 32_000]);

        let (allow_tail, tail) = mpsc::channel();
        let (played, sample) = mpsc::channel();
        let length = 44 + u64::from(PCM_BYTES);
        let cancel = Arc::new(AtomicBool::new(false));
        let reader = Cursor::new(first_chunk).chain(GatedTail {
            signal: tail,
            remaining: 128_000,
        });
        let input = BufferedDownload::new(reader, Some(length), cancel.clone()).unwrap();
        let decoder = thread::spawn(move || {
            let result = rodio::Decoder::builder()
                .with_data(io::BufReader::new(input))
                .with_byte_len(length)
                .with_seekable(true)
                .build()
                .ok()
                .and_then(|mut source| source.next());
            let _ = played.send(result);
        });
        let first_sample = sample.recv_timeout(Duration::from_secs(3));
        cancel.store(true, Ordering::Relaxed);
        let _ = allow_tail.send(());
        decoder.join().unwrap();
        assert_eq!(first_sample.unwrap(), Some(0.0));
    }

    #[test]
    fn a_dropped_connection_resumes_from_the_same_byte() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let length = data.len() as u64;
        // The first connection breaks after 50,000 bytes.
        let first = Cursor::new(data[..50_000].to_vec()).chain(Broken);
        let source = Arc::new(data.clone());
        let offsets = Arc::new(Mutex::new(Vec::new()));
        let (seen, bytes) = (offsets.clone(), source.clone());
        let reopen: Reopen = Arc::new(move |offset| {
            seen.lock().unwrap().push(offset);
            Ok(Box::new(Cursor::new(bytes[offset as usize..].to_vec())) as Box<dyn Read + Send>)
        });
        let mut input =
            BufferedDownload::with_options(first, Some(length), Arc::default(), Some(reopen), None).unwrap();
        let mut out = Vec::new();
        input.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
        assert_eq!(offsets.lock().unwrap().first(), Some(&50_000));
    }

    #[test]
    fn seeking_ahead_jumps_the_download() {
        let data: Vec<u8> = (0..400_000u32).map(|i| (i % 249) as u8).collect();
        let length = data.len() as u64;
        // The first connection is very slow; seeking near the end shouldn't wait for it.
        let first = Slow { inner: Cursor::new(data.clone()) };
        let bytes = Arc::new(data.clone());
        let reopen: Reopen = Arc::new(move |offset| {
            Ok(Box::new(Cursor::new(bytes[offset as usize..].to_vec())) as Box<dyn Read + Send>)
        });
        let mut input =
            BufferedDownload::with_options(first, Some(length), Arc::default(), Some(reopen), None).unwrap();
        let started = Instant::now();
        input.seek(SeekFrom::Start(390_000)).unwrap();
        let mut tail = vec![0; 10_000];
        input.read_exact(&mut tail).unwrap();
        assert_eq!(tail, data[390_000..]);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn queued_track_starts_without_an_empty_audio_frame() {
        use rodio::buffer::SamplesBuffer;
        let (sink, mut output) = rodio::Sink::new();
        let started = Arc::new(AtomicBool::new(false));
        sink.append(SamplesBuffer::new(1, 48_000, vec![0.25; 4]));
        sink.append(NextSource {
            source: SamplesBuffer::new(1, 48_000, vec![0.5; 4]),
            started: started.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
            previous: None,
        });
        assert_eq!(output.by_ref().take(4).collect::<Vec<_>>(), vec![0.25; 4]);
        assert!(!started.load(Ordering::Acquire));
        assert_eq!(output.by_ref().take(4).collect::<Vec<_>>(), vec![0.5; 4]);
        assert!(started.load(Ordering::Acquire));
    }

    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::ConnectionReset, "network changed"))
        }
    }

    struct Slow {
        inner: Cursor<Vec<u8>>,
    }

    impl Read for Slow {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            thread::sleep(Duration::from_millis(50));
            let limit = buffer.len().min(1024);
            self.inner.read(&mut buffer[..limit])
        }
    }

    struct GatedTail {
        signal: mpsc::Receiver<()>,
        remaining: usize,
    }

    impl Read for GatedTail {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 128_000 {
                self.signal.recv().map_err(io::Error::other)?;
            }
            let count = self.remaining.min(buffer.len());
            buffer[..count].fill(0);
            self.remaining -= count;
            Ok(count)
        }
    }
}
