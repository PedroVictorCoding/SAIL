use sail_flow::gateway::telegram::{
    BotApi, BotToken, FakeBot, Incoming, IncomingKind, Outgoing, parse_sent, parse_updates, request,
};
use serde_json::{Value, json};

fn token() -> String {
    format!("123456789:{}", "A".repeat(35))
}

fn private_message(update_id: i64, extra: &str) -> String {
    format!(
        r#"{{"update_id":{update_id},"message":{{"message_id":7,"from":{{"id":42,"is_bot":false,"first_name":"Ada","last_name":"Lovelace","username":"ada"}},"chat":{{"id":42,"type":"private"}},"date":1700000000{extra}}}}}"#
    )
}

fn updates(items: &str) -> String {
    format!(r#"{{"ok":true,"result":[{items}]}}"#)
}

#[test]
fn token_shape_and_no_echo() {
    let parsed = BotToken::parse(&token()).expect("valid token");
    assert_eq!(parsed.expose(), token());
    assert_eq!(format!("{parsed:?}"), "BotToken(…)");
    assert_eq!(format!("{parsed}"), "BotToken(…)");
    assert!(!format!("{parsed:?}").contains(&"A".repeat(35)));

    let too_short = format!("123:{}", "B".repeat(34));
    let error = BotToken::parse(&too_short).expect_err("short secret");
    assert!(!error.contains(&"B".repeat(34)));
    assert!(BotToken::parse("not-a-token").is_err());
    assert!(BotToken::parse("123:abc def").is_err());
    assert!(BotToken::parse(":secretsecretsecretsecretsecretsecretsecret").is_err());
    assert!(BotToken::parse(&format!("12x:{}", "C".repeat(35))).is_err());
}

#[test]
fn parses_text_voice_audio_button_photo() {
    let items = [
        private_message(100, r#","text":"hello""#),
        private_message(
            101,
            r#","voice":{"file_id":"voice-1","duration":3,"mime_type":"audio/ogg"}"#,
        ),
        private_message(
            102,
            r#","audio":{"file_id":"audio-1","duration":9,"mime_type":"audio/mpeg"}"#,
        ),
        private_message(103, r#","photo":[{"file_id":"p1","width":10,"height":10}]"#),
        r#"{"update_id":104,"callback_query":{"id":"cb-1","from":{"id":42,"is_bot":false,"first_name":"Ada"},"message":{"message_id":9,"chat":{"id":42,"type":"private"},"date":1700000001},"data":"a1b2c3"}}"#.to_string(),
        r#"{"update_id":105,"edited_message":{"message_id":7,"from":{"id":42,"is_bot":false},"chat":{"id":42,"type":"private"},"date":1700000002,"text":"edited"}}"#.to_string(),
        r#"{"update_id":106,"message":{"message_id":8,"from":{"id":42,"is_bot":false},"chat":{"id":-10,"type":"group"},"date":1700000003,"text":"group"}}"#.to_string(),
        r#"{"update_id":107,"message":{"message_id":9,"from":{"id":99,"is_bot":true},"chat":{"id":99,"type":"private"},"date":1700000004,"text":"bot"}}"#.to_string(),
        r#"{"update_id":108,"message":{"message_id":10,"chat":{"id":42,"type":"private"},"date":1700000005,"text":"no sender"}}"#.to_string(),
    ];
    let body = updates(&items.join(",")).into_bytes();
    let first = parse_updates(&body, 0).expect("updates");
    let second = parse_updates(&body, 0).expect("updates");
    assert_eq!(first, second, "parsing is pure");
    let (list, next) = first;
    assert_eq!(next, 109);
    assert_eq!(list.len(), 5);
    assert_eq!(list[0].sender_name, "Ada Lovelace");
    assert_eq!(
        list[0].kind,
        IncomingKind::Text {
            text: "hello".to_string()
        }
    );
    assert!(matches!(list[1].kind, IncomingKind::Voice { .. }));
    if let IncomingKind::Voice { file_id, mime, .. } = &list[2].kind {
        assert_eq!(file_id, "audio-1");
        assert_eq!(mime, "audio/mpeg");
    } else {
        panic!("audio should be a voice note");
    }
    assert_eq!(
        list[3].kind,
        IncomingKind::Unsupported {
            what: "photo".to_string()
        }
    );
    assert_eq!(
        list[4].kind,
        IncomingKind::Button {
            callback_id: "cb-1".to_string(),
            data: "a1b2c3".to_string()
        }
    );
}

#[test]
fn names_fall_back_to_username_then_default() {
    let items = [
        r#"{"update_id":1,"message":{"message_id":1,"from":{"id":5,"is_bot":false,"username":"sail_user"},"chat":{"id":5,"type":"private"},"date":1,"text":"a"}}"#.to_string(),
        r#"{"update_id":2,"message":{"message_id":2,"from":{"id":6,"is_bot":false},"chat":{"id":6,"type":"private"},"date":2,"text":"b"}}"#.to_string(),
    ];
    let body = updates(&items.join(",")).into_bytes();
    let (list, _) = parse_updates(&body, 0).expect("updates");
    assert_eq!(list[0].sender_name, "@sail_user");
    assert_eq!(list[1].sender_name, "Telegram user");
}

#[test]
fn text_is_cut_at_4096_chars_and_callback_data_at_64_bytes() {
    let long = "é".repeat(5000);
    let body = updates(&format!(
        r#"{{"update_id":1,"message":{{"message_id":1,"from":{{"id":5,"is_bot":false}},"chat":{{"id":5,"type":"private"}},"date":1,"text":"{long}"}}}},{{"update_id":2,"callback_query":{{"id":"cb","from":{{"id":5,"is_bot":false}},"message":{{"message_id":2,"chat":{{"id":5,"type":"private"}},"date":1}},"data":"{}"}}}}"#,
        "x".repeat(80)
    ))
    .into_bytes();
    let (list, _) = parse_updates(&body, 0).expect("updates");
    if let IncomingKind::Text { text } = &list[0].kind {
        assert_eq!(text.chars().count(), 4096);
    } else {
        panic!("text expected");
    }
    if let IncomingKind::Button { data, .. } = &list[1].kind {
        assert_eq!(data.len(), 64);
    } else {
        panic!("button expected");
    }
}

#[test]
fn telegram_errors_and_bad_bodies_are_err() {
    let (_, next) = parse_updates(updates("").as_bytes(), 42).expect("empty");
    assert_eq!(next, 42);
    let error = parse_updates(br#"{"ok":false,"description":"Unauthorized"}"#, 0)
        .expect_err("telegram error");
    assert_eq!(error, "Unauthorized");
    assert!(parse_updates(b"not json", 0).is_err());
    assert!(parse_updates(br#"{"ok":true,"result":{}}"#, 0).is_err());
    assert!(parse_updates(br#"{"result":[]}"#, 0).is_err());
    assert!(parse_updates(&[0xff, 0xfe], 0).is_err());
}

#[test]
fn outgoing_requests_have_telegram_shapes() {
    let (method, body) = request(&Outgoing::Send {
        chat_id: 7,
        text: "hello".to_string(),
        reply_to: Some(3),
        buttons: vec![
            ("Approve".to_string(), "approve:1".to_string()),
            ("Deny".to_string(), "deny:1".to_string()),
        ],
    });
    assert_eq!(method, "sendMessage");
    assert_eq!(body["chat_id"], 7);
    assert_eq!(body["text"], "hello");
    assert_eq!(body["reply_to_message_id"], 3);
    assert_eq!(
        body["reply_markup"]["inline_keyboard"][0][0]["text"],
        "Approve"
    );
    assert_eq!(
        body["reply_markup"]["inline_keyboard"][0][1]["callback_data"],
        "deny:1"
    );

    let (method, body) = request(&Outgoing::Send {
        chat_id: 7,
        text: "plain".to_string(),
        reply_to: None,
        buttons: Vec::new(),
    });
    assert_eq!(method, "sendMessage");
    assert!(body.get("reply_markup").is_none());
    assert!(body.get("reply_to_message_id").is_none());

    assert_eq!(
        request(&Outgoing::Typing { chat_id: 7 }),
        ("sendChatAction", json!({"chat_id": 7, "action": "typing"}))
    );
    assert_eq!(
        request(&Outgoing::AnswerButton {
            callback_id: "cb".to_string(),
            text: "ok".to_string()
        }),
        (
            "answerCallbackQuery",
            json!({"callback_query_id": "cb", "text": "ok"})
        )
    );
    let (method, body) = request(&Outgoing::EditButtons {
        chat_id: 7,
        message_id: 8,
        text: "done".to_string(),
    });
    assert_eq!(method, "editMessageText");
    assert_eq!(body["reply_markup"]["inline_keyboard"], json!([]));
}

#[test]
fn parse_sent_reads_message_id_and_errors() {
    assert_eq!(
        parse_sent(br#"{"ok":true,"result":{"message_id":55}}"#).expect("sent"),
        Some(55)
    );
    assert_eq!(
        parse_sent(br#"{"ok":true,"result":true}"#).expect("sent"),
        None
    );
    assert_eq!(
        parse_sent(br#"{"ok":true,"result":{}}"#).expect("sent"),
        None
    );
    assert_eq!(
        parse_sent(br#"{"ok":false,"description":"Forbidden"}"#).expect_err("error"),
        "Forbidden"
    );
    assert!(parse_sent(b"[]").is_err());
}

#[tokio::test]
async fn fake_bot_records_replays_and_bounds_downloads() {
    let bot = FakeBot::default();
    let reply = json!({"ok":true,"result":{"message_id":12}});
    bot.push_response("sendMessage", reply.clone());
    let body = json!({"chat_id": 1});
    let raw = bot.call("sendMessage", body.clone()).await.expect("call");
    assert_eq!(serde_json::from_slice::<Value>(&raw).unwrap(), reply);
    assert_eq!(bot.calls(), vec![("sendMessage".to_string(), body)]);

    let fallback = bot.call("getMe", json!({})).await.expect("fallback");
    assert_eq!(
        serde_json::from_slice::<Value>(&fallback).unwrap(),
        json!({"ok":true,"result":{}})
    );

    bot.add_file("voice", vec![1, 2, 3]);
    assert_eq!(bot.download("voice", 3).await.expect("file"), vec![1, 2, 3]);
    assert!(bot.download("voice", 2).await.is_err());
    assert!(bot.download("missing", 10).await.is_err());
}

#[test]
fn incoming_round_trips_through_json() {
    let incoming = Incoming {
        update_id: 1,
        chat_id: 2,
        sender_id: 2,
        sender_name: "Ada".to_string(),
        message_id: 3,
        kind: IncomingKind::Text {
            text: "hi".to_string(),
        },
        date: 4,
    };
    let encoded = serde_json::to_string(&incoming).expect("serialize");
    let decoded: Incoming = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded, incoming);
    assert!(encoded.contains("\"type\":\"text\""));
}
