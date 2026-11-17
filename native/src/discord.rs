use discord_rich_presence::{
    DiscordIpc, DiscordIpcClient,
    activity::{Activity, Assets},
};
use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(crate) struct Presence {
    pub title: String,
    pub artist: String,
    pub album: String,
}

#[derive(Clone)]
pub(crate) struct Update {
    pub id: String,
    pub image: String,
    pub presence: Option<Presence>,
    pub test: bool,
}

pub(crate) fn start() -> (Sender<Update>, Receiver<String>) {
    let (tx, rx) = mpsc::channel::<Update>();
    let (status, messages) = mpsc::channel();
    std::thread::spawn(move || {
        let mut client: Option<(String, DiscordIpcClient)> = None;
        let mut desired: Option<Update> = None;
        let mut test_until: Option<Instant> = None;
        let mut retry = Duration::from_secs(5);
        let mut previous_status = String::new();
        loop {
            let interval = if let Some(deadline) = test_until {
                deadline.saturating_duration_since(Instant::now())
            } else if desired
                .as_ref()
                .is_some_and(|update| update.presence.is_some())
            {
                retry
            } else {
                Duration::from_secs(3600)
            };
            let update = match rx.recv_timeout(interval) {
                Ok(update) if update.test => {
                    test_until = Some(Instant::now() + Duration::from_secs(5));
                    if desired.is_none() {
                        desired = Some(Update {
                            test: false,
                            ..update.clone()
                        });
                    }
                    update
                }
                Ok(update) => {
                    test_until = None;
                    desired = Some(update.clone());
                    update
                }
                Err(RecvTimeoutError::Timeout) => {
                    test_until = None;
                    let Some(update) = desired.clone() else {
                        continue;
                    };
                    update
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut report = |message: &str| {
                if message != previous_status || update.test {
                    previous_status = message.to_owned();
                    let _ = status.send(previous_status.clone());
                }
            };
            let has_client = client.as_ref().is_some_and(|(id, _)| *id == update.id);
            if !has_client {
                if let Some((_, mut old)) = client.take() {
                    let _ = old.clear_activity();
                    let _ = old.close();
                }
            }
            if update.id.is_empty() {
                report("");
                continue;
            }
            if !update.id.bytes().all(|byte| byte.is_ascii_digit()) {
                report("Enter a numeric Discord Application ID.");
                desired = None;
                continue;
            }
            if update.presence.is_none() && !update.test {
                if let Some((_, active)) = &mut client {
                    if active.clear_activity().is_err() {
                        client = None;
                    }
                }
                report("Ready to share the next track.");
                continue;
            }
            if client.is_none() {
                let mut next = DiscordIpcClient::new(&update.id);
                if next.connect().is_ok() {
                    client = Some((update.id, next));
                } else {
                    report("Waiting for the Discord desktop app…");
                    retry = Duration::from_secs(5);
                    continue;
                }
            }
            let track = if update.test {
                Presence {
                    title: "Test track".into(),
                    artist: "Test artist".into(),
                    album: "Test album".into(),
                }
            } else if let Some(track) = update.presence {
                track
            } else {
                continue;
            };
            let mut activity =
                Activity::new().details(track.title.chars().take(128).collect::<String>());
            if !track.artist.is_empty() {
                activity = activity.state(track.artist.chars().take(128).collect::<String>());
            }
            if !update.image.is_empty() {
                activity = activity.assets(
                    Assets::new()
                        .large_image(update.image)
                        .large_text(track.album.chars().take(128).collect::<String>()),
                );
            }
            if let Some((_, active)) = &mut client {
                if active.set_activity(activity).is_ok() {
                    report(if update.test {
                        "Test activity sent. Check your Discord profile."
                    } else {
                        "Track activity sent to Discord."
                    });
                    retry = Duration::from_secs(30);
                } else {
                    report("Discord disconnected. Reconnecting…");
                    retry = Duration::from_secs(5);
                    client = None;
                }
            }
        }
        if let Some((_, mut active)) = client {
            let _ = active.clear_activity();
            let _ = active.close();
        }
    });
    (tx, messages)
}
