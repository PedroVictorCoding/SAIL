//! Spoken confirmation before SAIL plays music.
//!
//! `spotify_find` offers candidates in one turn and SAIL asks "Should we proceed by playing
//! the playlist with the name: '…' created by '…'?". `spotify_play` runs in a later turn only
//! when this gate agrees: the item was offered to this session in an earlier turn, its exact
//! name appeared in SAIL's previous reply, and the owner's reply agrees or names it. The rule
//! is enforced here, not left to the model.
use sail_bridge::spotify::Candidate;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// An offer older than this needs a fresh search.
const OFFER_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_SESSIONS: usize = 64;

struct Offer {
    turn: u64,
    at: Instant,
    items: Vec<Candidate>,
}

fn offers() -> &'static Mutex<HashMap<String, Offer>> {
    static OFFERS: OnceLock<Mutex<HashMap<String, Offer>>> = OnceLock::new();
    OFFERS.get_or_init(Default::default)
}

/// Every candidate in a `spotify_find` observation (best, alternatives, or the full list).
pub fn candidates_in(output: &Value) -> Vec<Candidate> {
    let parsed;
    let output = match output.as_str() {
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(_) => return Vec::new(),
        },
        None => output,
    };
    let mut items: Vec<Candidate> = Vec::new();
    let one = |v: &Value| serde_json::from_value::<Candidate>(v.clone()).ok();
    items.extend(one(&output["best"]));
    for key in ["alternatives", "playlists"] {
        items.extend(output[key].as_array().into_iter().flatten().filter_map(one));
    }
    items
}

/// Remember what SAIL offered this session in turn `turn` (replacing earlier offers).
pub fn record(session: &str, turn: u64, output: &Value) {
    let items = candidates_in(output);
    if items.is_empty() {
        return;
    }
    let mut offers = offers().lock().expect("media offers");
    offers.retain(|_, offer| offer.at.elapsed() < OFFER_TTL);
    if offers.len() >= MAX_SESSIONS
        && !offers.contains_key(session)
        && let Some(oldest) = offers
            .iter()
            .min_by_key(|(_, o)| o.at)
            .map(|(k, _)| k.clone())
    {
        offers.remove(&oldest);
    }
    offers.insert(
        session.into(),
        Offer {
            turn,
            at: Instant::now(),
            items,
        },
    );
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

/// Does the owner's reply agree? Any refusal word wins over agreement.
pub fn is_affirmative(reply: &str) -> bool {
    let text = format!(" {} ", normalized(reply));
    const NO: [&str; 14] = [
        " no ",
        " nope ",
        " not ",
        " don't ",
        " dont ",
        " cancel ",
        " stop ",
        " wait ",
        " never ",
        " não ",
        " nao ",
        " nem ",
        " espera ",
        " cancela ",
    ];
    if NO.iter().any(|w| text.contains(w)) {
        return false;
    }
    const YES: [&str; 32] = [
        " yes ",
        " yeah ",
        " yep ",
        " yup ",
        " sure ",
        " ok ",
        " okay ",
        " go ahead ",
        " proceed ",
        " do it ",
        " play it ",
        " please do ",
        " sounds good ",
        " correct ",
        " that one ",
        " that's it ",
        " confirm ",
        " affirmative ",
        " absolutely ",
        " of course ",
        " sim ",
        " claro ",
        " pode ",
        " podes ",
        " avança ",
        " avanca ",
        " bora ",
        " isso ",
        " confirmo ",
        " exato ",
        " exatamente ",
        " força ",
    ];
    YES.iter().any(|w| text.contains(w))
}

/// Decide whether `uri` may play now. Returns the confirmed candidate, or the reason it may
/// not, worded as an instruction for the model.
pub fn authorize(
    session: &str,
    turn: u64,
    uri: &str,
    reply: &str,
    previous_reply: &str,
) -> Result<Candidate, String> {
    let offers = offers().lock().expect("media offers");
    let Some(offer) = offers.get(session).filter(|o| o.at.elapsed() < OFFER_TTL) else {
        return Err("Not played: nothing was offered in this conversation yet. Call spotify_find, ask the confirmation question, and wait for the user's answer.".into());
    };
    if offer.turn >= turn {
        return Err("Not played: ask the confirmation question from spotify_find first and wait for the user's answer in their next message.".into());
    }
    let Some(item) = offer.items.iter().find(|c| c.uri == uri) else {
        return Err("Not played: that item was not among the results offered to the user. Use a uri returned by spotify_find.".into());
    };
    let asked = normalized(previous_reply);
    if !asked.contains(&normalized(&item.name)) {
        return Err(format!(
            "Not played: SAIL did not name this item when asking. Ask the user: {}",
            item.confirmation_question()
        ));
    }
    let answer = normalized(reply);
    let named = answer.contains(&normalized(&item.name));
    if !(named || is_affirmative(reply)) {
        return Err(format!(
            "Not played: the user has not agreed yet. If they still want it, ask: {}",
            item.confirmation_question()
        ));
    }
    Ok(item.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sail_bridge::spotify::Kind;
    use serde_json::json;

    fn playlist(name: &str, uri: &str) -> Value {
        json!({"kind":"playlist","name":name,"by":"Pedro Victor","uri":uri})
    }

    fn offer(session: &str, turn: u64) {
        let output = json!({
            "best": playlist("Cabeça de Radio", "spotify:playlist:aaa"),
            "alternatives": [playlist("Radiohead Essentials", "spotify:playlist:bbb")],
        });
        record(session, turn, &Value::String(output.to_string()));
    }

    const ASKED: &str = "Should we proceed by playing the playlist with the name: 'Cabeça de Radio' created by 'Pedro Victor'?";

    #[test]
    fn the_example_conversation_plays_after_yes() {
        offer("s-example", 1);
        let item = authorize(
            "s-example",
            2,
            "spotify:playlist:aaa",
            "Yes go ahead",
            ASKED,
        )
        .unwrap();
        assert_eq!(item.kind, Kind::Playlist);
        assert_eq!(item.name, "Cabeça de Radio");
    }

    #[test]
    fn nothing_plays_in_the_turn_that_offered_it() {
        offer("s-same", 3);
        assert!(
            authorize(
                "s-same",
                3,
                "spotify:playlist:aaa",
                "Play my radiohead playlist",
                ASKED
            )
            .is_err()
        );
    }

    #[test]
    fn refusals_and_unrelated_replies_do_not_play() {
        offer("s-no", 1);
        for reply in ["No", "no, wait", "Não", "What time is it?", "don't play it"] {
            assert!(
                authorize("s-no", 2, "spotify:playlist:aaa", reply, ASKED).is_err(),
                "{reply}"
            );
        }
    }

    #[test]
    fn only_offered_items_that_were_named_may_play() {
        offer("s-named", 1);
        assert!(authorize("s-named", 2, "spotify:playlist:zzz", "yes", ASKED).is_err());
        // Offered, but SAIL asked about a different playlist.
        assert!(authorize("s-named", 2, "spotify:playlist:bbb", "yes", ASKED).is_err());
        // Naming the alternative after SAIL listed it is a choice.
        let listed = format!("{ASKED} Or 'Radiohead Essentials' by Spotify?");
        assert!(
            authorize(
                "s-named",
                2,
                "spotify:playlist:bbb",
                "Radiohead Essentials please",
                &listed
            )
            .is_ok()
        );
    }

    #[test]
    fn other_sessions_cannot_use_an_offer() {
        offer("s-owner", 1);
        assert!(authorize("s-other", 2, "spotify:playlist:aaa", "yes", ASKED).is_err());
    }

    #[test]
    fn agreement_words_in_english_and_portuguese() {
        for yes in [
            "Yes go ahead",
            "sure!",
            "OK",
            "Sim, pode",
            "bora",
            "go ahead please",
        ] {
            assert!(is_affirmative(yes), "{yes}");
        }
        for no in ["no", "nope", "not that one", "não, espera", "maybe later"] {
            assert!(!is_affirmative(no), "{no}");
        }
    }
}
