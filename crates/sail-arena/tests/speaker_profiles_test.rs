use sail_arena::MemoryStore;
#[test]
fn deleting_a_profile_invalidates_pending_enrollment_and_never_reuses_request_ids() {
    let s = MemoryStore::open_in_memory().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let (generation, _) = s.speaker_profiles().unwrap();
    s.enroll_speaker(&id, b"opaque ciphertext", generation)
        .unwrap();
    assert!(
        s.enroll_speaker(&id, b"other ciphertext", generation)
            .is_err()
    );
    assert!(s.forget_speaker(&id).unwrap());
    assert!(
        s.enroll_speaker(
            &uuid::Uuid::new_v4().to_string(),
            b"late enrollment",
            generation
        )
        .is_err()
    );
    assert!(s.speaker_profiles().unwrap().1.is_empty());
    assert!(s.check_integrity().unwrap().is_empty());
}
