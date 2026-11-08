//! HTTPS client for Moodle's REST web service and pluginfile downloads.
//!
//! The client is deliberately narrow: it only knows the read-only functions the connector
//! needs, sends the token in a form body (never a URL that might be logged), refuses any
//! host other than the configured site, and streams downloads behind a hard byte cap. Every
//! error string is scrubbed of the token before it leaves this module.

use crate::client::parse;
use crate::contracts::{
    Announcement, Assignment, CalendarEvent, Course, DownloadedFile, FileRef, GradeItem,
    MAX_FILE_BYTES, MAX_RESPONSE_BYTES, MoodleApi, MoodleError, MoodleToken, Section, SiteConfig,
    SiteInfo, SubmissionStatus,
};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::header::CONTENT_TYPE;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

/// Sent with every request so a Moodle administrator can identify SAIL in their logs.
pub const USER_AGENT: &str = "SAIL-Moodle/0.1";

/// Most pages of news-forum discussions read per forum. Announcements are a convenience,
/// not an archive; the three newest pages are enough to notice what is new.
pub const MAX_ANNOUNCEMENT_PAGES: usize = 3;

/// Discussions requested per page. Moodle's own default is 25; keeping it small bounds the
/// work and makes the `since` cut-off effective before the page limit.
pub const ANNOUNCEMENTS_PER_PAGE: usize = 25;

/// How much larger than the snapshot's recorded size a download may be. Moodle updates the
/// size when a file changes, so a small overrun is harmless, but a large one means the URL
/// no longer points at the file the snapshot described.
pub const MAX_DOWNLOAD_OVERSHOOT_BYTES: u64 = 1024 * 1024;

/// Error codes Moodle uses when the token is missing, expired, suspended or not allowed to
/// use web services. They all require the owner to sign in again, so they map to
/// [`MoodleError::Unauthorized`] and are never retried.
const UNAUTHORIZED_CODES: &[&str] = &[
    "invalidtoken",
    "accessexception",
    "invalidlogin",
    "tokenexpired",
    "tokensuspended",
    "tokenmissing",
];

/// One Moodle site, one token. Cheap to clone the configuration but not the token, so callers
/// keep one client per sync rather than a global.
pub struct RestClient {
    site: SiteConfig,
    token: MoodleToken,
    http: reqwest::Client,
}

impl RestClient {
    /// Build a client for a site. HTTPS is required; plain HTTP is accepted only for the
    /// loopback addresses the integration tests bind, so a misconfigured site can never send
    /// the token in clear text to the network.
    pub fn new(site: SiteConfig, token: MoodleToken) -> Result<Self, MoodleError> {
        require_secure_site(&site)?;
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                MoodleError::Unavailable(format!("could not build the HTTP client: {error}"))
            })?;
        Ok(Self { site, token, http })
    }

    /// Make one web-service call and return its JSON body.
    ///
    /// Moodle reports application errors with HTTP 200, so the body is inspected before the
    /// status: a token problem can never be mistaken for a successful empty reply.
    async fn call(
        &self,
        function: &str,
        params: &[(String, String)],
    ) -> Result<serde_json::Value, MoodleError> {
        let url = self.rest_url(function)?;
        let mut form = Vec::with_capacity(params.len() + 1);
        form.push(("wstoken".to_string(), self.token.expose().to_string()));
        form.extend(params.iter().cloned());

        let response = self
            .http
            .post(url)
            .form(&form)
            .send()
            .await
            .map_err(|error| self.network_error(error))?;
        let status = response.status();
        if status.is_server_error() {
            return Err(MoodleError::Unavailable(format!(
                "Moodle answered HTTP {status}"
            )));
        }
        if status.is_redirection() {
            return Err(MoodleError::BadResponse(
                "Moodle redirected a web-service call".to_string(),
            ));
        }
        let bytes = self.read_capped(response).await?;
        let value = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|error| {
                MoodleError::BadResponse(format!("unreadable JSON from {function}: {error}"))
            })?
        };

        if let Some(error) = self.error_from_value(&value, function) {
            return Err(error);
        }
        if !status.is_success() {
            return Err(MoodleError::BadResponse(format!(
                "Moodle answered HTTP {status}"
            )));
        }
        Ok(value)
    }

    fn rest_url(&self, function: &str) -> Result<url::Url, MoodleError> {
        let mut url = self.site.base_url.clone();
        let base_path = url.path().trim_end_matches('/');
        url.set_path(&format!("{base_path}/webservice/rest/server.php"));
        url.set_fragment(None);
        url.set_query(None);
        url.query_pairs_mut()
            .append_pair("moodlewsrestformat", "json")
            .append_pair("wsfunction", function);
        Ok(url)
    }

    /// Read a reply without ever buffering more than the contract's cap.
    async fn read_capped(&self, response: reqwest::Response) -> Result<Vec<u8>, MoodleError> {
        if response
            .content_length()
            .is_some_and(|length| length as usize > MAX_RESPONSE_BYTES)
        {
            return Err(MoodleError::BadResponse(format!(
                "reply exceeds {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| self.network_error(error))?;
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(MoodleError::BadResponse(format!(
                    "reply exceeds {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Map Moodle's `{"exception","errorcode","message"}` shape to a typed error.
    fn error_from_value(&self, value: &serde_json::Value, function: &str) -> Option<MoodleError> {
        let object = value.as_object()?;
        let code = object
            .get("errorcode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let exception = object
            .get("exception")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if code.is_empty() && exception.is_empty() {
            return None;
        }
        let message = object
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(exception);
        let lowered = code.to_ascii_lowercase();
        if UNAUTHORIZED_CODES.contains(&lowered.as_str()) {
            return Some(MoodleError::Unauthorized(self.scrub(message)));
        }
        if lowered == "servicenotavailable"
            || exception
                .to_ascii_lowercase()
                .contains("access control exception")
        {
            return Some(MoodleError::FunctionNotAllowed {
                function: function.to_string(),
            });
        }
        let code = if code.is_empty() {
            "unknown".to_string()
        } else {
            self.scrub(code)
        };
        Some(MoodleError::Remote {
            code,
            message: self.scrub(message),
        })
    }

    fn network_error(&self, error: reqwest::Error) -> MoodleError {
        MoodleError::Unavailable(self.scrub(&error.without_url().to_string()))
    }

    /// Remove the token from any text that might escape. `reqwest` errors can echo request
    /// URLs, and Moodle's own messages occasionally repeat request parameters.
    fn scrub(&self, text: &str) -> String {
        let token = self.token.expose();
        if token.is_empty() {
            text.to_string()
        } else {
            text.replace(token, "<token>")
        }
    }

    /// Rewrite a pluginfile URL to the web-service endpoint on the configured host, dropping
    /// any token already present. The caller adds the token in memory for the request only.
    fn download_url(&self, file: &FileRef) -> Result<url::Url, MoodleError> {
        let mut url = url::Url::parse(&file.url)
            .map_err(|_| MoodleError::FileRefused("the file URL is not valid".to_string()))?;
        let base = &self.site.base_url;
        if url.scheme() != base.scheme()
            || url.host_str() != base.host_str()
            || url.port_or_known_default() != base.port_or_known_default()
        {
            return Err(MoodleError::FileRefused(
                "the file URL is on another host".to_string(),
            ));
        }
        let path = url
            .path()
            .replacen("/pluginfile.php/", "/webservice/pluginfile.php/", 1);
        url.set_path(&path);
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "token")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.set_query(None);
        if !kept.is_empty() {
            let mut query = url.query_pairs_mut();
            for (key, value) in kept {
                query.append_pair(&key, &value);
            }
        }
        Ok(url)
    }
}

fn require_secure_site(site: &SiteConfig) -> Result<(), MoodleError> {
    match site.base_url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&site.base_url) => Ok(()),
        _ => Err(MoodleError::BadResponse(
            "the Moodle site must use https (http is only allowed for loopback tests)".to_string(),
        )),
    }
}

fn is_loopback(url: &url::Url) -> bool {
    matches!(
        url.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
    )
}

fn form_params(pairs: &[(&str, String)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

#[async_trait::async_trait]
impl MoodleApi for RestClient {
    async fn site_info(&self) -> Result<SiteInfo, MoodleError> {
        let value = self.call("core_webservice_get_site_info", &[]).await?;
        parse::site_info(&value)
    }

    async fn courses(&self, user_id: i64) -> Result<Vec<Course>, MoodleError> {
        let params = form_params(&[("userid", user_id.to_string())]);
        let value = self.call("core_enrol_get_users_courses", &params).await?;
        parse::courses(&value)
    }

    async fn course_contents(&self, course_id: i64) -> Result<Vec<Section>, MoodleError> {
        let params = form_params(&[("courseid", course_id.to_string())]);
        let value = self.call("core_course_get_contents", &params).await?;
        parse::course_contents(course_id, &value)
    }

    async fn assignments(&self, course_ids: &[i64]) -> Result<Vec<Assignment>, MoodleError> {
        if course_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut params = Vec::with_capacity(course_ids.len());
        for (index, course_id) in course_ids.iter().enumerate() {
            params.push((format!("courseids[{index}]"), course_id.to_string()));
        }
        let value = self.call("mod_assign_get_assignments", &params).await?;
        parse::assignments(&value)
    }

    async fn submission_status(&self, assignment_id: i64) -> Result<SubmissionStatus, MoodleError> {
        let params = form_params(&[("assignid", assignment_id.to_string())]);
        let value = self
            .call("mod_assign_get_submission_status", &params)
            .await?;
        parse::submission_status(assignment_id, &value)
    }

    async fn events(
        &self,
        course_ids: &[i64],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>, MoodleError> {
        if course_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut params = Vec::with_capacity(course_ids.len() + 4);
        for (index, course_id) in course_ids.iter().enumerate() {
            params.push((format!("events[courseids][{index}]"), course_id.to_string()));
        }
        params.push((
            "options[timestart]".to_string(),
            from.timestamp().to_string(),
        ));
        params.push(("options[timeend]".to_string(), to.timestamp().to_string()));
        params.push(("options[userevents]".to_string(), "1".to_string()));
        params.push(("options[siteevents]".to_string(), "0".to_string()));
        let value = self
            .call("core_calendar_get_calendar_events", &params)
            .await?;
        parse::events(&value)
    }

    async fn announcements(
        &self,
        course_ids: &[i64],
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Announcement>, MoodleError> {
        if course_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut params = Vec::with_capacity(course_ids.len());
        for (index, course_id) in course_ids.iter().enumerate() {
            params.push((format!("courseids[{index}]"), course_id.to_string()));
        }
        let value = self
            .call("mod_forum_get_forums_by_courses", &params)
            .await?;
        let forums = parse::forums(&value)?;

        let mut announcements = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for forum in forums {
            for page in 0..MAX_ANNOUNCEMENT_PAGES {
                let params = form_params(&[
                    ("forumid", forum.id.to_string()),
                    ("sortorder", "-1".to_string()),
                    ("page", page.to_string()),
                    ("perpage", ANNOUNCEMENTS_PER_PAGE.to_string()),
                ]);
                let value = self
                    .call("mod_forum_get_forum_discussions", &params)
                    .await?;
                let discussions = parse::discussions(forum.course_id, forum.id, &value)?;
                let count = discussions.len();
                let mut reached_cutoff = false;
                for discussion in discussions.into_iter().take(ANNOUNCEMENTS_PER_PAGE) {
                    if since.is_some_and(|cutoff| discussion.modified <= cutoff) {
                        reached_cutoff = true;
                        break;
                    }
                    if seen.insert(discussion.discussion_id) {
                        announcements.push(discussion);
                    }
                }
                if reached_cutoff || count < ANNOUNCEMENTS_PER_PAGE {
                    break;
                }
            }
        }
        Ok(announcements)
    }

    async fn grades(&self, course_id: i64, user_id: i64) -> Result<Vec<GradeItem>, MoodleError> {
        let params = form_params(&[
            ("courseid", course_id.to_string()),
            ("userid", user_id.to_string()),
        ]);
        let value = self
            .call("gradereport_user_get_grade_items", &params)
            .await?;
        parse::grades(course_id, &value)
    }

    async fn download(
        &self,
        file: &FileRef,
        dest: &Path,
        cancel: CancellationToken,
    ) -> Result<DownloadedFile, MoodleError> {
        if cancel.is_cancelled() {
            return Err(MoodleError::Cancelled);
        }
        let mut url = self.download_url(file)?;
        url.query_pairs_mut()
            .append_pair("token", self.token.expose());

        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(MoodleError::Cancelled),
            response = self.http.get(url).send() => response.map_err(|error| self.network_error(error))?,
        };
        let status = response.status();
        if status.is_redirection() {
            return Err(MoodleError::FileRefused(
                "Moodle redirected the file request".to_string(),
            ));
        }
        if status.is_server_error() {
            return Err(MoodleError::Unavailable(format!(
                "Moodle answered HTTP {status}"
            )));
        }
        if !status.is_success() {
            return Err(MoodleError::FileRefused(format!(
                "Moodle answered HTTP {status}"
            )));
        }
        if let Some(length) = response.content_length() {
            check_download_length(file, length)?;
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with("application/json") {
            let body = self.read_capped(response).await?;
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body)
                && value.get("errorcode").is_some()
                && let Some(error) = self.error_from_value(&value, "file download")
            {
                return Err(error);
            }
            return write_downloaded(dest, &body).await;
        }

        let mut handle = create_private_file(dest).await?;
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        let mut stream = response.bytes_stream();
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => {
                    drop(handle);
                    discard(dest).await;
                    return Err(MoodleError::Cancelled);
                }
                next = stream.next() => next,
            };
            let Some(chunk) = next else {
                break;
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    drop(handle);
                    discard(dest).await;
                    return Err(self.network_error(error));
                }
            };
            written = written.saturating_add(chunk.len() as u64);
            if let Err(error) = check_download_progress(file, written) {
                drop(handle);
                discard(dest).await;
                return Err(error);
            }
            if let Err(error) = handle.write_all(&chunk).await {
                drop(handle);
                discard(dest).await;
                return Err(MoodleError::FileRefused(format!(
                    "cannot write the download: {error}"
                )));
            }
            hasher.update(&chunk);
        }
        if let Err(error) = handle.flush().await {
            drop(handle);
            discard(dest).await;
            return Err(MoodleError::FileRefused(format!(
                "cannot flush the download: {error}"
            )));
        }
        drop(handle);
        let digest = hasher.finalize();
        Ok(DownloadedFile {
            path: dest.to_path_buf(),
            bytes: written,
            sha256: hex(&digest),
        })
    }
}

fn check_download_length(file: &FileRef, length: u64) -> Result<(), MoodleError> {
    if length > MAX_FILE_BYTES {
        return Err(MoodleError::FileRefused(format!(
            "the file is larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    if length > file.size.saturating_add(MAX_DOWNLOAD_OVERSHOOT_BYTES) {
        return Err(MoodleError::FileRefused(
            "the file is larger than its snapshot entry".to_string(),
        ));
    }
    Ok(())
}

fn check_download_progress(file: &FileRef, written: u64) -> Result<(), MoodleError> {
    if written > MAX_FILE_BYTES {
        return Err(MoodleError::FileRefused(format!(
            "the file is larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    if written > file.size.saturating_add(MAX_DOWNLOAD_OVERSHOOT_BYTES) {
        return Err(MoodleError::FileRefused(
            "the file is larger than its snapshot entry".to_string(),
        ));
    }
    Ok(())
}

async fn create_private_file(dest: &Path) -> Result<tokio::fs::File, MoodleError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    options.open(dest).await.map_err(|error| {
        MoodleError::FileRefused(format!("cannot create {}: {error}", dest.display()))
    })
}

async fn write_downloaded(dest: &Path, body: &[u8]) -> Result<DownloadedFile, MoodleError> {
    let mut handle = create_private_file(dest).await?;
    if let Err(error) = handle.write_all(body).await {
        drop(handle);
        discard(dest).await;
        return Err(MoodleError::FileRefused(format!(
            "cannot write the download: {error}"
        )));
    }
    if let Err(error) = handle.flush().await {
        drop(handle);
        discard(dest).await;
        return Err(MoodleError::FileRefused(format!(
            "cannot flush the download: {error}"
        )));
    }
    drop(handle);
    let digest = Sha256::digest(body);
    Ok(DownloadedFile {
        path: dest.to_path_buf(),
        bytes: body.len() as u64,
        sha256: hex(&digest),
    })
}

async fn discard(dest: &Path) {
    let _ = tokio::fs::remove_file(dest).await;
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(base: &str) -> SiteConfig {
        SiteConfig {
            base_url: url::Url::parse(base).expect("test URL"),
        }
    }

    fn client(base: &str) -> RestClient {
        RestClient::new(
            site(base),
            MoodleToken::new("0123456789abcdef0123456789abcdef"),
        )
        .expect("client")
    }

    #[test]
    fn https_is_required_except_for_loopback() {
        assert!(RestClient::new(site("https://example.test"), MoodleToken::new("x")).is_ok());
        assert!(RestClient::new(site("http://127.0.0.1:1234"), MoodleToken::new("x")).is_ok());
        assert!(RestClient::new(site("http://localhost:1234"), MoodleToken::new("x")).is_ok());
        assert!(RestClient::new(site("http://example.test"), MoodleToken::new("x")).is_err());
    }

    #[test]
    fn rest_url_keeps_the_site_subdirectory() {
        let client = client("https://example.test/moodle/");
        let url = client
            .rest_url("core_webservice_get_site_info")
            .expect("url");
        assert_eq!(url.path(), "/moodle/webservice/rest/server.php");
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs.get("wsfunction").map(String::as_str),
            Some("core_webservice_get_site_info")
        );
        assert_eq!(
            pairs.get("moodlewsrestformat").map(String::as_str),
            Some("json")
        );
    }

    #[test]
    fn token_never_appears_in_errors() {
        let client = client("https://example.test");
        let error = MoodleError::Unauthorized(
            client.scrub("invalid token 0123456789abcdef0123456789abcdef"),
        )
        .to_string();
        assert!(!error.contains("0123456789abcdef0123456789abcdef"));
        assert!(error.contains("<token>"));
    }

    #[test]
    fn download_url_rewrites_pluginfile_and_drops_any_token() {
        let client = client("https://example.test");
        let file = FileRef {
            course_id: 1,
            module_id: 2,
            filename: "a.pdf".to_string(),
            filepath: "/".to_string(),
            size: 10,
            modified: None,
            mimetype: None,
            url: "https://example.test/pluginfile.php/1/a.pdf?token=leak&forcedownload=1"
                .to_string(),
        };
        let url = client.download_url(&file).expect("url");
        assert_eq!(url.path(), "/webservice/pluginfile.php/1/a.pdf");
        assert!(!url.as_str().contains("leak"));
        assert!(url.as_str().contains("forcedownload=1"));
    }

    #[test]
    fn download_url_refuses_another_host_and_scheme() {
        let client = client("https://example.test");
        let mut file = FileRef {
            course_id: 1,
            module_id: 2,
            filename: "a.pdf".to_string(),
            filepath: "/".to_string(),
            size: 10,
            modified: None,
            mimetype: None,
            url: "https://evil.test/pluginfile.php/1/a.pdf".to_string(),
        };
        assert!(matches!(
            client.download_url(&file),
            Err(MoodleError::FileRefused(_))
        ));
        file.url = "http://example.test/pluginfile.php/1/a.pdf".to_string();
        assert!(matches!(
            client.download_url(&file),
            Err(MoodleError::FileRefused(_))
        ));
    }

    #[test]
    fn length_caps_are_enforced() {
        let file = FileRef {
            course_id: 1,
            module_id: 2,
            filename: "a.pdf".to_string(),
            filepath: "/".to_string(),
            size: 1024,
            modified: None,
            mimetype: None,
            url: "https://example.test/pluginfile.php/1/a.pdf".to_string(),
        };
        assert!(check_download_length(&file, 2048).is_ok());
        assert!(matches!(
            check_download_length(&file, 1024 + MAX_DOWNLOAD_OVERSHOOT_BYTES + 1),
            Err(MoodleError::FileRefused(_))
        ));
        assert!(matches!(
            check_download_length(&file, MAX_FILE_BYTES + 1),
            Err(MoodleError::FileRefused(_))
        ));
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
