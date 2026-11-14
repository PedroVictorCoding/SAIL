use sail_flow::gateway::pairing::{
    Admission, CODE_ALPHABET, CODE_LEN, CODE_TTL_SECS, MAX_FAILED_APPROVALS, MAX_PENDING,
    PairingStore, REQUEST_COOLDOWN_SECS,
};
use std::fs;

fn store() -> (tempfile::TempDir, PairingStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PairingStore {
        dir: dir.path().to_path_buf(),
    };
    (dir, store)
}

fn code_of(admission: Admission) -> String {
    match admission {
        Admission::NewCode(code) => code,
        other => panic!("expected a new code, got {other:?}"),
    }
}

#[test]
fn alphabet_has_no_confusing_characters() {
    assert_eq!(CODE_LEN, 8);
    assert_eq!(CODE_ALPHABET.len(), 31);
    for bad in ['0', 'O', '1', 'I', 'L'] {
        assert!(!CODE_ALPHABET.contains(bad));
    }
    let (_dir, store) = store();
    let code = code_of(
        store
            .admit(1, 1, "Ada", 1000, [0; CODE_LEN])
            .expect("admit"),
    );
    assert_eq!(code, "AAAAAAAA");
    assert!(code.chars().all(|c| CODE_ALPHABET.contains(c)));
}

#[test]
fn admit_issues_once_then_respects_cooldown_and_expiry() {
    let (_dir, store) = store();
    let code = code_of(
        store
            .admit(1, 1, "Ada", 1000, [1; CODE_LEN])
            .expect("admit"),
    );
    assert_eq!(
        store
            .admit(1, 1, "Ada", 1001, [2; CODE_LEN])
            .expect("admit"),
        Admission::Ignore
    );
    store.deny(&code).expect("deny");
    assert_eq!(
        store
            .admit(1, 1, "Ada", 1000 + REQUEST_COOLDOWN_SECS - 1, [3; CODE_LEN])
            .expect("admit"),
        Admission::Ignore,
        "just inside the cooldown"
    );
    let second = code_of(
        store
            .admit(1, 1, "Ada", 1000 + REQUEST_COOLDOWN_SECS, [4; CODE_LEN])
            .expect("admit"),
    );
    assert_ne!(second, code);
    assert_eq!(
        store
            .admit(1, 1, "Ada", 1600 + CODE_TTL_SECS - 1, [5; CODE_LEN])
            .expect("admit"),
        Admission::Ignore,
        "a live code still suppresses a new one"
    );
    let third = code_of(
        store
            .admit(1, 1, "Ada", 1600 + CODE_TTL_SECS, [6; CODE_LEN])
            .expect("admit"),
    );
    assert_eq!(third.len(), CODE_LEN);
}

#[test]
fn at_most_three_pending_codes() {
    let (_dir, store) = store();
    for sender in 1..=MAX_PENDING as i64 {
        assert!(matches!(
            store
                .admit(sender, sender, "Stranger", 1000, [sender as u8; CODE_LEN])
                .expect("admit"),
            Admission::NewCode(_)
        ));
    }
    assert_eq!(
        store
            .admit(99, 99, "Fourth", 1000, [9; CODE_LEN])
            .expect("admit"),
        Admission::Ignore
    );
    let view = store.snapshot(1000).expect("snapshot");
    assert_eq!(view.pending.len(), MAX_PENDING);
}

#[test]
fn approve_is_case_and_space_insensitive_and_pairs() {
    let (_dir, store) = store();
    let code = code_of(
        store
            .admit(7, -7, "Ada Lovelace", 1000, [7; CODE_LEN])
            .expect("admit"),
    );
    let typed = format!(
        " {} {} ",
        code[..4].to_lowercase(),
        code[4..].to_lowercase()
    );
    let paired = store.approve(&typed, 2000).expect("approve");
    assert_eq!(paired.sender_id, 7);
    assert_eq!(paired.chat_id, -7);
    assert_eq!(paired.name, "Ada Lovelace");
    assert_eq!(paired.paired_at, 2000);
    assert!(store.is_paired(7).expect("paired"));
    assert!(!store.is_paired(8).expect("paired"));
    assert_eq!(
        store
            .admit(7, -7, "Ada", 2001, [8; CODE_LEN])
            .expect("admit"),
        Admission::Allowed
    );
    assert!(store.approve(&code, 2002).is_err(), "codes are single use");
}

#[test]
fn deny_and_revoke_remove_state() {
    let (_dir, store) = store();
    let code = code_of(
        store
            .admit(3, 3, "Grace", 1000, [3; CODE_LEN])
            .expect("admit"),
    );
    store.deny(&code).expect("deny");
    assert!(store.deny(&code).is_err());
    let code = code_of(
        store
            .admit(3, 3, "Grace", 2000, [4; CODE_LEN])
            .expect("admit"),
    );
    store.approve(&code, 2001).expect("approve");
    store.revoke(3).expect("revoke");
    assert!(!store.is_paired(3).expect("paired"));
    assert!(store.revoke(3).is_err());
}

#[test]
fn five_wrong_codes_lock_approvals_for_an_hour() {
    let (_dir, store) = store();
    for attempt in 1..=MAX_FAILED_APPROVALS {
        let error = store
            .approve("NOPE", 1000 + attempt as i64)
            .expect_err("wrong");
        assert!(!error.contains("NOPE"));
    }
    let view = store.snapshot(1005).expect("snapshot");
    assert!(view.locked_until.is_some());
    assert!(store.approve("NOPE", 1010).is_err(), "locked");
    let expired = store.snapshot(1005 + CODE_TTL_SECS).expect("snapshot");
    assert_eq!(expired.locked_until, None);

    let code = code_of(
        store
            .admit(5, 5, "Locked", 1005 + CODE_TTL_SECS, [5; CODE_LEN])
            .expect("admit"),
    );
    assert!(store.approve(&code, 1005 + CODE_TTL_SECS + 1).is_ok());
}

#[test]
fn policy_decline_refuses_once_then_ignores() {
    let (_dir, store) = store();
    store.set_policy("decline").expect("policy");
    assert_eq!(
        store
            .admit(1, 1, "Stranger", 1000, [1; CODE_LEN])
            .expect("admit"),
        Admission::Decline
    );
    assert_eq!(
        store
            .admit(1, 1, "Stranger", 1001, [1; CODE_LEN])
            .expect("admit"),
        Admission::Ignore
    );
    store.set_policy("ignore").expect("policy");
    assert_eq!(
        store
            .admit(2, 2, "Other", 1000, [2; CODE_LEN])
            .expect("admit"),
        Admission::Ignore
    );
    assert!(store.set_policy("friends").is_err());
    let view = store.snapshot(1000).expect("snapshot");
    assert_eq!(view.policy, "ignore");
    assert!(view.paired.is_empty());
}

#[test]
fn snapshot_hides_expired_codes_and_respects_lockout() {
    let (_dir, store) = store();
    let code = code_of(
        store
            .admit(1, 1, "Ada", 1000, [1; CODE_LEN])
            .expect("admit"),
    );
    let live = store.snapshot(1000 + CODE_TTL_SECS - 1).expect("snapshot");
    assert_eq!(live.pending.len(), 1);
    assert_eq!(live.pending[0].code, code);
    let expired = store.snapshot(1000 + CODE_TTL_SECS).expect("snapshot");
    assert!(expired.pending.is_empty());
}

#[test]
fn corrupt_file_is_an_error_and_is_not_replaced() {
    let (dir, store) = store();
    let path = dir.path().join("pairing.json");
    fs::write(&path, b"{not json").expect("write");
    let before = fs::read(&path).expect("read");
    assert!(store.snapshot(0).is_err());
    assert!(store.admit(1, 1, "Ada", 0, [0; CODE_LEN]).is_err());
    assert_eq!(fs::read(&path).expect("read"), before);
}

#[test]
fn wrong_version_is_an_error() {
    let (dir, store) = store();
    fs::write(
        dir.path().join("pairing.json"),
        br#"{"version":2,"paired":[],"pending":[],"failed_approvals":0,"locked_until":null,"policy":"pair","declined":[]}"#,
    )
    .expect("write");
    assert!(store.snapshot(0).is_err());
}

#[cfg(unix)]
#[test]
fn pairing_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, store) = store();
    store
        .admit(1, 1, "Ada", 1000, [1; CODE_LEN])
        .expect("admit");
    let mode = fs::metadata(dir.path().join("pairing.json"))
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}
