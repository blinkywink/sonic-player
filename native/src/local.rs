use crate::plex::Entry;
use anyhow::{Context, Result};
use lofty::{
    file::{AudioFile, TaggedFileExt},
    tag::{Accessor, ItemKey},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const EXTENSIONS: [&str; 9] = [
    "mp3", "m4a", "flac", "ogg", "opus", "wav", "aac", "aiff", "aif",
];

pub fn folder() -> Result<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("Could not locate the user folder")?;
    Ok(PathBuf::from(home).join("Music").join("SonicMusic"))
}

pub fn scan() -> Result<Vec<(Entry, Vec<Entry>)>> {
    let root = folder()?;
    fs::create_dir_all(&root)?;
    scan_folder(root)
}

fn scan_folder(root: PathBuf) -> Result<Vec<(Entry, Vec<Entry>)>> {
    let mut groups: BTreeMap<(String, String), Vec<Entry>> = BTreeMap::new();
    let mut folders = vec![root];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder)?.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file()
                && path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|ext| EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
            {
                if let Some((album_artist, track)) = track(&path) {
                    groups
                        .entry((track.parent_title.clone(), album_artist))
                        .or_default()
                        .push(track);
                }
            }
        }
    }
    Ok(groups
        .into_iter()
        .map(|((title, artist), mut tracks)| {
            tracks.sort_by(|a, b| {
                (a.disc, a.index, a.title.as_str()).cmp(&(b.disc, b.index, b.title.as_str()))
            });
            let cover = tracks
                .iter()
                .find(|track| !track.thumb.is_empty())
                .and_then(|track| track.local_path.clone());
            let album = Entry {
                rating_key: format!("local-album:{title:?}:{artist:?}"),
                title,
                parent_title: artist,
                year: tracks
                    .iter()
                    .map(|track| track.year)
                    .max()
                    .unwrap_or_default(),
                local_path: cover,
                ..Entry::default()
            };
            for track in &mut tracks {
                track.parent_rating_key.clone_from(&album.rating_key);
            }
            (album, tracks)
        })
        .collect())
}

fn track(path: &Path) -> Option<(String, Entry)> {
    let mut track = Entry::local(path.to_owned())?;
    let tagged = lofty::read_from_path(path).ok();
    let tag = tagged
        .as_ref()
        .and_then(|f| f.primary_tag().or_else(|| f.first_tag()));
    if let Some(file) = &tagged {
        track.duration = file.properties().duration().as_millis() as u64;
        track.bitrate = file
            .properties()
            .audio_bitrate()
            .or(file.properties().overall_bitrate())
            .unwrap_or_default();
    }
    if let Some(tag) = tag {
        if let Some(title) = tag.title() {
            track.title = title.into_owned();
        }
        track.parent_title = tag.album().map(|s| s.into_owned()).unwrap_or_default();
        track.grandparent_title = tag.artist().map(|s| s.into_owned()).unwrap_or_default();
        track.index = tag.track().unwrap_or_default();
        track.disc = tag.disk().unwrap_or_default();
        track.year = tag
            .date()
            .map(|date| u32::from(date.year))
            .unwrap_or_default();
        if tag
            .pictures()
            .iter()
            .any(|picture| !picture.data().is_empty())
        {
            track.thumb = "embedded".into();
        }
    }
    if track.parent_title.is_empty() {
        track.parent_title = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .unwrap_or("Local files")
            .into();
    }
    let album_artist = tag
        .and_then(|tag| tag.get_string(ItemKey::AlbumArtist))
        .unwrap_or(&track.grandparent_title)
        .to_owned();
    Some((album_artist, track))
}

pub fn art(path: &Path) -> Option<Vec<u8>> {
    let file = lofty::read_from_path(path).ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag())?;
    let image = tag
        .pictures()
        .iter()
        .find(|p| !p.data().is_empty() && p.data().len() <= 8 * 1024 * 1024)?;
    Some(image.data().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_nested_music_and_groups_untagged_tracks_by_folder() {
        let root = tempfile::tempdir().unwrap();
        let album = root.path().join("Fixture album");
        fs::create_dir(&album).unwrap();
        // One second of mono silence, 16-bit PCM at 8 kHz.
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&16036u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.resize(wav.len() + 16000, 0);
        fs::write(album.join("Disc one.wav"), &wav).unwrap();
        fs::write(album.join("readme.txt"), "not audio").unwrap();

        let albums = scan_folder(root.path().to_owned()).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].0.title, "Fixture album");
        assert_eq!(albums[0].1.len(), 1);
        assert_eq!(albums[0].1[0].title, "Disc one");
        assert_eq!(albums[0].1[0].duration, 1000);
    }
}
