//! K4: URL normalization, hints and the SSRF address guard (no DNS/network).

use std::net::IpAddr;

use sail_calendar::contracts::CalendarError;
use sail_calendar::fetch::{check_addr, normalize_feed_url, url_hint};

#[test]
fn only_https_and_webcal_are_accepted() {
    assert_eq!(
        normalize_feed_url("webcal://calendar.example.test/private")
            .expect("webcal")
            .as_str(),
        "https://calendar.example.test/private"
    );
    for raw in [
        "http://calendar.example.test/x",
        "ftp://calendar.example.test/x",
        "calendar.example.test/x",
        "",
        "https://user:pass@calendar.example.test/x",
    ] {
        assert!(normalize_feed_url(raw).is_err(), "{raw}");
    }
}

#[test]
fn hints_hide_secret_paths_and_queries() {
    let url =
        normalize_feed_url("https://calendar.google.com/secret/address?token=1").expect("url");
    assert_eq!(url_hint(&url), "https://calendar.google.com/…");
}

#[test]
fn every_non_public_address_class_is_refused() {
    for raw in [
        "127.0.0.1",
        "10.0.0.1",
        "172.31.0.1",
        "192.168.0.1",
        "169.254.169.254",
        "100.64.0.1",
        "100.127.255.255",
        "0.0.0.0",
        "255.255.255.255",
        "224.0.0.1",
        "::1",
        "fc00::1",
        "fd12::1",
        "fe80::1",
        "ff02::1",
        "::",
        "::ffff:192.168.1.2",
    ] {
        let ip: IpAddr = raw.parse().expect("ip");
        assert_eq!(check_addr(ip), Err(CalendarError::BlockedAddress), "{raw}");
    }
    for raw in ["8.8.8.8", "1.1.1.1", "2001:4860:4860::8888"] {
        let ip: IpAddr = raw.parse().expect("ip");
        assert_eq!(check_addr(ip), Ok(()), "{raw}");
    }
}

#[test]
fn hints_are_deterministic() {
    let url = normalize_feed_url("https://example.test/a").expect("url");
    assert_eq!(url_hint(&url), url_hint(&url));
}
