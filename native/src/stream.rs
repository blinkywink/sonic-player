use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Default)]
struct Progress {
    bytes: u64,
    finished: bool,
    error: Option<io::Error>,
}

#[derive(Clone)]
pub struct DownloadStatus(Arc<(Mutex<Progress>, Condvar)>);

impl DownloadStatus {
    pub fn error(&self) -> Option<String> {
        self.0
            .0
            .lock()
            .unwrap()
            .error
            .as_ref()
            .map(ToString::to_string)
    }
}

pub struct BufferedDownload {
    file: File,
    position: u64,
    length: Option<u64>,
    progress: Arc<(Mutex<Progress>, Condvar)>,
    cancelled: Arc<AtomicBool>,
    abandoned: Arc<AtomicBool>,
}

impl BufferedDownload {
    pub fn new(
        mut response: impl Read + Send + 'static,
        length: Option<u64>,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        if length.is_some_and(|length| length > LIMIT) {
            return Err(io::Error::other("Track exceeds the 256 MB buffer limit"));
        }
        let file = tempfile::NamedTempFile::new()?;
        let reader = file.reopen()?;
        let progress = Arc::new((Mutex::new(Progress::default()), Condvar::new()));
        let worker_progress = progress.clone();
        let worker_cancelled = cancelled.clone();
        let abandoned = Arc::new(AtomicBool::new(false));
        let worker_abandoned = abandoned.clone();
        thread::spawn(move || {
            let mut file = file;
            let (state, changed) = &*worker_progress;
            let result = (|| -> io::Result<()> {
                let mut buffer = [0; 64 * 1024];
                let mut bytes = 0;
                while !worker_cancelled.load(Ordering::Relaxed)
                    && !worker_abandoned.load(Ordering::Relaxed)
                {
                    let count = response.read(&mut buffer)?;
                    if count == 0 {
                        if length.is_some_and(|length| length != bytes) {
                            return Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "Incomplete Plex track",
                            ));
                        }
                        return Ok(());
                    }
                    bytes += count as u64;
                    if bytes > LIMIT {
                        return Err(io::Error::other("Track exceeds the 256 MB buffer limit"));
                    }
                    file.write_all(&buffer[..count])?;
                    state.lock().unwrap().bytes = bytes;
                    changed.notify_all();
                }
                Ok(())
            })();
            let mut state = state.lock().unwrap();
            state.error = result.err();
            state.finished = true;
            changed.notify_all();
        });
        Ok(Self {
            file: reader,
            position: 0,
            length,
            progress,
            cancelled,
            abandoned,
        })
    }

    pub fn status(&self) -> DownloadStatus {
        DownloadStatus(self.progress.clone())
    }

    fn available(&self, offset: Option<u64>) -> io::Result<u64> {
        let (state, changed) = &*self.progress;
        let mut state = state.lock().unwrap();
        loop {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::other("Track selection changed"));
            }
            if offset.is_some_and(|offset| offset < state.bytes) {
                return Ok(state.bytes);
            }
            if state.finished {
                if let Some(error) = &state.error {
                    return Err(io::Error::new(error.kind(), error.to_string()));
                }
                return Ok(state.bytes);
            }
            state = changed
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap()
                .0;
        }
    }
}

impl Read for BufferedDownload {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.length.is_some_and(|length| self.position >= length) {
            return Ok(0);
        }
        let available = self
            .available(Some(self.position))?
            .saturating_sub(self.position);
        let limit = buffer.len().min(available as usize);
        let count = self.file.read(&mut buffer[..limit])?;
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for BufferedDownload {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let target = match target {
            SeekFrom::End(offset) => {
                let length = match self.length {
                    Some(length) => length,
                    None => self.available(None)?,
                };
                SeekFrom::Start(length.checked_add_signed(offset).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "Invalid track seek")
                })?)
            }
            target => target,
        };
        self.position = self.file.seek(target)?;
        Ok(self.position)
    }
}

impl Drop for BufferedDownload {
    fn drop(&mut self) {
        self.abandoned.store(true, Ordering::Relaxed);
    }
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
