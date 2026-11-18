//! Self-update from GitHub Releases (Windows). In the background, Sonic checks the
//! latest release; if it is newer, the new sonicplayer.exe is downloaded next to the
//! running one and swapped in, so the next launch runs the new version. Nothing is
//! shown and nothing is interrupted.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{io::Read, path::Path, thread, time::Duration};

const LATEST: &str = "https://api.github.com/repos/blinkywink/sonic-player/releases/latest";
const ASSET: &str = "sonicplayer-windows-x64.exe";
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// "v0.3.1" / "0.3.1" -> (0, 3, 1).
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(0)))
}

/// Starts the background updater (installed copies only, not development builds).
pub fn start() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent().map(Path::to_path_buf) else { return };
    // Clean up the copy replaced by the last update.
    let _ = std::fs::remove_file(dir.join("sonicplayer.old.exe"));
    let in_dev_tree = exe.components().any(|part| part.as_os_str() == "target");
    if !cfg!(windows) || in_dev_tree || std::env::var_os("SONIC_NO_UPDATE").is_some() {
        return;
    }
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(20));
        loop {
            match check_and_install(&exe) {
                Ok(true) => return, // Installed; it takes effect on the next launch.
                Ok(false) => {}
                Err(error) => eprintln!("Sonic update check failed: {error:#}"),
            }
            thread::sleep(CHECK_EVERY);
        }
    });
}

/// Returns true once a newer version has been put in place.
fn check_and_install(exe: &Path) -> Result<bool> {
    let current = parse_version(env!("CARGO_PKG_VERSION")).context("Bad app version")?;
    let http = reqwest::blocking::Client::builder()
        .user_agent(concat!("SonicPlayer/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?;
    let release: Release = http
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .json()?;
    let Some(latest) = parse_version(&release.tag_name) else { return Ok(false) };
    if release.draft || release.prerelease || latest <= current {
        return Ok(false);
    }
    let Some(asset) = release.assets.iter().find(|asset| asset.name == ASSET) else {
        return Ok(false);
    };
    if !asset.browser_download_url.starts_with("https://github.com/blinkywink/sonic-player/releases/download/") {
        bail!("Unexpected download location");
    }

    let mut bytes = Vec::with_capacity(asset.size as usize);
    http.get(&asset.browser_download_url)
        .timeout(Duration::from_secs(600))
        .send()?
        .error_for_status()?
        .take(200 * 1024 * 1024)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != asset.size || !bytes.starts_with(b"MZ") {
        bail!("Downloaded update is incomplete");
    }

    // Windows lets a running exe be renamed but not overwritten: move it aside,
    // then put the new one in its place.
    let dir = exe.parent().context("No install folder")?;
    let staged = dir.join("sonicplayer.new.exe");
    let old = dir.join("sonicplayer.old.exe");
    std::fs::write(&staged, &bytes).context("Saving update")?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).context("Moving the running copy aside")?;
    if let Err(error) = std::fs::rename(&staged, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(error).context("Installing update");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v0.10.2"), Some((0, 10, 2)));
        assert!(parse_version("v0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("nightly"), None);
    }
}
