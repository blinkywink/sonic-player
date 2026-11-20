use crate::plex::Entry;
use anyhow::{Result, bail};
use reqwest::{Url, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone)]
pub struct ClientKeys {
    http: Client,
    key: String,
    secret: String,
}

#[derive(Clone)]
pub struct Pending {
    client: ClientKeys,
    token: String,
}

#[derive(Clone)]
pub struct Session {
    client: ClientKeys,
    session_key: String,
    pub username: String,
}

#[derive(Serialize, Deserialize)]
struct SavedSession {
    key: String,
    secret: String,
    session_key: String,
    username: String,
}

const KEYRING_ACCOUNT: &str = "Last.fm";

fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

impl ClientKeys {
    pub fn authorize(key: String, secret: String) -> Result<Pending> {
        if key.trim().is_empty() || secret.trim().is_empty() {
            bail!("Enter your Last.fm API key and secret");
        }
        let keys = Self {
            http: http_client()?,
            key: key.trim().to_owned(),
            secret: secret.trim().to_owned(),
        };
        let response = keys.call("auth.getToken", [], false)?;
        let token = response["token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Last.fm did not return a login token"))?
            .to_owned();
        let mut url = Url::parse("https://www.last.fm/api/auth/")?;
        url.query_pairs_mut()
            .append_pair("api_key", &keys.key)
            .append_pair("token", &token);
        // On iPhone the `open` crate needs a jailbreak tool; use UIKit instead.
        #[cfg(target_os = "ios")]
        crate::ios::open_url(url.as_str());
        #[cfg(not(target_os = "ios"))]
        open::that_detached(url.as_str())?;
        Ok(Pending {
            client: keys,
            token,
        })
    }

    fn call<'a>(
        &self,
        method: &'a str,
        params: impl IntoIterator<Item = (&'a str, &'a str)>,
        post: bool,
    ) -> Result<Value> {
        let mut params: BTreeMap<_, _> = params.into_iter().collect();
        params.insert("api_key", &self.key);
        params.insert("method", method);
        let mut signature = String::new();
        for (key, value) in &params {
            signature.push_str(key);
            signature.push_str(value);
        }
        signature.push_str(&self.secret);
        let digest = format!("{:x}", md5::compute(signature.as_bytes()));
        params.insert("api_sig", &digest);
        params.insert("format", "json");
        let endpoint = "https://ws.audioscrobbler.com/2.0/";
        let reply = if post {
            self.http.post(endpoint).form(&params)
        } else {
            self.http.get(endpoint).query(&params)
        }
        .send()
        .map_err(|_| anyhow::anyhow!("Could not reach Last.fm"))?;
        let status = reply.status();
        let value: Value = reply
            .json()
            .map_err(|_| anyhow::anyhow!("Last.fm returned an invalid response ({status})"))?;
        if let Some(error) = value["error"].as_i64() {
            match error {
                9 => bail!("Last.fm session expired. Disconnect and sign in again."),
                14 => bail!("Approve Sonic in the browser, then finish sign-in again."),
                15 => bail!("Last.fm approval expired. Start sign-in again."),
                _ => {
                    let message = value["message"].as_str().unwrap_or("Request failed");
                    bail!("Last.fm error {error}: {message}");
                }
            }
        }
        if !status.is_success() {
            bail!("Last.fm returned {status}");
        }
        Ok(value)
    }
}

impl Pending {
    pub fn finish(&self) -> Result<Session> {
        let value = self
            .client
            .call("auth.getSession", [("token", self.token.as_str())], false)?;
        let session = &value["session"];
        let field = |key: &str| {
            session[key]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("Last.fm did not return a session"))
        };
        let session = Session {
            client: self.client.clone(),
            session_key: field("key")?,
            username: field("name")?,
        };
        Ok(session)
    }
}

impl Session {
    pub fn save(&self) -> Result<()> {
        crate::credentials::save(
            KEYRING_ACCOUNT,
            &SavedSession {
                key: self.client.key.clone(),
                secret: self.client.secret.clone(),
                session_key: self.session_key.clone(),
                username: self.username.clone(),
            },
        )
    }
    pub fn restore() -> Result<Option<Self>> {
        let Some(saved) = crate::credentials::read::<SavedSession>(KEYRING_ACCOUNT)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            client: ClientKeys {
                http: http_client()?,
                key: saved.key,
                secret: saved.secret,
            },
            session_key: saved.session_key,
            username: saved.username,
        }))
    }

    pub fn forget() -> Result<()> {
        crate::credentials::remove(KEYRING_ACCOUNT)
    }

    pub fn test(&self) -> Result<()> {
        let response =
            self.client
                .call("user.getInfo", [("sk", self.session_key.as_str())], false)?;
        if !response["user"]["name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(&self.username))
        {
            bail!("Last.fm did not verify the signed-in account");
        }
        Ok(())
    }

    pub fn now_playing(&self, track: &Entry) -> Result<()> {
        self.send_track(track, None)
    }

    pub fn scrobble(&self, track: &Entry, started_at: u64) -> Result<()> {
        self.send_track(track, Some(started_at))
    }

    /// Sends up to 50 scrobbles in one request (Last.fm's limit per call).
    pub fn scrobble_batch(&self, scrobbles: &[(Entry, u64)]) -> Result<()> {
        let mut owned: Vec<(String, String)> = vec![("sk".into(), self.session_key.clone())];
        for (index, (track, started_at)) in scrobbles.iter().take(50).enumerate() {
            if track.grandparent_title.trim().is_empty() {
                continue;
            }
            owned.push((format!("artist[{index}]"), track.grandparent_title.clone()));
            owned.push((format!("track[{index}]"), track.title.clone()));
            owned.push((format!("album[{index}]"), track.parent_title.clone()));
            owned.push((format!("duration[{index}]"), (track.duration / 1000).to_string()));
            owned.push((format!("timestamp[{index}]"), started_at.to_string()));
        }
        if owned.len() == 1 {
            return Ok(());
        }
        let params = owned.iter().map(|(key, value)| (key.as_str(), value.as_str()));
        self.client.call("track.scrobble", params, true)?;
        Ok(())
    }

    fn send_track(&self, track: &Entry, started_at: Option<u64>) -> Result<()> {
        if track.grandparent_title.trim().is_empty() {
            bail!("This track is missing an artist")
        }
        let duration = (track.duration / 1000).to_string();
        let timestamp = started_at.map(|x| x.to_string());
        let mut params = vec![
            ("sk", self.session_key.as_str()),
            ("artist", track.grandparent_title.as_str()),
            ("track", track.title.as_str()),
            ("album", track.parent_title.as_str()),
            ("duration", duration.as_str()),
        ];
        let method = if let Some(timestamp) = timestamp.as_deref() {
            params.push(("timestamp", timestamp));
            "track.scrobble"
        } else {
            "track.updateNowPlaying"
        };
        let response = self.client.call(method, params, true)?;
        let result = if started_at.is_some() {
            let scrobble = &response["scrobbles"]["scrobble"];
            if scrobble.is_array() {
                &scrobble[0]
            } else {
                scrobble
            }
        } else {
            &response["nowplaying"]
        };
        if let Some(code) = result["ignoredMessage"]["code"]
            .as_str()
            .and_then(|code| code.parse::<u64>().ok())
            .or_else(|| result["ignoredMessage"]["code"].as_u64())
            && code != 0
        {
            let reason = result["ignoredMessage"]["#text"]
                .as_str()
                .filter(|text| !text.is_empty())
                .unwrap_or("Track ignored");
            bail!("Last.fm did not accept the track ({code}): {reason}");
        }
        Ok(())
    }
}
