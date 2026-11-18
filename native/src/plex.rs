use anyhow::{Context, Result, bail};
use reqwest::{
    Url,
    blocking::{Client, RequestBuilder},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex, RwLock, mpsc},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Plex {
    pub http: Client,
    account: Account,
    /// The address in use; shared by every clone so a reconnect helps them all.
    endpoint: Arc<RwLock<Endpoint>>,
    /// Held while looking for a working address, so only one search runs at a time.
    reconnecting: Arc<Mutex<Option<Instant>>>,
    token: String,
    pub library: String,
    /// Every music library on the server as (key, title), in server order.
    pub sections: Vec<(String, String)>,
    pub server_id: String,
}

#[derive(Clone)]
struct Endpoint {
    url: Url,
    local: bool,
}

/// A music library server found at one of its addresses.
struct Found {
    endpoint: Endpoint,
    token: String,
    server_id: String,
    music: Vec<Section>,
}

#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub rating_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<std::path::PathBuf>,
    pub title: String,
    #[serde(default)]
    pub parent_title: String,
    #[serde(default)]
    pub parent_rating_key: String,
    #[serde(default)]
    pub grandparent_title: String,
    #[serde(default)]
    pub thumb: String,
    #[serde(default)]
    pub parent_thumb: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub year: u32,
    #[serde(default)]
    pub index: u32,
    #[serde(default, rename = "parentIndex")]
    pub disc: u32,
    #[serde(default, rename = "Media")]
    pub(crate) media: Vec<Media>,
}

impl Entry {
    pub fn local(local_path: std::path::PathBuf) -> Option<Self> {
        Some(Self {
            rating_key: format!("local:{}", local_path.display()),
            title: local_path.file_stem()?.to_string_lossy().into_owned(),
            local_path: Some(local_path),
            ..Self::default()
        })
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub(crate) struct Media {
    #[serde(default, rename = "Part")]
    parts: Vec<Part>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
struct Part {
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pin {
    id: u64,
    code: String,
    auth_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    client_identifier: String,
    provides: String,
    access_token: Option<String>,
    /// The server's public IP as plex.tv last saw it.
    #[serde(default)]
    public_address: String,
    #[serde(default)]
    connections: Vec<Connection>,
}

/// An address typed in Settings (e.g. "203.0.113.7:25565"), tried right after the
/// home network.
static MANUAL_ADDRESS: RwLock<Option<Url>> = RwLock::new(None);

/// Sets (or clears, with "") the address typed in Settings. Accepts "host:port",
/// or a full http(s):// URL.
pub fn set_manual_address(text: &str) -> Result<()> {
    let text = text.trim().trim_end_matches('/');
    let url = if text.is_empty() {
        None
    } else {
        let full = if text.contains("://") { text.to_owned() } else { format!("http://{text}") };
        let url = Url::parse(&full).context("Enter an address like 203.0.113.7:25565")?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            bail!("Enter an address like 203.0.113.7:25565");
        }
        Some(url)
    };
    if let Ok(mut manual) = MANUAL_ADDRESS.write() {
        *manual = url;
    }
    Ok(())
}

/// Short reason a connection attempt failed, for the "couldn't reach" message.
fn describe_failure(error: &anyhow::Error) -> String {
    match error.downcast_ref::<reqwest::Error>() {
        Some(e) if e.is_timeout() => "timed out".into(),
        Some(e) if e.is_connect() => "couldn't connect".into(),
        Some(e) if e.status().is_some() => format!("HTTP {}", e.status().map(|s| s.as_u16()).unwrap_or(0)),
        Some(e) if e.is_decode() => "not a Plex server".into(),
        Some(_) => "failed".into(),
        None => error.to_string(),
    }
}

#[derive(Deserialize)]
struct Connection {
    uri: Url,
    local: bool,
    relay: bool,
}

#[derive(Deserialize)]
struct Envelope<T> {
    #[serde(rename = "MediaContainer")]
    container: T,
}

#[derive(Deserialize)]
struct Sections {
    #[serde(default, rename = "Directory")]
    directories: Vec<Section>,
}

#[derive(Deserialize)]
struct Section {
    key: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Items {
    #[serde(default, rename = "Metadata")]
    entries: Vec<Entry>,
}

fn headers(request: RequestBuilder, id: &str) -> RequestBuilder {
    request
        .header("Accept", "application/json")
        .header("X-Plex-Product", "Sonic")
        .header("X-Plex-Client-Identifier", id)
}

#[derive(Clone, Deserialize, Serialize)]
struct Account {
    token: String,
    client_id: String,
}

const KEYRING_ACCOUNT: &str = "Plex";

fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

impl Plex {
    pub fn restore() -> Result<Option<Self>> {
        let Some(account) = crate::credentials::read::<Account>(KEYRING_ACCOUNT)? else {
            return Ok(None);
        };
        Ok(Some(Self::discover(http_client()?, account, None)?))
    }

    pub fn forget() -> Result<()> {
        crate::credentials::remove(KEYRING_ACCOUNT)
    }

    pub fn sign_in() -> Result<Self> {
        let http = http_client()?;
        let client_id = uuid::Uuid::new_v4().to_string();
        let pin: Pin = headers(
            http.post("https://plex.tv/api/v2/pins?strong=true"),
            &client_id,
        )
        .send()?
        .error_for_status()?
        .json()?;
        let mut auth = Url::parse("https://app.plex.tv/auth/")?;
        let query = url_query(&client_id, &pin.code);
        auth.set_fragment(Some(&format!("!?{query}")));
        // On iPhone the `open` crate needs a jailbreak tool; use UIKit instead.
        #[cfg(target_os = "ios")]
        crate::ios::open_url(auth.as_str());
        #[cfg(not(target_os = "ios"))]
        open::that_detached(auth.as_str()).context("Opening Plex sign-in")?;
        let deadline = Instant::now() + Duration::from_secs(180);
        let account_token = loop {
            if Instant::now() >= deadline {
                bail!("Plex sign-in timed out");
            }
            thread::sleep(Duration::from_secs(2));
            let status: Pin = headers(
                http.get(format!("https://plex.tv/api/v2/pins/{}", pin.id)),
                &client_id,
            )
            .send()?
            .error_for_status()?
            .json()?;
            if let Some(token) = status.auth_token {
                break token;
            }
        };
        let account = Account {
            token: account_token,
            client_id,
        };
        let _ = crate::credentials::save(KEYRING_ACCOUNT, &account);
        Self::discover(http, account, None)
    }

    /// The server address in use, and whether it is on the home network.
    pub fn server(&self) -> Url {
        self.endpoint.read().map(|e| e.url.clone()).unwrap_or_else(|e| e.into_inner().url.clone())
    }

    pub fn is_local(&self) -> bool {
        self.endpoint.read().map(|e| e.local).unwrap_or(false)
    }

    /// Looks for a working address for the same server again (after a network
    /// change, such as leaving home). Returns whether one was found.
    pub fn reconnect(&self) -> bool {
        let Ok(mut last) = self.reconnecting.lock() else { return false };
        // Someone else just reconnected; use what they found.
        if last.is_some_and(|at| at.elapsed() < Duration::from_secs(5)) {
            return true;
        }
        let found = Self::find_server(&self.http, &self.account, Some(&self.server_id));
        *last = Some(Instant::now());
        match found {
            Ok(found) => {
                if let Ok(mut endpoint) = self.endpoint.write() {
                    *endpoint = found.endpoint;
                }
                true
            }
            Err(_) => false,
        }
    }

    /// Finds the best working address for this server now (Settings' refresh button).
    pub fn refresh_connection(&self) -> Result<Self> {
        let found = Self::find_server(&self.http, &self.account, Some(&self.server_id))?;
        if let Ok(mut endpoint) = self.endpoint.write() {
            *endpoint = found.endpoint;
        }
        if let Ok(mut last) = self.reconnecting.lock() {
            *last = Some(Instant::now());
        }
        Ok(self.clone())
    }

    fn discover(http: Client, account: Account, current: Option<(&str, &str)>) -> Result<Self> {
        let found = Self::find_server(&http, &account, current.map(|(server, _)| server))?;
        let section = found
            .music
            .iter()
            .find(|s| current.is_none_or(|(_, library)| s.key == library))
            .or(found.music.first())
            .context("No Plex music library was found")?;
        Ok(Self {
            http,
            account,
            endpoint: Arc::new(RwLock::new(found.endpoint)),
            reconnecting: Arc::new(Mutex::new(None)),
            token: found.token,
            library: section.key.clone(),
            sections: found.music.into_iter().map(|s| (s.key, s.title)).collect(),
            server_id: found.server_id,
        })
    }

    /// Tries every address of every music server at once and returns the best one
    /// that answers: home network, an address typed in Settings, direct remote
    /// (HTTPS, then HTTP), the server's public IP on its known ports, then Plex's
    /// relay as a last resort.
    fn find_server(http: &Client, account: &Account, only: Option<&str>) -> Result<Found> {
        let devices: Vec<Device> = headers(
            http.get("https://plex.tv/api/v2/resources?includeHttps=1&includeRelay=1"),
            &account.client_id,
        )
        .header("X-Plex-Token", &account.token)
        .send()
        .context("Couldn't reach plex.tv. Check your internet connection")?
        .error_for_status()?
        .json()?;
        let manual = MANUAL_ADDRESS.read().ok().and_then(|manual| manual.clone());
        // (rank, url, local, token, server id); lower rank is better.
        let mut candidates: Vec<(u8, Url, bool, String, String)> = Vec::new();
        for device in devices
            .into_iter()
            .filter(|device| device.provides.split(',').any(|p| p == "server"))
            .filter(|device| only.is_none_or(|id| device.client_identifier == id))
        {
            let token = device.access_token.clone().unwrap_or_else(|| account.token.clone());
            let id = device.client_identifier.clone();
            let mut ports = vec![32400];
            for connection in &device.connections {
                if !matches!(connection.uri.scheme(), "http" | "https") {
                    continue;
                }
                let https = connection.uri.scheme() == "https";
                let rank = match (connection.local, connection.relay, https) {
                    (true, _, true) => 0,
                    (true, _, false) => 1,
                    (false, true, _) => 6,
                    (false, false, true) => 3,
                    (false, false, false) => 4,
                };
                if !connection.local
                    && !connection.relay
                    && let Some(port) = connection.uri.port_or_known_default()
                    && !ports.contains(&port)
                {
                    ports.push(port);
                }
                candidates.push((rank, connection.uri.clone(), connection.local, token.clone(), id.clone()));
            }
            if let Some(url) = &manual {
                candidates.push((2, url.clone(), false, token.clone(), id.clone()));
            }
            // plex.tv's remote addresses can carry the wrong port (for example when
            // the router forwards a custom one); try the public IP directly too.
            if !device.public_address.is_empty() {
                for port in ports {
                    if let Ok(url) = Url::parse(&format!("http://{}:{port}", device.public_address))
                        && !candidates.iter().any(|(_, known, ..)| *known == url)
                    {
                        candidates.push((5, url, false, token.clone(), id.clone()));
                    }
                }
            }
        }
        candidates.sort_by_key(|(rank, ..)| *rank);
        if candidates.is_empty() {
            bail!("No Plex server was found on this account");
        }

        let labels: Vec<String> = candidates
            .iter()
            .map(|(rank, url, ..)| {
                let host = url.host_str().unwrap_or("?");
                let port = url.port_or_known_default().unwrap_or(0);
                if *rank == 6 { "Plex relay".to_owned() } else { format!("{host}:{port}") }
            })
            .collect();
        let (tx, rx) = mpsc::channel();
        for (index, (_, url, local, token, server_id)) in candidates.into_iter().enumerate() {
            let (tx, http, client_id) = (tx.clone(), http.clone(), account.client_id.clone());
            thread::spawn(move || {
                let probe = || -> Result<Found> {
                    let sections = headers(http.get(url.join("/library/sections")?), &client_id)
                        .header("X-Plex-Token", &token)
                        .timeout(Duration::from_secs(6))
                        .send()?
                        .error_for_status()?
                        .json::<Envelope<Sections>>()?;
                    let music: Vec<Section> = sections
                        .container
                        .directories
                        .into_iter()
                        .filter(|s| matches!(s.kind.as_str(), "artist" | "music"))
                        .collect();
                    if music.is_empty() {
                        bail!("no music library");
                    }
                    Ok(Found { endpoint: Endpoint { url, local }, token, server_id, music })
                };
                let _ = tx.send((index, probe().map_err(|error| describe_failure(&error))));
            });
        }
        drop(tx);

        // Take the best-ranked address that answered, once every better one has failed.
        let mut results: Vec<Option<std::result::Result<Found, String>>> = Vec::new();
        results.resize_with(labels.len(), || None);
        while let Ok((index, result)) = rx.recv() {
            results[index] = Some(result);
            for slot in &mut results {
                match slot {
                    Some(Ok(_)) => {
                        if let Some(Ok(found)) = slot.take() {
                            return Ok(found);
                        }
                    }
                    Some(Err(_)) => continue,
                    None => break,
                }
            }
        }
        let tried: Vec<String> = labels
            .iter()
            .zip(&results)
            .map(|(label, result)| match result {
                Some(Err(reason)) => format!("{label} {reason}"),
                _ => format!("{label} no answer"),
            })
            .collect();
        bail!(
            "Couldn't reach your Plex server ({}). Away from home? Turn on Remote Access in Plex, or enter your public address in Settings.",
            tried.join(", ")
        )
    }

    /// Sends a request, and if the server can't be reached, finds a working address
    /// (e.g. after leaving home) and tries once more.
    fn send(&self, path: &str, build: impl Fn(RequestBuilder) -> RequestBuilder) -> Result<reqwest::blocking::Response> {
        match build(self.request(path)?).send() {
            Ok(response) => Ok(response.error_for_status()?),
            Err(error) if (error.is_connect() || error.is_timeout()) && self.reconnect() => {
                Ok(build(self.request(path)?).send()?.error_for_status()?)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn request(&self, path: &str) -> Result<RequestBuilder> {
        let server = self.server();
        let url = server.join(path)?;
        if url.origin() != server.origin() {
            bail!("Unexpected Plex resource origin");
        }
        Ok(
            headers(self.http.get(url), &self.account.client_id)
                .header("X-Plex-Token", &self.token),
        )
    }

    /// Library keys to load: the wanted ones that still exist, or every music library
    /// when nothing is chosen (or none of the chosen ones are left).
    pub fn chosen_sections(&self, wanted: &[String]) -> Vec<String> {
        let keys: Vec<String> = self
            .sections
            .iter()
            .map(|(key, _)| key.clone())
            .filter(|key| wanted.contains(key))
            .collect();
        if keys.is_empty() {
            self.sections.iter().map(|(key, _)| key.clone()).collect()
        } else {
            keys
        }
    }

    /// Albums from the chosen music libraries, without duplicates.
    pub fn albums(&self, wanted: &[String]) -> Result<Vec<Entry>> {
        let mut seen = std::collections::HashSet::new();
        let mut all = Vec::new();
        let sections = self.chosen_sections(wanted);
        let sections = if sections.is_empty() {
            vec![self.library.clone()]
        } else {
            sections
        };
        for section in sections {
            for album in self.section_albums(&section)? {
                if seen.insert(album.rating_key.clone()) {
                    all.push(album);
                }
            }
        }
        Ok(all)
    }

    fn section_albums(&self, section: &str) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        loop {
            let start = entries.len().to_string();
            let page = self
                .send(&format!("/library/sections/{section}/all"), |request| {
                    request.query(&[
                        ("type", "9"),
                        ("sort", "titleSort"),
                        ("X-Plex-Container-Start", &start),
                        ("X-Plex-Container-Size", "200"),
                    ])
                })?
                .json::<Envelope<Items>>()?
                .container
                .entries;
            let done = page.len() < 200
                || entries.last().is_some_and(|last: &Entry| {
                    page.iter().any(|entry| entry.rating_key == last.rating_key)
                });
            if entries
                .last()
                .is_some_and(|last| page.iter().any(|entry| entry.rating_key == last.rating_key))
            {
                break;
            }
            entries.extend(page);
            if done {
                break;
            }
        }
        Ok(entries)
    }

    pub fn tracks(&self, album: &Entry) -> Result<Vec<Entry>> {
        Ok(self
            .send(&format!("/library/metadata/{}/children", album.rating_key), |request| request)?
            .json::<Envelope<Items>>()?
            .container
            .entries)
    }

    pub fn track_audio(&self, track: &Entry) -> Result<reqwest::blocking::Response> {
        let part = track
            .media
            .iter()
            .find_map(|m| m.parts.first())
            .context("Plex gave this track no playable file")?;
        self.send(&part.key, |request| request.timeout(Duration::from_secs(120)))
    }
}

fn url_query(id: &str, code: &str) -> String {
    let mut url = Url::parse("https://app.plex.tv").expect("static URL");
    url.query_pairs_mut()
        .append_pair("clientID", id)
        .append_pair("code", code)
        .append_pair("context[device][product]", "Sonic");
    url.query().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(sections: &[(&str, &str)]) -> Plex {
        Plex {
            http: Client::new(),
            account: Account {
                token: String::new(),
                client_id: String::new(),
            },
            endpoint: Arc::new(RwLock::new(Endpoint {
                url: Url::parse("http://127.0.0.1:32400").unwrap(),
                local: true,
            })),
            reconnecting: Arc::new(Mutex::new(None)),
            token: String::new(),
            library: sections
                .first()
                .map(|(key, _)| key.to_string())
                .unwrap_or_default(),
            sections: sections
                .iter()
                .map(|(k, t)| (k.to_string(), t.to_string()))
                .collect(),
            server_id: String::new(),
        }
    }

    #[test]
    fn library_choice_defaults_to_all_and_ignores_removed_libraries() {
        let plex = server(&[("1", "Music"), ("4", "Soundtracks"), ("7", "Live")]);
        let all = vec!["1".to_string(), "4".to_string(), "7".to_string()];
        assert_eq!(plex.chosen_sections(&[]), all);
        assert_eq!(plex.chosen_sections(&["7".into(), "1".into()]), ["1", "7"]);
        // A saved library that no longer exists is dropped; if none remain, show all.
        assert_eq!(plex.chosen_sections(&["4".into(), "9".into()]), ["4"]);
        assert_eq!(plex.chosen_sections(&["9".into()]), all);
    }
}
