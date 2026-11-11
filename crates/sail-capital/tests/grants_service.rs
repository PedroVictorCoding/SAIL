//! Owner-declared grants: units received without a purchase row (round 9).
use chrono::{Duration, TimeZone, Utc};
use sail_capital::service::{CapitalService, GrantCostInput, GrantInput, ServiceError};

const FILE: &[u8] = include_bytes!("fixtures/xtb/xtb_2026_layout.xlsx");

fn input(symbol: &str, quantity: &str, received_at: &str) -> GrantInput {
    GrantInput {
        symbol: symbol.into(),
        kind: None,
        quantity: quantity.into(),
        received_at: received_at.into(),
        cost: None,
        note: Some("promotional campaign".into()),
    }
}

fn setup() -> (tempfile::TempDir, CapitalService, sail_capital::AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();
    let batch = svc
        .import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap();
    (dir, svc, batch.account)
}

#[test]
fn a_grant_is_recorded_listed_and_retracted_once() {
    let (_dir, svc, account) = setup();
    let now = Utc::now();
    let grant = svc
        .record_grant(&account, input(" galp.pt ", "1", "2024-05-02"), now)
        .unwrap();
    assert_eq!(grant.symbol, "GALP.PT");
    assert_eq!(grant.quantity.to_string(), "1");
    assert_eq!(
        grant.received_at,
        Utc.with_ymd_and_hms(2024, 5, 2, 0, 0, 0).unwrap()
    );
    assert_eq!(grant.cost, None);
    assert_eq!(grant.note.as_deref(), Some("promotional campaign"));

    let listed = svc.grants(&account).unwrap();
    assert_eq!(listed, vec![grant.clone()]);

    let later = now + Duration::seconds(5);
    let retracted = svc.retract_grant(&account, &grant.id, later).unwrap();
    assert_eq!(retracted.retracted_at, Some(later));
    assert!(matches!(
        svc.retract_grant(&account, &grant.id, later),
        Err(ServiceError::BadRequest(_))
    ));
    assert!(matches!(
        svc.retract_grant(&account, "nope", later),
        Err(ServiceError::NotFound(_))
    ));
    // Still listed, with its retraction.
    assert_eq!(svc.grants(&account).unwrap()[0].retracted_at, Some(later));
}

#[test]
fn bad_grants_are_refused() {
    let (_dir, svc, account) = setup();
    let now = Utc::now();
    let cases = [
        input("", "1", "2024-05-02"),
        input("GALP PT", "1", "2024-05-02"),
        input("GALP.PT", "0", "2024-05-02"),
        input("GALP.PT", "-1", "2024-05-02"),
        input("GALP.PT", "1", "02/05/2024"),
        input("GALP.PT", "1", "2999-01-01"),
        GrantInput {
            kind: Some("cfd".into()),
            ..input("GALP.PT", "1", "2024-05-02")
        },
        GrantInput {
            cost: Some(GrantCostInput {
                amount: "-1".into(),
                currency: "EUR".into(),
            }),
            ..input("GALP.PT", "1", "2024-05-02")
        },
        GrantInput {
            cost: Some(GrantCostInput {
                amount: "1".into(),
                currency: "euro".into(),
            }),
            ..input("GALP.PT", "1", "2024-05-02")
        },
    ];
    for case in cases {
        let shown = format!("{case:?}");
        assert!(
            matches!(
                svc.record_grant(&account, case, now),
                Err(ServiceError::BadRequest(_))
            ),
            "{shown}"
        );
    }
    assert!(svc.grants(&account).unwrap().is_empty());
}

#[test]
fn the_store_refuses_edits_and_deletes() {
    let (dir, svc, account) = setup();
    let grant = svc
        .record_grant(&account, input("GALP.PT", "1", "2024-05-02"), Utc::now())
        .unwrap();
    drop(svc);
    let conn = rusqlite::Connection::open(dir.path().join("capital.sqlite3")).unwrap();
    assert!(
        conn.execute(
            "UPDATE grants SET grant_json = '{}' WHERE id = ?1",
            [&grant.id]
        )
        .is_err()
    );
    assert!(
        conn.execute("DELETE FROM grants WHERE id = ?1", [&grant.id])
            .is_err()
    );
}
