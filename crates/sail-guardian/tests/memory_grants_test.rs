use sail_common::types::SubsystemId;
use sail_guardian::memory::*;
use uuid::Uuid;
fn policy() -> MemoryPolicy {
    MemoryPolicy {
        scopes: vec!["voice".into()],
        max_sensitivity: 1,
        observe: false,
        maintain: false,
        approved_only: false,
    }
}
#[test]
fn issuer_task_scope_expiry_revision_and_operations_are_bound() {
    let authority = MemoryAuthority::new();
    authority
        .configure(SubsystemId::Dialogue, policy())
        .unwrap();
    let task = Uuid::new_v4();
    let scopes = vec!["voice".into()];
    let grant = authority
        .issue(SubsystemId::Dialogue, task, &scopes, 100, 100)
        .unwrap();
    assert!(
        authority
            .validate(&grant, SubsystemId::Dialogue, task, MemoryUse::Read, 101)
            .is_ok()
    );
    assert!(
        authority
            .validate(
                &grant,
                SubsystemId::Dialogue,
                Uuid::new_v4(),
                MemoryUse::Read,
                101
            )
            .is_err()
    );
    assert!(
        authority
            .validate(&grant, SubsystemId::Agents, task, MemoryUse::Read, 101)
            .is_err()
    );
    assert!(
        authority
            .validate(&grant, SubsystemId::Dialogue, task, MemoryUse::Observe, 101)
            .is_err()
    );
    assert!(
        authority
            .validate(&grant, SubsystemId::Dialogue, task, MemoryUse::Read, 200)
            .is_err()
    );
    let other = MemoryAuthority::new();
    other.configure(SubsystemId::Dialogue, policy()).unwrap();
    assert!(
        other
            .validate(&grant, SubsystemId::Dialogue, task, MemoryUse::Read, 101)
            .is_err()
    );
    assert!(
        authority
            .issue(
                SubsystemId::Dialogue,
                task,
                &["project:secret".into()],
                100,
                100
            )
            .is_err()
    );
    authority
        .configure(SubsystemId::Dialogue, policy())
        .unwrap();
    assert!(
        authority
            .validate(&grant, SubsystemId::Dialogue, task, MemoryUse::Read, 101)
            .is_err()
    );
}
