//! Site grants: the network scope of one browser profile.
//!
//! Every request the browser makes, including subresources, popups and workers, is paused
//! and checked here before it leaves the machine. Anything outside the grant fails with
//! `BlockedByClient`. Top-level navigations get a second, narrower check (`deny_documents`),
//! so an authenticated profile can load what a site needs without being steered to other
//! pages on the same account.

use url::{Host, Url};

/// One host rule. `Suffix("google.com")` matches `google.com` and `*.google.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRule {
    Exact(String),
    Suffix(String),
}

impl HostRule {
    fn matches(&self, host: &str) -> bool {
        match self {
            HostRule::Exact(rule) => host == rule,
            HostRule::Suffix(rule) => {
                host == rule
                    || host
                        .strip_suffix(rule.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SiteGrant {
    /// Shown to the user and in logs, e.g. `aistudio`.
    pub name: String,
    /// Where the session starts and where `navigate` may go.
    pub home: Url,
    /// Hosts any request may reach.
    pub allow: Vec<HostRule>,
    /// Hosts that may still serve subresources but may not be opened as a page.
    pub deny_documents: Vec<HostRule>,
    /// Allow plain `http://` to loopback. Only for local test fixtures.
    pub allow_loopback_http: bool,
}

/// What a request is, as far as the grant cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Document,
    Subresource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
}

impl SiteGrant {
    pub fn check(&self, raw_url: &str, kind: RequestKind) -> Verdict {
        let url = match Url::parse(raw_url) {
            Ok(url) => url,
            Err(error) => return Verdict::Deny(format!("unparseable URL: {error}")),
        };
        match url.scheme() {
            // Local to the renderer; no network.
            "data" | "blob" | "about" => return Verdict::Allow,
            "https" => {}
            "http" | "ws" if self.allow_loopback_http && is_loopback(&url) => {
                return Verdict::Allow;
            }
            "wss" => {}
            other => return Verdict::Deny(format!("scheme {other} is outside the grant")),
        }
        let Some(Host::Domain(host)) = url.host() else {
            // IP literals (including private and metadata ranges) are never part of a site.
            return Verdict::Deny(format!("{raw_url} is not a named host"));
        };
        let host = host.to_ascii_lowercase();
        if !self.allow.iter().any(|rule| rule.matches(&host)) {
            return Verdict::Deny(format!("{host} is outside the {} grant", self.name));
        }
        if kind == RequestKind::Document && self.deny_documents.iter().any(|r| r.matches(&host)) {
            return Verdict::Deny(format!(
                "{host} may not be opened as a page in the {} grant",
                self.name
            ));
        }
        Verdict::Allow
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(d)) => d == "localhost",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> SiteGrant {
        SiteGrant {
            name: "test".into(),
            home: Url::parse("https://app.example.com/").unwrap(),
            allow: vec![
                HostRule::Suffix("example.com".into()),
                HostRule::Exact("cdn.other.net".into()),
            ],
            deny_documents: vec![HostRule::Exact("mail.example.com".into())],
            allow_loopback_http: false,
        }
    }

    #[test]
    fn suffix_rules_match_subdomains_but_not_lookalikes() {
        let g = grant();
        assert_eq!(
            g.check("https://app.example.com/x", RequestKind::Document),
            Verdict::Allow
        );
        assert_eq!(
            g.check("https://example.com/", RequestKind::Document),
            Verdict::Allow
        );
        assert!(matches!(
            g.check("https://evilexample.com/", RequestKind::Subresource),
            Verdict::Deny(_)
        ));
        assert!(matches!(
            g.check("https://example.com.evil.io/", RequestKind::Subresource),
            Verdict::Deny(_)
        ));
        assert!(matches!(
            g.check("https://x.cdn.other.net/", RequestKind::Subresource),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn denied_documents_still_serve_subresources() {
        let g = grant();
        assert!(matches!(
            g.check("https://mail.example.com/", RequestKind::Document),
            Verdict::Deny(_)
        ));
        assert_eq!(
            g.check("https://mail.example.com/api", RequestKind::Subresource),
            Verdict::Allow
        );
    }

    #[test]
    fn plain_http_ip_literals_and_other_schemes_are_denied() {
        let mut g = grant();
        for url in [
            "http://app.example.com/",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/",
            "file:///etc/passwd",
            "chrome://settings",
            "http://127.0.0.1:8080/",
        ] {
            assert!(
                matches!(g.check(url, RequestKind::Document), Verdict::Deny(_)),
                "{url} should be denied"
            );
        }
        assert_eq!(
            g.check("data:text/html,hi", RequestKind::Document),
            Verdict::Allow
        );
        g.allow_loopback_http = true;
        assert_eq!(
            g.check("http://127.0.0.1:8080/", RequestKind::Document),
            Verdict::Allow
        );
        assert!(matches!(
            g.check("http://10.0.0.1/", RequestKind::Document),
            Verdict::Deny(_)
        ));
    }
}
