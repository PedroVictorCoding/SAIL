//! Music requests answered without the reasoning model.
//!
//! "Play my radiohead playlist", "pause" and "next song" have a fixed shape and a fixed
//! answer, so SAIL handles them in code: parse the request, ask Spotify, play the best match
//! and say what is playing. That takes about a second instead of minutes of local model
//! inference. Anything this does not recognise, or cannot resolve confidently, goes to the
//! model exactly as before.
use sail_bridge::spotify::{Candidate, Kind, PlaylistIndex, Spotify, rank_playlists};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Find something and play the best match.
    Play {
        query: String,
        kind: Option<Kind>,
        mine: bool,
    },
    /// pause, resume, next, previous or status.
    Control(&'static str),
}

fn normalized(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_any<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes
        .iter()
        .find_map(|p| text.strip_prefix(p))
        .map(str::trim)
}

/// Remove greetings and politeness around a request.
fn core(text: &str) -> String {
    let mut text = normalized(text);
    loop {
        let before = text.clone();
        for prefix in [
            "hey sail ",
            "ok sail ",
            "sail ",
            "hey ",
            "please ",
            "can you ",
            "could you ",
            "would you ",
            "por favor ",
            "podes ",
            "pode ",
            "consegues ",
        ] {
            if let Some(rest) = text.strip_prefix(prefix) {
                text = rest.to_string();
            }
        }
        for suffix in [
            " please",
            " por favor",
            " on spotify",
            " no spotify",
            " for me",
            " sail",
            " now",
            " agora",
        ] {
            if let Some(rest) = text.strip_suffix(suffix) {
                text = rest.to_string();
            }
        }
        if text == before {
            return text;
        }
    }
}

const CONTROLS: &[(&str, &[&str])] = &[
    (
        "pause",
        &[
            "pause",
            "pause the music",
            "pause music",
            "pause the song",
            "pause spotify",
            "stop the music",
            "stop music",
            "stop playing",
            "pausa",
            "pausa a música",
            "pausa a musica",
            "para a música",
            "para a musica",
            "pára a música",
        ],
    ),
    (
        "resume",
        &[
            "resume",
            "resume the music",
            "resume music",
            "resume playing",
            "continue the music",
            "unpause",
            "continua",
            "continua a música",
            "continua a musica",
            "retoma",
        ],
    ),
    (
        "next",
        &[
            "next",
            "next song",
            "next track",
            "skip",
            "skip this song",
            "skip song",
            "skip this",
            "skip it",
            "próxima",
            "proxima",
            "próxima música",
            "proxima musica",
            "passa à frente",
            "passa a frente",
            "salta",
            "salta esta",
        ],
    ),
    (
        "previous",
        &[
            "previous",
            "previous song",
            "previous track",
            "go back",
            "last song",
            "anterior",
            "música anterior",
            "musica anterior",
        ],
    ),
    (
        "status",
        &[
            "what's playing",
            "what is playing",
            "what song is this",
            "what's this song",
            "what is this song",
            "o que está a tocar",
            "o que esta a tocar",
            "que música é esta",
            "que musica e esta",
        ],
    ),
];

const PLAY_VERBS: &[&str] = &[
    "play ", "put on ", "toca ", "tocar ", "põe ", "poe ", "coloca ", "mete ",
];
const VAGUE: &[&str] = &[
    "",
    "music",
    "some music",
    "something",
    "anything",
    "música",
    "musica",
    "algo",
    "alguma coisa",
];

/// Recognise a music request. `None` leaves the turn to the reasoning model.
pub fn parse(prompt: &str) -> Option<Intent> {
    let text = core(prompt);
    if let Some((action, _)) = CONTROLS
        .iter()
        .find(|(_, phrases)| phrases.contains(&text.as_str()))
    {
        return Some(Intent::Control(action));
    }
    let rest = strip_any(&text, PLAY_VERBS)?;
    if VAGUE.contains(&rest) || rest.split_whitespace().count() > 12 {
        return None;
    }
    let (mine, rest) = match strip_any(rest, &["my ", "a minha ", "minha ", "o meu ", "meu "]) {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let mut kind = None;
    let mut query = rest.to_string();
    for (prefixes, k) in [
        (
            &[
                "songs by ",
                "music by ",
                "something by ",
                "músicas de ",
                "musicas de ",
                "the artist ",
                "artist ",
            ][..],
            Kind::Artist,
        ),
        (
            &["the playlist ", "playlist ", "a playlist "][..],
            Kind::Playlist,
        ),
        (
            &[
                "the song ",
                "song ",
                "the track ",
                "track ",
                "a música ",
                "a musica ",
            ][..],
            Kind::Track,
        ),
        (
            &["the album ", "album ", "o álbum ", "o album ", "álbum "][..],
            Kind::Album,
        ),
    ] {
        if let Some(r) = strip_any(&query, prefixes) {
            kind = Some(k);
            query = r.to_string();
            break;
        }
    }
    if kind.is_none() {
        for (suffix, k) in [
            (" playlist", Kind::Playlist),
            (" song", Kind::Track),
            (" track", Kind::Track),
            (" album", Kind::Album),
            (" álbum", Kind::Album),
        ] {
            if let Some(r) = query.strip_suffix(suffix) {
                kind = Some(k);
                query = r.trim().to_string();
                break;
            }
        }
    }
    if mine && kind.is_none() {
        kind = Some(Kind::Playlist);
    }
    // Only the owner's playlists can be "mine" (liked songs etc. go to the model).
    if mine && kind != Some(Kind::Playlist) {
        return None;
    }
    if kind != Some(Kind::Artist) {
        query = query.replace(" by ", " ");
    }
    // "a minha playlist de radiohead": the subject follows "de"/"do"/"da"/"with".
    let query = strip_any(&query, &["de ", "do ", "da ", "with ", "of "])
        .unwrap_or(&query)
        .trim()
        .to_string();
    if mine
        && [
            "liked songs",
            "liked",
            "favorites",
            "favourites",
            "músicas favoritas",
        ]
        .contains(&query.as_str())
    {
        return None;
    }
    if VAGUE.contains(&query.as_str()) {
        return None;
    }
    Some(Intent::Play { query, kind, mine })
}

/// Answer a music request without the model, or `None` to let the model handle the turn.
pub async fn try_handle(spotify: &Arc<Spotify>, prompt: &str) -> Option<String> {
    let started = Instant::now();
    let reply = answer(spotify, prompt).await?;
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Music request answered without the reasoning model"
    );
    Some(reply)
}

async fn answer(spotify: &Arc<Spotify>, prompt: &str) -> Option<String> {
    let intent = parse(prompt)?;
    if !spotify.configured() {
        return Some(
            "Spotify isn't set up yet. Use Set up Spotify in the Now Playing card.".into(),
        );
    }
    if !spotify.connected().await {
        return Some(
            "Spotify isn't connected. Use Connect Spotify in the Now Playing card.".into(),
        );
    }
    match intent {
        Intent::Control(action) => Some(control(spotify, action).await),
        Intent::Play { query, kind, mine } => {
            let found = if mine {
                own_playlist(spotify, &query).await?
            } else {
                let result = match kind {
                    Some(kind) => spotify.search(&query, kind, 5).await,
                    None => spotify.search_any(&query).await,
                };
                match result {
                    Ok(found) => found,
                    Err(error) => return Some(error.message),
                }
            };
            let Some(best) = found.into_iter().next() else {
                return Some(format!("I couldn't find \"{query}\" on Spotify."));
            };
            Some(match spotify.play(&best.uri).await {
                Ok(done) => format!(
                    "Playing {} on {}.",
                    best.describe(),
                    done["device"]
                        .as_str()
                        .filter(|d| !d.is_empty())
                        .unwrap_or("Spotify")
                ),
                Err(error) => error.message,
            })
        }
    }
}

/// The owner's playlist for a query, from the playlist index (names and the artists in
/// each). `None` when not confident: the model then decides from the full list.
async fn own_playlist(spotify: &Arc<Spotify>, query: &str) -> Option<Vec<Candidate>> {
    let index = match spotify.playlist_index() {
        Some(index) => index,
        // Not indexed yet (being built now): names alone can still be enough.
        None => PlaylistIndex {
            built_at: 0,
            playlists: spotify
                .my_playlists()
                .await
                .ok()?
                .into_iter()
                .map(|playlist| sail_bridge::spotify::IndexedPlaylist {
                    playlist,
                    tracks: 0,
                    artists: Vec::new(),
                })
                .collect(),
        },
    };
    let ranked = rank_playlists(query, &index);
    if !ranked.first().is_some_and(|m| m.confident()) {
        return None;
    }
    Some(
        ranked
            .into_iter()
            .filter(|m| m.confident())
            .map(|m| m.playlist)
            .collect(),
    )
}

async fn control(spotify: &Spotify, action: &str) -> String {
    if action != "status"
        && let Err(error) = spotify.control(action).await
    {
        return error.message;
    }
    if action != "status" {
        return match action {
            "pause" => "Paused.",
            "resume" => "Resuming.",
            "next" => "Skipping to the next song.",
            _ => "Going back.",
        }
        .into();
    }
    match spotify.now_playing().await {
        Ok(now) if !now.is_null() => {
            let device = now["device"].as_str().unwrap_or_default();
            format!(
                "{} '{}' by {}{}.",
                if now["is_playing"] == true {
                    "Playing"
                } else {
                    "Paused on"
                },
                now["name"].as_str().unwrap_or_default(),
                now["by"].as_str().unwrap_or_default(),
                if device.is_empty() {
                    String::new()
                } else {
                    format!(" on {device}")
                }
            )
        }
        Ok(_) => "Nothing is playing on Spotify.".into(),
        Err(error) => error.message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn play(query: &str, kind: Option<Kind>, mine: bool) -> Option<Intent> {
        Some(Intent::Play {
            query: query.into(),
            kind,
            mine,
        })
    }

    /// The owner's example conversation, end to end against a stand-in Spotify Web API.
    #[tokio::test]
    async fn the_radiohead_conversation_runs_without_the_model() {
        use axum::{Json, Router, routing::get};
        use serde_json::Value;
        let played = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let sink = played.clone();
        let tracks = |artists: &'static [&'static str]| json!({"next": null, "items": artists.iter().map(|a| json!({"track":{"artists":[{"name":a}]}})).collect::<Vec<_>>()});
        let api = Router::new()
            .route("/v1/me/playlists", get(|| async { Json(json!({"next":null,"items":[
                {"name":"Road trip","uri":"spotify:playlist:road","owner":{"display_name":"Pedro Victor"},"tracks":{"total":4}},
                {"name":"Cabeça de Radio","uri":"spotify:playlist:cabeca","owner":{"display_name":"Pedro Victor"},"tracks":{"total":4}}]})) }))
            .route("/v1/playlists/road/tracks", get(move || async move { Json(tracks(&["Blur", "Oasis", "Pulp", "Radiohead"])) }))
            .route("/v1/playlists/cabeca/tracks", get(move || async move { Json(tracks(&["Radiohead", "Radiohead", "Radiohead", "Thom Yorke"])) }))
            .route("/v1/me/player/devices", get(|| async { Json(json!({"devices":[{"id":"phone1","name":"Pedro's phone","is_active":true,"is_restricted":false}]})) }))
            .route("/v1/me/player/play", axum::routing::put(move |Json(body): Json<Value>| {
                let sink = sink.clone();
                async move { sink.lock().unwrap().push(body); axum::http::StatusCode::NO_CONTENT }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
        let dir = std::env::temp_dir().join(format!("sail-music-fast-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), json!({"client_id":"0123456789abcdef0123456789abcdef","redirect_uri":"https://127.0.0.1:8088/api/spotify/callback"}).to_string()).unwrap();
        std::fs::write(
            dir.join("token.json"),
            json!({"access_token":"a","refresh_token":"r","expires_at":u64::MAX/2}).to_string(),
        )
        .unwrap();
        let spotify = Arc::new(Spotify::with_endpoints(dir.clone(), &base, &base));

        // Before the index exists, a name that does not say "radiohead" is left to the model.
        assert_eq!(
            try_handle(&spotify, "Play my radiohead playlist.").await,
            None
        );
        spotify.build_index().await.unwrap();

        // Asking plays the best match straight away, without a confirmation question.
        let started = Instant::now();
        let done = try_handle(&spotify, "Play my radiohead playlist.")
            .await
            .unwrap();
        assert_eq!(
            done,
            "Playing the playlist 'Cabeça de Radio' by Pedro Victor on Pedro's phone."
        );
        assert_eq!(
            played.lock().unwrap()[0]["context_uri"],
            "spotify:playlist:cabeca"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );

        // A bare "yes" or an unrelated turn is left to the model and plays nothing.
        assert_eq!(try_handle(&spotify, "yes").await, None);
        assert_eq!(try_handle(&spotify, "what's the weather?").await, None);
        assert_eq!(played.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn play_requests_are_recognised() {
        assert_eq!(
            parse("Play my radiohead playlist."),
            play("radiohead", Some(Kind::Playlist), true)
        );
        assert_eq!(
            parse("Hey SAIL, play my Radiohead playlist please"),
            play("radiohead", Some(Kind::Playlist), true)
        );
        assert_eq!(
            parse("play my chill"),
            play("chill", Some(Kind::Playlist), true)
        );
        assert_eq!(
            parse("Play Creep by Radiohead"),
            play("creep radiohead", None, false)
        );
        assert_eq!(
            parse("play the song creep"),
            play("creep", Some(Kind::Track), false)
        );
        assert_eq!(
            parse("play the album kid a"),
            play("kid a", Some(Kind::Album), false)
        );
        assert_eq!(
            parse("play songs by the beatles"),
            play("the beatles", Some(Kind::Artist), false)
        );
        assert_eq!(
            parse("Toca a minha playlist de radiohead"),
            play("radiohead", Some(Kind::Playlist), true)
        );
        assert_eq!(parse("põe radiohead"), play("radiohead", None, false));
    }

    #[test]
    fn controls_are_whole_utterances_only() {
        assert_eq!(parse("Pause"), Some(Intent::Control("pause")));
        assert_eq!(parse("next song please"), Some(Intent::Control("next")));
        assert_eq!(parse("Skip this song."), Some(Intent::Control("next")));
        assert_eq!(parse("what's playing?"), Some(Intent::Control("status")));
        assert_eq!(parse("pausa a música"), Some(Intent::Control("pause")));
        assert_eq!(parse("why did you pause the build"), None);
        assert_eq!(parse("next steps for the project"), None);
    }

    #[test]
    fn everything_else_goes_to_the_model() {
        for prompt in [
            "play some music",
            "play",
            "what should I play at the party?",
            "play my liked songs",
            "Plan: play radiohead",
            "explain how playlists work",
        ] {
            assert_eq!(parse(prompt), None, "{prompt}");
        }
    }
}
