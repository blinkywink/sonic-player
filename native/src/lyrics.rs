//! Lyrics for the Now Playing view: a sidecar `.lrc` file, embedded tags, or LRCLIB.

use crate::plex::Entry;
use lofty::{file::TaggedFileExt, tag::ItemKey};
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Start time in seconds; `None` for unsynced lyrics.
    pub time: Option<f32>,
    pub text: String,
}

/// Parses LRC (`[mm:ss.xx]text`, several stamps per line allowed). Text without
/// any timestamps is returned as unsynced lines.
pub fn parse(source: &str) -> Vec<Line> {
    let mut synced = Vec::new();
    let mut plain = Vec::new();
    for raw in source.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(after) = rest.strip_prefix('[') {
            let Some(end) = after.find(']') else { break };
            let tag = &after[..end];
            rest = after[end + 1..].trim_start();
            if let Some(seconds) = stamp(tag) {
                stamps.push(seconds);
            } else if stamps.is_empty() && tag.contains(':') {
                // Metadata such as [ar:Artist]; skip the whole line.
                rest = "";
            }
        }
        let text = strip_word_stamps(rest);
        if stamps.is_empty() {
            if !raw.trim_start().starts_with('[') {
                plain.push(Line { time: None, text });
            }
        } else {
            synced.extend(stamps.into_iter().map(|time| Line { time: Some(time), text: text.clone() }));
        }
    }
    if synced.is_empty() {
        // Trim blank lines at both ends of plain lyrics.
        while plain.first().is_some_and(|l| l.text.is_empty()) {
            plain.remove(0);
        }
        while plain.last().is_some_and(|l| l.text.is_empty()) {
            plain.pop();
        }
        return plain;
    }
    synced.sort_by(|a, b| a.time.unwrap_or(0.0).total_cmp(&b.time.unwrap_or(0.0)));
    synced
}

/// Removes enhanced-LRC word timings such as `<00:19.53>` and tidies spacing.
fn strip_word_stamps(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else { break };
        if stamp(&rest[start + 1..start + end]).is_none() {
            out.push_str(&rest[..start + end + 1]);
        } else {
            out.push_str(&rest[..start]);
            out.push(' ');
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn stamp(tag: &str) -> Option<f32> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes: f32 = minutes.trim().parse().ok()?;
    let seconds: f32 = seconds.trim().replace(',', ".").parse().ok()?;
    (minutes >= 0.0 && (0.0..60.0).contains(&seconds)).then_some(minutes * 60.0 + seconds)
}

/// Index of the line being sung at `position` seconds, if the lyrics are synced.
pub fn current(lines: &[Line], position: f32) -> i32 {
    let mut index = -1;
    for (row, line) in lines.iter().enumerate() {
        match line.time {
            Some(time) if time <= position + 0.15 => index = row as i32,
            Some(_) => break,
            None => return -1,
        }
    }
    index
}

/// Looks up lyrics for a track. Local sources are tried first; `online` allows LRCLIB.
pub fn find(track: &Entry, online: bool) -> Option<Vec<Line>> {
    if let Some(path) = &track.local_path {
        let sidecar = path.with_extension("lrc");
        if let Ok(text) = std::fs::read_to_string(&sidecar) {
            let lines = parse(&text);
            if !lines.is_empty() {
                return Some(lines);
            }
        }
        if let Some(text) = lofty::read_from_path(path).ok().and_then(|file| {
            file.tags()
                .iter()
                .find_map(|tag| tag.get_string(ItemKey::Lyrics).map(str::to_owned))
        }) {
            let lines = parse(&text);
            if !lines.is_empty() {
                return Some(lines);
            }
        }
    }
    if online { lrclib(track) } else { None }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    instrumental: bool,
}

impl Record {
    fn lines(&self) -> Option<Vec<Line>> {
        if self.instrumental {
            return None;
        }
        [&self.synced_lyrics, &self.plain_lyrics]
            .into_iter()
            .flatten()
            .map(|text| parse(text))
            .find(|lines| !lines.is_empty())
    }
}

fn lrclib(track: &Entry) -> Option<Vec<Line>> {
    if track.title.trim().is_empty() || track.grandparent_title.trim().is_empty() {
        return None;
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("SonicPlayer/0.1")
        .build()
        .ok()?;
    let seconds = (track.duration / 1000).to_string();
    let exact = client
        .get("https://lrclib.net/api/get")
        .query(&[
            ("artist_name", track.grandparent_title.as_str()),
            ("track_name", track.title.as_str()),
            ("album_name", track.parent_title.as_str()),
            ("duration", seconds.as_str()),
        ])
        .send()
        .ok()
        .filter(|response| response.status().is_success())
        .and_then(|response| response.json::<Record>().ok())
        .and_then(|record| record.lines());
    if exact.is_some() {
        return exact;
    }
    // No exact match: search and prefer a result with timestamps.
    let results = client
        .get("https://lrclib.net/api/search")
        .query(&[
            ("artist_name", track.grandparent_title.as_str()),
            ("track_name", track.title.as_str()),
        ])
        .send()
        .ok()
        .filter(|response| response.status().is_success())
        .and_then(|response| response.json::<Vec<Record>>().ok())?;
    results
        .iter()
        .find(|r| r.synced_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty()))
        .or_else(|| results.first())
        .and_then(Record::lines)
}

/// Lays out lines for the lyrics panel by estimating word wrap at `chars` per row.
/// Returns (y, height) in logical pixels for each line.
pub fn layout(lines: &[Line], chars: usize) -> Vec<(f32, f32)> {
    // Matches the 34px bold lyric text in the Now Playing view.
    const ROW: f32 = 44.0;
    const GAP: f32 = 18.0;
    let chars = chars.max(8);
    let mut y = 0.0;
    lines
        .iter()
        .map(|line| {
            let rows = if line.text.is_empty() { 0 } else { wrapped_rows(&line.text, chars) };
            // Blank lines in lyrics are pauses between verses: keep a short gap.
            let height = if rows == 0 { ROW * 0.6 } else { rows as f32 * ROW };
            let slot = (y, height);
            y += height + GAP;
            slot
        })
        .collect()
}

fn wrapped_rows(text: &str, chars: usize) -> usize {
    let mut rows = 1;
    let mut used = 0;
    for word in text.split_whitespace() {
        let length = word.chars().count();
        if used > 0 && used + 1 + length > chars {
            rows += 1;
            used = length.min(chars);
            rows += length.saturating_sub(1) / chars;
        } else {
            used += if used > 0 { 1 } else { 0 } + length;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synced_lrc_with_metadata_and_repeated_stamps() {
        let lines = parse("[ar:Someone]\n[00:01.50]Hello\n[00:10.00][00:20.25]Chorus\n[00:05]Middle");
        let times: Vec<_> = lines.iter().map(|l| l.time.unwrap()).collect();
        assert_eq!(times, [1.5, 5.0, 10.0, 20.25]);
        assert_eq!(lines[1].text, "Middle");
        assert_eq!(current(&lines, 0.5), -1);
        assert_eq!(current(&lines, 12.0), 2);
        assert_eq!(current(&lines, 99.0), 3);
    }

    #[test]
    fn strips_enhanced_word_timings() {
        let lines = parse("[00:18.89]<00:18.89>We're <00:19.25>no <00:19.53>strangers <00:20.07>to <00:20.33>love");
        assert_eq!(lines[0].text, "We're no strangers to love");
        assert_eq!(lines[0].time, Some(18.89));
        assert_eq!(parse("[00:01]a <b> c")[0].text, "a <b> c");
    }

    #[test]
    fn plain_text_is_unsynced() {
        let lines = parse("\nFirst line\n\nSecond line\n");
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.time.is_none()));
        assert_eq!(current(&lines, 3.0), -1);
    }

    #[test]
    fn layout_wraps_long_lines() {
        let lines = parse("[00:01]short\n[00:02]a much longer line that needs to wrap here");
        let slots = layout(&lines, 20);
        assert_eq!(slots[0], (0.0, 44.0));
        assert_eq!(slots[1].1, 132.0);
        assert_eq!(slots[1].0, 62.0);
    }
}
