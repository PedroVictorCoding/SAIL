//! Mobile sign-in helpers for SSO Moodle sites: build the launch URL, then decode the
//! `signature:::token` redirect the browser lands on.
//!
//! Nothing here touches the network. The owner opens the launch URL in their own browser,
//! signs in through the school's identity provider, and pastes the redirect address back.
//! Checking the signature (`md5(site_root + passport)`) is what stops a forged or stale
//! token paste from being accepted.

use crate::contracts::{MOBILE_SERVICE, MoodleToken, SiteConfig};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use std::fmt::Write as _;

/// A fresh, unguessable passport for one launch attempt. It ties the browser redirect to
/// this process: the server signs the token with the same passport, and a redirect minted
/// for another attempt has a different signature.
///
/// The value is 16 random bytes (a UUID v4) in lowercase hex, so it is safe to put in a URL
/// and easy to compare as text.
pub fn new_passport() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Build the mobile launch URL the owner opens in their browser.
///
/// The `passport` is echoed back inside the signed payload, and `url_scheme` is the address
/// scheme the browser should redirect to (`sail://` in the app, any scheme in tests).
pub fn launch_url(site: &SiteConfig, passport: &str, url_scheme: &str) -> url::Url {
    let mut url = site.base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/admin/tool/mobile/launch.php"));
    url.set_query(None);
    url.set_fragment(None);
    url.query_pairs_mut()
        .append_pair("service", MOBILE_SERVICE)
        .append_pair("passport", passport)
        .append_pair("urlscheme", url_scheme);
    url
}

/// Why a pasted sign-in redirect was rejected.
///
/// The variants never carry the pasted text or the decoded token: the owner's token must not
/// leak through an error message, a log line or a JSON response.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("the sign-in reply is empty")]
    Empty,
    #[error("paste the full address the browser showed after signing in")]
    Unrecognised,
    #[error("the sign-in reply is not valid base64")]
    BadBase64,
    #[error("the sign-in payload is incomplete")]
    BadPayload,
    #[error("the sign-in signature does not match this sign-in attempt")]
    BadSignature,
    #[error("Moodle did not return a valid web-service token")]
    ImplausibleToken,
}

/// Decode and verify the redirect the browser landed on.
///
/// Accepts either `<scheme>://token=<base64>` (the mobile launch reply, with
/// `signature:::token[:::privatetoken]` inside) or a bare 32-character hex token copied from
/// *Preferences → Security keys*. Anything after the base64 (`#...`, `&...`, a trailing `/`)
/// is ignored. The private token, when present, is deliberately dropped: SAIL only needs the
/// web-service token.
pub fn parse_launch_redirect(
    site: &SiteConfig,
    passport: &str,
    pasted: &str,
) -> Result<MoodleToken, AuthError> {
    let pasted = pasted.trim();
    if pasted.is_empty() {
        return Err(AuthError::Empty);
    }

    if is_hex_token(pasted) {
        return Ok(MoodleToken::new(pasted.to_ascii_lowercase()));
    }

    let encoded = extract_payload(pasted).ok_or(AuthError::Unrecognised)?;
    let decoded = decode_base64(encoded).ok_or(AuthError::BadBase64)?;
    let payload = std::str::from_utf8(&decoded).map_err(|_| AuthError::BadPayload)?;

    let mut parts = payload.split(":::");
    let signature = parts.next().unwrap_or_default().trim();
    let token = parts.next().unwrap_or_default().trim();
    if signature.is_empty() || token.is_empty() {
        return Err(AuthError::BadPayload);
    }

    let expected = md5_hex(&format!("{}{}", site_root(site), passport));
    if !signature.eq_ignore_ascii_case(&expected) {
        return Err(AuthError::BadSignature);
    }

    let token = MoodleToken::new(token);
    if !token.is_plausible() {
        return Err(AuthError::ImplausibleToken);
    }
    Ok(token)
}

/// The site root as Moodle signed it: the configured base URL without trailing slashes.
fn site_root(site: &SiteConfig) -> String {
    site.base_url.as_str().trim_end_matches('/').to_string()
}

/// True for a bare Moodle web-service token (32 hex characters, any case).
fn is_hex_token(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Pull the base64 value out of `<scheme>://token=<base64>...`.
///
/// The base64 may contain `/`, so only a trailing slash is trimmed; internal slashes are
/// part of the alphabet.
fn extract_payload(pasted: &str) -> Option<&str> {
    let (_scheme, rest) = pasted.split_once("://")?;
    let start = rest.find("token=")? + "token=".len();
    let rest = &rest[start..];
    let end = rest.find(['#', '&']).unwrap_or(rest.len());
    let value = rest[..end].trim();
    let trimmed = value.trim_end_matches('/');
    if trimmed.is_empty() {
        Some(value)
    } else {
        Some(trimmed)
    }
}

/// Decode standard or URL-safe base64, with or without padding. Browsers and Moodle
/// versions disagree on which alphabet and padding they use, so try all four shapes.
fn decode_base64(value: &str) -> Option<Vec<u8>> {
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(value) {
            return Some(bytes);
        }
    }
    // A `+` in a pasted URL is sometimes turned into a space by the browser.
    if value.contains(' ') {
        return STANDARD.decode(value.replace(' ', "+")).ok();
    }
    None
}

/// Lowercase hex MD5, matching the signature Moodle computes in PHP.
fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let digest = Md5::digest(input.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> SiteConfig {
        SiteConfig {
            base_url: url::Url::parse("https://moodle.example.test/").expect("test URL"),
        }
    }

    fn redirect(passport: &str, token: &str, engine: &dyn Fn(&[u8]) -> String) -> String {
        let signature = md5_hex(&format!("{}{}", site_root(&site()), passport));
        let payload = format!("{signature}:::{token}:::private-token");
        format!("sail://token={}", engine(payload.as_bytes()))
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn good_redirect_is_accepted_and_private_token_dropped() {
        let passport = new_passport();
        let pasted = redirect(&passport, TOKEN, &|bytes| STANDARD.encode(bytes));
        let token = parse_launch_redirect(&site(), &passport, &pasted).expect("valid redirect");
        assert_eq!(token.expose(), TOKEN);
        assert!(!format!("{token:?}").contains(TOKEN));
    }

    #[test]
    fn bad_signature_is_rejected() {
        let passport = new_passport();
        let mut pasted = redirect(&passport, TOKEN, &|bytes| STANDARD.encode(bytes));
        // Flip one character of the encoded payload so the signature no longer matches.
        let marker = "token=";
        let start = pasted.find(marker).expect("token marker") + marker.len();
        let byte = pasted.as_bytes()[start];
        let replacement = if byte == b'A' { 'B' } else { 'A' };
        pasted.replace_range(start..start + 1, &replacement.to_string());
        assert!(matches!(
            parse_launch_redirect(&site(), &passport, &pasted),
            Err(AuthError::BadSignature) | Err(AuthError::BadBase64)
        ));
    }

    #[test]
    fn wrong_passport_is_rejected() {
        let passport = new_passport();
        let pasted = redirect(&passport, TOKEN, &|bytes| STANDARD.encode(bytes));
        let other = new_passport();
        assert_eq!(
            parse_launch_redirect(&site(), &other, &pasted),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn url_safe_base64_is_accepted() {
        let passport = new_passport();
        let pasted = redirect(&passport, TOKEN, &|bytes| URL_SAFE.encode(bytes));
        assert_eq!(
            parse_launch_redirect(&site(), &passport, &pasted)
                .expect("URL-safe redirect")
                .expose(),
            TOKEN
        );
    }

    #[test]
    fn trailing_junk_is_ignored() {
        let passport = new_passport();
        let pasted = format!(
            "{}#section&more",
            redirect(&passport, TOKEN, &|bytes| STANDARD.encode(bytes))
        );
        assert!(parse_launch_redirect(&site(), &passport, &pasted).is_ok());
    }

    #[test]
    fn bare_token_is_accepted() {
        let token = parse_launch_redirect(&site(), "unused", TOKEN).expect("bare token");
        assert_eq!(token.expose(), TOKEN);
    }

    #[test]
    fn garbage_never_panics() {
        let cases = [
            "",
            "   ",
            "not a redirect",
            "sail://",
            "sail://token=",
            "sail://token=%%%",
            "://token=zzzz",
            "sail://token=AAAA",
            "sail://token=MTIz",
        ];
        for case in cases {
            let _ = parse_launch_redirect(&site(), "passport", case);
        }
    }

    #[test]
    fn launch_url_has_the_mobile_parameters() {
        let url = launch_url(&site(), "abc123", "sail");
        assert_eq!(url.path(), "/admin/tool/mobile/launch.php");
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs.get("service").map(String::as_str),
            Some(MOBILE_SERVICE)
        );
        assert_eq!(pairs.get("passport").map(String::as_str), Some("abc123"));
        assert_eq!(pairs.get("urlscheme").map(String::as_str), Some("sail"));
    }

    #[test]
    fn launch_url_keeps_a_subdirectory_install() {
        let site = SiteConfig {
            base_url: url::Url::parse("https://example.test/moodle").expect("test URL"),
        };
        let url = launch_url(&site, "abc123", "sail");
        assert_eq!(url.path(), "/moodle/admin/tool/mobile/launch.php");
    }

    #[test]
    fn passport_is_hex_and_long_enough() {
        let passport = new_passport();
        assert_eq!(passport.len(), 32);
        assert!(passport.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(passport, new_passport());
    }

    #[test]
    fn signature_uses_the_site_root_without_a_trailing_slash() {
        let site = SiteConfig {
            base_url: url::Url::parse("https://example.test/moodle/").expect("test URL"),
        };
        let passport = "passport";
        let signature = md5_hex("https://example.test/moodlepassport");
        let payload = format!("{signature}:::{TOKEN}");
        let encoded = STANDARD.encode(payload.as_bytes());
        let pasted = format!("sail://token={encoded}");
        assert!(parse_launch_redirect(&site, passport, &pasted).is_ok());
    }
}
