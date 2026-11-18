use anyhow::{Context, Result, bail};
use reqwest::{
    Url,
    blocking::{Client, RequestBuilder},
};
use serde::{Deserialize, Serialize};
use std::{
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Plex {
    pub http: Client,
    account: Account,
    pub server: Url,
    token: String,
    pub library: String,
    /// Every music library on the server as (key, title), in server order.
    pub sections: Vec<(String, String)>,
    pub server_id: String,
    pub is_local: bool,
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
    #[serde(default)]
    connections: Vec<Connection>,
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

    pub fn refresh_connection(&self) -> Result<Self> {
        Self::discover(
            self.http.clone(),
            self.account.clone(),
            Some((&self.server_id, &self.library)),
        )
    }

    fn discover(http: Client, account: Account, current: Option<(&str, &str)>) -> Result<Self> {
        let devices: Vec<Device> = headers(
            http.get("https://plex.tv/api/v2/resources?includeHttps=1&includeRelay=0"),
            &account.client_id,
        )
        .header("X-Plex-Token", &account.token)
        .send()?
        .error_for_status()?
        .json()?;
        for mut device in devices
            .into_iter()
            .filter(|device| device.provides.split(',').any(|p| p == "server"))
        {
            if current.is_some_and(|(server, _)| device.client_identifier != server) {
                continue;
            }
            let token = device.access_token.unwrap_or_else(|| account.token.clone());
            device
                .connections
                .sort_by_key(|c| (!c.local, c.uri.scheme() != "https"));
            for connection in device
                .connections
                .into_iter()
                .filter(|c| !c.relay && matches!(c.uri.scheme(), "http" | "https"))
            {
                let mut plex = Self {
                    http: http.clone(),
                    account: account.clone(),
                    server: connection.uri,
                    token: token.clone(),
                    library: String::new(),
                    sections: Vec::new(),
                    server_id: device.client_identifier.clone(),
                    is_local: connection.local,
                };
                let sections = plex.request("/library/sections").and_then(|r| {
                    Ok(r.timeout(Duration::from_secs(5))
                        .send()?
                        .error_for_status()?
                        .json::<Envelope<Sections>>()?)
                });
                if let Ok(sections) = sections {
                    let music: Vec<Section> = sections
                        .container
                        .directories
                        .into_iter()
                        .filter(|s| matches!(s.kind.as_str(), "artist" | "music"))
                        .collect();
                    if let Some(section) = music
                        .iter()
                        .find(|s| current.is_none_or(|(_, library)| s.key == library))
                        .or(music.first())
                    {
                        plex.library = section.key.clone();
                        plex.sections = music.into_iter().map(|s| (s.key, s.title)).collect();
                        return Ok(plex);
                    }
                }
            }
        }
        bail!("No reachable Plex music library was found");
    }

    pub fn request(&self, path: &str) -> Result<RequestBuilder> {
        let url = self.server.join(path)?;
        if url.origin() != self.server.origin() {
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
                .request(&format!("/library/sections/{section}/all"))?
                .query(&[
                    ("type", "9"),
                    ("sort", "titleSort"),
                    ("X-Plex-Container-Start", &start),
                    ("X-Plex-Container-Size", "200"),
                ])
                .send()?
                .error_for_status()?
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
            .request(&format!("/library/metadata/{}/children", album.rating_key))?
            .send()?
            .error_for_status()?
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
        Ok(self
            .request(&part.key)?
            .timeout(Duration::from_secs(120))
            .send()?
            .error_for_status()?)
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
            server: Url::parse("http://127.0.0.1:32400").unwrap(),
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
            is_local: true,
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
