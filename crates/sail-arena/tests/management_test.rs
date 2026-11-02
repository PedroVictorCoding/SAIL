use sail_arena::{MemoryStore, knowledge::*};
fn entity(tx: &mut KnowledgeTx<'_>, name: &str) -> KnowledgeResult<i64> {
    tx.create_entity(&NewEntity {
        kind: EntityKind::Person,
        name,
        aliases: &[],
        scope: "voice",
        sensitivity: Sensitivity::Private,
    })
}
#[test]
fn merge_can_be_separated_only_before_intervening_mutations() {
    let s = MemoryStore::open_in_memory().unwrap();
    let k = s.knowledge();
    let (a, b) = k
        .write(Actor::User, |tx| Ok((entity(tx, "A")?, entity(tx, "B")?)))
        .unwrap();
    k.write(Actor::User, |tx| tx.merge_entities(a, b, "same person"))
        .unwrap();
    k.write(Actor::User, |tx| tx.separate_entity(a, "different people"))
        .unwrap();
    assert!(
        k.entity(a, &Access::owner())
            .unwrap()
            .unwrap()
            .merged_into
            .is_none()
    );
    assert!(
        k.find_entities("A", None, &Access::owner())
            .unwrap()
            .iter()
            .all(|e| e.id == a)
    );
    k.write(Actor::User, |tx| tx.merge_entities(a, b, "merge"))
        .unwrap();
    k.write(Actor::User, |tx| tx.add_alias(b, "Later alias"))
        .unwrap();
    assert!(
        k.write(Actor::User, |tx| tx.separate_entity(a, "undo"))
            .is_err()
    );
    assert_eq!(
        k.find_entities("Later alias", None, &Access::owner())
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn privacy_invalidates_work_and_pin_and_export_are_visible_to_owner() {
    let s = MemoryStore::open_in_memory().unwrap();
    let k = s.knowledge();
    let id = k.write(Actor::User, |tx| entity(tx, "Person")).unwrap();
    k.write(Actor::User, |tx| tx.pin_entity(id, true)).unwrap();
    assert!(k.entities(&Access::owner(), 10).unwrap()[0].1);
    k.write(Actor::User, |tx| tx.mark_private(id)).unwrap();
    assert_eq!(k.deletion_generation().unwrap(), 1);
    assert!(
        k.entity(id, &Access::scoped(&["voice"], Sensitivity::Normal))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        k.export(&Access::owner()).unwrap()["entities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    k.write(Actor::User, |tx| tx.delete_entity(id, "forget"))
        .unwrap();
    assert!(
        k.export(&Access::owner()).unwrap()["entities"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
