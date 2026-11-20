//! Songs kept on disk: everything played or preloaded is saved here (oldest
//! removed first past the size limit), so replays, going back and reopening
//! albums start instantly and work without a connection.

use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, OnceLock},
    thread,
    time::SystemTime,
};

struct Cache {
    dir: PathBuf,
    limit: u64,
}

static CACHE: OnceLock<Cache> = OnceLock::new();
/// Songs being preloaded right now (by key).
static WARMING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
/// At most this many preloads at once, so they don't crowd out playback.
const MAX_WARMING: usize = 2;

/// Sets where songs are cached and how much space they may use.
pub fn init(dir: PathBuf, limit: u64) {
    let _ = fs::create_dir_all(&dir);
    if CACHE.set(Cache { dir, limit }).is_ok() {
        trim();
    }
}

/// Where a song is (or would be) cached.
pub fn path(key: &str) -> Option<PathBuf> {
    let name: String = key.chars().filter(char::is_ascii_alphanumeric).collect();
    if name.is_empty() {
        return None;
    }
    Some(CACHE.get()?.dir.join(format!("{name}.audio")))
}

/// The cached copy of a song, if there is one (marked as just used).
pub fn cached(key: &str) -> Option<PathBuf> {
    let path = path(key)?;
    if !path.is_file() {
        return None;
    }
    if let Ok(file) = fs::File::options().write(true).open(&path) {
        let _ = file.set_modified(SystemTime::now());
    }
    Some(path)
}

/// Copies a finished download into the cache.
pub fn store(source: &Path, target: &Path) {
    let part = target.with_extension("part");
    if fs::copy(source, &part).is_ok() && fs::rename(&part, target).is_ok() {
        trim();
    } else {
        let _ = fs::remove_file(&part);
    }
}

/// Downloads a song into the cache in the background (the next songs, the one
/// you're likely to play), unless it's already there or being fetched.
pub fn warm<R: Read + Send + 'static>(key: String, fetch: impl FnOnce() -> anyhow::Result<(R, Option<u64>)> + Send + 'static) {
    let Some(target) = path(&key) else { return };
    if target.is_file() {
        return;
    }
    {
        let Ok(mut warming) = WARMING.lock() else { return };
        if warming.len() >= MAX_WARMING || !warming.insert(key.clone()) {
            return;
        }
    }
    thread::spawn(move || {
        let part = target.with_extension("warm");
        let result = (|| -> anyhow::Result<()> {
            let (mut response, length) = fetch()?;
            let mut file = fs::File::create(&part)?;
            let copied = io::copy(&mut response, &mut file)?;
            file.flush()?;
            drop(file);
            if length.is_some_and(|length| length != copied) {
                anyhow::bail!("Incomplete download");
            }
            fs::rename(&part, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        if let Ok(mut warming) = WARMING.lock() {
            warming.remove(&key);
        }
        trim();
    });
}

/// Removes the least recently used songs until the cache fits its limit.
pub fn trim() {
    let Some(cache) = CACHE.get() else { return };
    let (dir, limit) = (cache.dir.clone(), cache.limit);
    thread::spawn(move || {
        let Ok(entries) = fs::read_dir(&dir) else { return };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "audio"))
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                Some((meta.modified().ok()?, meta.len(), entry.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
        files.sort_by_key(|(time, ..)| *time);
        for (_, size, path) in files {
            if total <= limit {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
            }
        }
    });
}

/// Writes a file so a crash mid-write never leaves it half written: write a
/// temporary copy, then swap it in.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path)
}
