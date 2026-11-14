//! Discovery over a real UDP socket on loopback: announcements are heard, deduplicated and
//! sorted, and a quiet network is not an error.
use sail_mesh::{
    MESH_VERSION,
    discovery::{Announcement, announce, decode, encode, listen},
};
use std::{net::SocketAddr, time::Duration};

fn free_udp_address() -> SocketAddr {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("bind free UDP port")
        .local_addr()
        .expect("free UDP address")
}

fn announcement(worker_id: &str, name: &str) -> Announcement {
    Announcement {
        version: MESH_VERSION,
        worker_id: worker_id.into(),
        name: name.into(),
        port: 7710,
        fingerprint: "ab".repeat(32),
    }
}

fn start_announcing(
    announcement: Announcement,
    target: SocketAddr,
    every: Duration,
) -> tokio::task::JoinHandle<()> {
    // The stop future is a long sleep; the task is aborted once the listener returned.
    tokio::spawn(announce(
        announcement,
        target,
        every,
        tokio::time::sleep(Duration::from_secs(60)),
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_is_heard_once_despite_repeated_announcements() {
    let bind = free_udp_address();
    let listening = tokio::spawn(listen(bind, Duration::from_millis(1200)));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let sender = start_announcing(
        announcement("0b7c6c1e-0000-4000-8000-000000000001", "desktop"),
        bind,
        Duration::from_millis(30),
    );

    let found = listening
        .await
        .expect("listen task")
        .expect("listen result");
    sender.abort();
    let _ = sender.await;

    assert_eq!(
        found.len(),
        1,
        "a repeated announcement is one worker: {found:?}"
    );
    assert_eq!(found[0].announcement.name, "desktop");
    assert_eq!(
        found[0].address,
        format!("127.0.0.1:{}", found[0].announcement.port)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workers_are_sorted_by_name_and_then_address() {
    let bind = free_udp_address();
    let listening = tokio::spawn(listen(bind, Duration::from_millis(1200)));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let beta = start_announcing(
        announcement("0b7c6c1e-0000-4000-8000-000000000002", "beta"),
        bind,
        Duration::from_millis(30),
    );
    let alpha = start_announcing(
        announcement("0b7c6c1e-0000-4000-8000-000000000003", "alpha"),
        bind,
        Duration::from_millis(30),
    );

    let found = listening
        .await
        .expect("listen task")
        .expect("listen result");
    beta.abort();
    alpha.abort();
    let _ = beta.await;
    let _ = alpha.await;

    let names: Vec<&str> = found
        .iter()
        .map(|worker| worker.announcement.name.as_str())
        .collect();
    assert_eq!(names, vec!["alpha", "beta"], "{found:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_network_returns_an_empty_list() {
    let found = listen(free_udp_address(), Duration::from_millis(150))
        .await
        .expect("a quiet network is not an error");
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn announcements_round_trip_and_encode_is_pure() {
    let announcement = announcement("0b7c6c1e-0000-4000-8000-000000000004", "desktop");
    let first = encode(&announcement).expect("encode");
    let second = encode(&announcement).expect("encode again");
    assert_eq!(first, second, "encode has no clock, randomness or I/O");
    assert_eq!(decode(&first), Some(announcement), "decode twice agrees");
    assert_eq!(decode(&first), decode(&first));
}
