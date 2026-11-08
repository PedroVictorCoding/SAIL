//! Pure parsers for Moodle's JSON replies.
//!
//! Moodle's web service returns loosely typed JSON: numbers can arrive as strings, `visible`
//! can be `0`/`1` or a boolean, and fields appear only when the site's version or the
//! user's permissions provide them. These functions turn that into the contract types and
//! keep every missing required field a [`MoodleError::BadResponse`] instead of a panic.
//! They never do IO, so the bulk of the client tests need no fake server.

use crate::contracts::{
    Announcement, Assignment, CalendarEvent, Course, FileRef, GradeItem, MAX_COURSES,
    MAX_FILES_PER_MODULE, MAX_MODULES_PER_COURSE, Module, ModuleDate, MoodleError, Section,
    SiteInfo, SubmissionState, SubmissionStatus,
};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

type Obj = Map<String, Value>;

fn bad(what: &str) -> MoodleError {
    MoodleError::BadResponse(format!("missing or invalid {what}"))
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Obj, MoodleError> {
    value.as_object().ok_or_else(|| bad(what))
}

fn array<'a>(value: &'a Value, what: &str) -> Result<&'a Vec<Value>, MoodleError> {
    value.as_array().ok_or_else(|| bad(what))
}

fn optional_array<'a>(obj: &'a Obj, key: &str) -> &'a [Value] {
    obj.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<i64>().ok())
        })
}

fn as_bool(value: &Value) -> Option<bool> {
    value
        .as_bool()
        .or_else(|| as_i64(value).map(|number| number != 0))
        .or_else(|| {
            value.as_str().map(|text| {
                let text = text.trim();
                text.eq_ignore_ascii_case("true") || text == "1"
            })
        })
}

fn required_int(obj: &Obj, key: &str) -> Result<i64, MoodleError> {
    obj.get(key).and_then(as_i64).ok_or_else(|| bad(key))
}

fn optional_int(obj: &Obj, key: &str) -> Option<i64> {
    obj.get(key).and_then(as_i64)
}

fn required_string(obj: &Obj, key: &str) -> Result<String, MoodleError> {
    match obj.get(key) {
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(bad(key)),
    }
}

fn optional_string(obj: &Obj, key: &str) -> Option<String> {
    match obj.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

fn bool_or(obj: &Obj, key: &str, default: bool) -> bool {
    obj.get(key).and_then(as_bool).unwrap_or(default)
}

fn timestamp(obj: &Obj, key: &str) -> Option<DateTime<Utc>> {
    optional_int(obj, key)
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
}

fn required_timestamp(obj: &Obj, key: &str) -> Result<DateTime<Utc>, MoodleError> {
    timestamp(obj, key).ok_or_else(|| bad(key))
}

/// Parse `core_webservice_get_site_info`.
///
/// The function list and `downloadfiles` decide which features SAIL may use; a site that
/// withholds them still works, just with fewer features.
pub fn site_info(value: &Value) -> Result<SiteInfo, MoodleError> {
    let obj = object(value, "site info")?;
    let functions = optional_array(obj, "functions")
        .iter()
        .take(MAX_COURSES)
        .filter_map(|function| {
            function
                .as_object()
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    Ok(SiteInfo {
        site_name: required_string(obj, "sitename")?,
        user_id: required_int(obj, "userid")?,
        full_name: required_string(obj, "fullname")?,
        release: optional_string(obj, "release").unwrap_or_default(),
        functions,
        can_download_files: obj.get("downloadfiles").and_then(as_bool).unwrap_or(false),
    })
}

/// Parse `core_enrol_get_users_courses`.
///
/// The list is capped at [`MAX_COURSES`]: a broken or hostile site must not make one sync
/// unbounded.
pub fn courses(value: &Value) -> Result<Vec<Course>, MoodleError> {
    let items = array(value, "courses")?;
    let mut courses = Vec::with_capacity(items.len().min(MAX_COURSES));
    for item in items.iter().take(MAX_COURSES) {
        let obj = object(item, "course")?;
        courses.push(Course {
            id: required_int(obj, "id")?,
            short_name: optional_string(obj, "shortname").unwrap_or_default(),
            full_name: optional_string(obj, "fullname").unwrap_or_default(),
            start: timestamp(obj, "startdate"),
            end: timestamp(obj, "enddate"),
            visible: bool_or(obj, "visible", true),
            modified: timestamp(obj, "timemodified"),
        });
    }
    Ok(courses)
}

/// Parse `core_course_get_contents` for one course.
///
/// Modules and their files are the bulk of a snapshot, so both are capped. A file URL may
/// carry a `token=` query parameter that Moodle adds for some sessions; it is stripped here
/// so a token can never reach a snapshot, a log or the mirror index.
pub fn course_contents(course_id: i64, value: &Value) -> Result<Vec<Section>, MoodleError> {
    let items = array(value, "course contents")?;
    let mut sections = Vec::with_capacity(items.len().min(MAX_MODULES_PER_COURSE));
    let mut remaining_modules = MAX_MODULES_PER_COURSE;
    for item in items.iter().take(MAX_MODULES_PER_COURSE) {
        let obj = object(item, "section")?;
        let mut modules = Vec::new();
        for module in optional_array(obj, "modules") {
            if remaining_modules == 0 {
                break;
            }
            remaining_modules -= 1;
            modules.push(parse_module(course_id, module)?);
        }
        sections.push(Section {
            id: required_int(obj, "id")?,
            number: optional_int(obj, "section")
                .or_else(|| optional_int(obj, "number"))
                .unwrap_or(0),
            name: optional_string(obj, "name").unwrap_or_default(),
            summary_html: optional_string(obj, "summary").unwrap_or_default(),
            visible: bool_or(obj, "visible", true),
            modules,
        });
    }
    Ok(sections)
}

fn parse_module(course_id: i64, value: &Value) -> Result<Module, MoodleError> {
    let obj = object(value, "module")?;
    let module_id = required_int(obj, "id")?;
    let dates = optional_array(obj, "dates")
        .iter()
        .filter_map(|date| {
            let obj = date.as_object()?;
            Some(ModuleDate {
                label: optional_string(obj, "label").unwrap_or_default(),
                at: timestamp(obj, "timestamp")?,
            })
        })
        .collect();
    let files = optional_array(obj, "contents")
        .iter()
        .filter(|content| {
            content
                .as_object()
                .and_then(|content| content.get("type"))
                .and_then(Value::as_str)
                == Some("file")
        })
        .take(MAX_FILES_PER_MODULE)
        .map(|content| {
            let content = content.as_object().ok_or_else(|| bad("file content"))?;
            file_ref(content, course_id, module_id)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Module {
        id: module_id,
        modname: optional_string(obj, "modname").unwrap_or_default(),
        instance: optional_int(obj, "instance").unwrap_or(0),
        name: optional_string(obj, "name").unwrap_or_default(),
        description_html: optional_string(obj, "description").unwrap_or_default(),
        url: optional_string(obj, "url").filter(|url| !url.is_empty()),
        visible: bool_or(obj, "visible", true),
        dates,
        files,
    })
}

/// Parse one file-like object (`contents[]`, `introattachments[]`, forum attachments).
///
/// `filename` and `fileurl` are required; the rest is defaulted because Moodle omits fields
/// such as `mimetype` for some resources. The returned URL never carries a token.
fn file_ref(obj: &Obj, course_id: i64, module_id: i64) -> Result<FileRef, MoodleError> {
    let url = required_string(obj, "fileurl")?;
    Ok(FileRef {
        course_id,
        module_id,
        filename: required_string(obj, "filename")?,
        filepath: optional_string(obj, "filepath").unwrap_or_else(|| "/".to_string()),
        size: optional_int(obj, "filesize").unwrap_or(0).max(0) as u64,
        modified: timestamp(obj, "timemodified"),
        mimetype: optional_string(obj, "mimetype").filter(|kind| !kind.is_empty()),
        url: strip_token_parameter(&url),
    })
}

fn optional_file_ref(obj: &Obj, course_id: i64, module_id: i64) -> Option<FileRef> {
    file_ref(obj, course_id, module_id).ok()
}

/// Remove a `token` query parameter from a file URL.
///
/// Moodle sometimes emits pluginfile URLs with the web-service token attached. It must not
/// be stored, compared or printed; downloads add it back in memory for the one request.
pub fn strip_token_parameter(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut url) => {
            let kept: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(key, _)| key != "token")
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect();
            if kept.len() == url.query_pairs().count() {
                return url.to_string();
            }
            url.set_query(None);
            if !kept.is_empty() {
                let mut query = url.query_pairs_mut();
                for (key, value) in kept {
                    query.append_pair(&key, &value);
                }
            }
            url.to_string()
        }
        Err(_) => {
            let Some((base, query)) = raw.split_once('?') else {
                return raw.to_string();
            };
            let kept: Vec<&str> = query
                .split('&')
                .filter(|pair| pair.split_once('=').map(|(key, _)| key).unwrap_or(pair) != "token")
                .collect();
            if kept.is_empty() {
                base.to_string()
            } else {
                format!("{base}?{}", kept.join("&"))
            }
        }
    }
}

/// Parse `mod_assign_get_assignments` (all requested courses in one reply).
pub fn assignments(value: &Value) -> Result<Vec<Assignment>, MoodleError> {
    let obj = object(value, "assignments reply")?;
    let mut out = Vec::new();
    let mut remaining = MAX_MODULES_PER_COURSE;
    for course in optional_array(obj, "courses").iter().take(MAX_COURSES) {
        let course_obj = object(course, "assignment course")?;
        let course_id = required_int(course_obj, "id")?;
        for assignment in optional_array(course_obj, "assignments") {
            if remaining == 0 {
                break;
            }
            remaining -= 1;
            let assignment_obj = object(assignment, "assignment")?;
            let module_id = required_int(assignment_obj, "cmid")?;
            let attachments = optional_array(assignment_obj, "introattachments")
                .iter()
                .filter_map(|attachment| attachment.as_object())
                .filter_map(|attachment| optional_file_ref(attachment, course_id, module_id))
                .take(MAX_FILES_PER_MODULE)
                .collect();
            out.push(Assignment {
                id: required_int(assignment_obj, "id")?,
                module_id,
                course_id: optional_int(assignment_obj, "course").unwrap_or(course_id),
                name: optional_string(assignment_obj, "name").unwrap_or_default(),
                intro_html: optional_string(assignment_obj, "intro").unwrap_or_default(),
                opens: timestamp(assignment_obj, "allowsubmissionsfromdate"),
                due: timestamp(assignment_obj, "duedate"),
                cutoff: timestamp(assignment_obj, "cutoffdate"),
                attachments,
            });
        }
    }
    Ok(out)
}

/// Parse `mod_assign_get_submission_status` for one assignment.
///
/// `lastattempt.submission` is absent before the owner opens the assignment; that is a
/// normal state, not a broken reply, so it maps to [`SubmissionState::New`].
pub fn submission_status(
    assignment_id: i64,
    value: &Value,
) -> Result<SubmissionStatus, MoodleError> {
    let obj = object(value, "submission status")?;
    let attempt = obj.get("lastattempt").and_then(Value::as_object);
    let submission = attempt
        .and_then(|attempt| attempt.get("submission"))
        .and_then(Value::as_object);
    let state = submission
        .and_then(|submission| submission.get("status"))
        .and_then(Value::as_str)
        .map(|status| match status {
            "new" => SubmissionState::New,
            "draft" => SubmissionState::Draft,
            "submitted" => SubmissionState::Submitted,
            "reopened" => SubmissionState::Reopened,
            _ => SubmissionState::Unknown,
        })
        .unwrap_or(SubmissionState::New);
    let grade = obj
        .get("feedback")
        .and_then(Value::as_object)
        .and_then(|feedback| optional_string(feedback, "gradefordisplay"))
        .filter(|grade| !grade.trim().is_empty() && grade.trim() != "-");
    Ok(SubmissionStatus {
        assignment_id,
        state,
        graded: grade.is_some(),
        grade,
        modified: submission.and_then(|submission| timestamp(submission, "timemodified")),
    })
}

/// Parse `core_calendar_get_calendar_events`.
pub fn events(value: &Value) -> Result<Vec<CalendarEvent>, MoodleError> {
    let obj = object(value, "events reply")?;
    let mut events = Vec::new();
    for event in optional_array(obj, "events")
        .iter()
        .take(MAX_MODULES_PER_COURSE)
    {
        let event = object(event, "event")?;
        events.push(CalendarEvent {
            id: required_int(event, "id")?,
            name: optional_string(event, "name").unwrap_or_default(),
            description_html: optional_string(event, "description").unwrap_or_default(),
            course_id: optional_int(event, "courseid").filter(|id| *id > 0),
            module_id: optional_int(event, "coursemoduleid")
                .or_else(|| optional_int(event, "cmid"))
                .filter(|id| *id > 0),
            event_type: optional_string(event, "eventtype").unwrap_or_default(),
            modname: optional_string(event, "modulename").filter(|name| !name.is_empty()),
            starts: required_timestamp(event, "timestart")?,
            duration_seconds: optional_int(event, "timeduration").unwrap_or(0),
            location: optional_string(event, "location").filter(|place| !place.is_empty()),
        });
    }
    Ok(events)
}

/// One news forum from `mod_forum_get_forums_by_courses`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forum {
    pub id: i64,
    pub course_id: i64,
    pub name: String,
}

/// Parse `mod_forum_get_forums_by_courses`, keeping only news forums.
///
/// Moodle returns every forum of the course; announcements live in the news forum only.
pub fn forums(value: &Value) -> Result<Vec<Forum>, MoodleError> {
    let items: &[Value] = if value.is_array() {
        array(value, "forums")?
    } else {
        optional_array(object(value, "forums reply")?, "forums")
    };
    let mut forums = Vec::new();
    for forum in items.iter().take(MAX_COURSES) {
        let forum = object(forum, "forum")?;
        if optional_string(forum, "type").as_deref() != Some("news") {
            continue;
        }
        forums.push(Forum {
            id: required_int(forum, "id")?,
            course_id: required_int(forum, "course")?,
            name: optional_string(forum, "name").unwrap_or_default(),
        });
    }
    Ok(forums)
}

/// Parse one page of `mod_forum_get_forum_discussions`.
///
/// Every discussion in a news forum is an announcement. Attachments are optional; a
/// malformed attachment is skipped rather than failing the whole page, because the
/// discussion text is still useful without it.
pub fn discussions(
    course_id: i64,
    forum_id: i64,
    value: &Value,
) -> Result<Vec<Announcement>, MoodleError> {
    let obj = object(value, "discussions reply")?;
    let mut announcements = Vec::new();
    for discussion in optional_array(obj, "discussions") {
        let discussion = object(discussion, "discussion")?;
        let posted = required_timestamp(discussion, "created")?;
        let modified = timestamp(discussion, "modified").unwrap_or(posted);
        // The discussions reply carries the forum instance id, not the course-module id.
        // The forum id is the closest stable identity available here, so it stands in for
        // `module_id`; the service can cross-reference the course contents when it needs the
        // real cmid.
        let attachments = optional_array(discussion, "attachments")
            .iter()
            .filter_map(|attachment| attachment.as_object())
            .filter_map(|attachment| optional_file_ref(attachment, course_id, forum_id))
            .take(MAX_FILES_PER_MODULE)
            .collect();
        announcements.push(Announcement {
            discussion_id: required_int(discussion, "id")?,
            course_id: optional_int(discussion, "course").unwrap_or(course_id),
            subject: optional_string(discussion, "name").unwrap_or_default(),
            message_html: optional_string(discussion, "message").unwrap_or_default(),
            author: optional_string(discussion, "userfullname").filter(|name| !name.is_empty()),
            posted,
            modified,
            attachments,
        });
    }
    Ok(announcements)
}

/// Parse `gradereport_user_get_grade_items` for one user and course.
pub fn grades(course_id: i64, value: &Value) -> Result<Vec<GradeItem>, MoodleError> {
    let obj = object(value, "grades reply")?;
    let Some(user) = optional_array(obj, "usergrades").first() else {
        return Ok(Vec::new());
    };
    let user = object(user, "user grade report")?;
    let mut grades = Vec::with_capacity(optional_array(user, "gradeitems").len());
    for item in optional_array(user, "gradeitems")
        .iter()
        .take(MAX_MODULES_PER_COURSE)
    {
        let item = object(item, "grade item")?;
        let grade = optional_string(item, "gradeformatted")
            .or_else(|| optional_string(item, "grade"))
            .filter(|grade| !grade.trim().is_empty());
        let range = optional_string(item, "range").or_else(|| {
            let min = optional_string(item, "grademin")?;
            let max = optional_string(item, "grademax")?;
            Some(format!("{min}–{max}"))
        });
        grades.push(GradeItem {
            course_id: optional_int(item, "courseid").unwrap_or(course_id),
            item_id: required_int(item, "id")?,
            name: optional_string(item, "itemname").unwrap_or_default(),
            grade,
            range,
            percentage: optional_string(item, "percentageformatted")
                .or_else(|| optional_string(item, "percentage")),
            feedback_html: optional_string(item, "feedback").filter(|text| !text.is_empty()),
            modified: timestamp(item, "timemodified"),
        });
    }
    Ok(grades)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let text = match name {
            "site_info" => include_str!("../../tests/fixtures/client/site_info.json"),
            "courses" => include_str!("../../tests/fixtures/client/courses.json"),
            "course_contents" => include_str!("../../tests/fixtures/client/course_contents.json"),
            "assignments" => include_str!("../../tests/fixtures/client/assignments.json"),
            "submission_status" => {
                include_str!("../../tests/fixtures/client/submission_status.json")
            }
            "events" => include_str!("../../tests/fixtures/client/events.json"),
            "forums" => include_str!("../../tests/fixtures/client/forums.json"),
            "discussions" => include_str!("../../tests/fixtures/client/discussions.json"),
            "grades" => include_str!("../../tests/fixtures/client/grades.json"),
            other => panic!("unknown fixture {other}"),
        };
        serde_json::from_str(text).expect("fixture JSON")
    }

    #[test]
    fn parses_site_info() {
        let info = site_info(&fixture("site_info")).expect("site info");
        assert_eq!(info.site_name, "ISCTE Moodle");
        assert_eq!(info.user_id, 42);
        assert_eq!(info.full_name, "Pessoa Exemplo");
        assert_eq!(info.functions.len(), 2);
        assert!(info.can_download_files);
    }

    #[test]
    fn parses_courses_and_caps_them() {
        let courses = courses(&fixture("courses")).expect("courses");
        assert_eq!(courses.len(), 2);
        assert_eq!(courses[0].short_name, "LEIC");
        assert_eq!(
            courses[0].start,
            DateTime::<Utc>::from_timestamp(1_767_225_600, 0)
        );
        assert!(!courses[1].visible);
    }

    #[test]
    fn parses_sections_modules_files_and_dates() {
        let sections = course_contents(7, &fixture("course_contents")).expect("contents");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].name, "Semana 1");
        assert_eq!(sections[0].modules.len(), 2);
        let resource = &sections[0].modules[0];
        assert_eq!(resource.modname, "resource");
        assert_eq!(resource.files.len(), 1);
        assert_eq!(resource.files[0].filename, "Enunciado.pdf");
        assert_eq!(resource.files[0].module_id, 101);
        assert!(resource.files[0].url.contains("pluginfile.php"));
        assert!(!resource.files[0].url.contains("token="));
        assert!(!resource.dates.is_empty());
        let assign = &sections[0].modules[1];
        assert_eq!(assign.modname, "assign");
        assert!(assign.files.is_empty());
        assert_eq!(sections[1].modules.len(), 1);
        assert_eq!(sections[1].modules[0].modname, "forum");
    }

    #[test]
    fn strips_a_token_from_file_urls() {
        assert_eq!(
            strip_token_parameter(
                "https://example.test/pluginfile.php/1/a.pdf?token=abc123&forcedownload=1"
            ),
            "https://example.test/pluginfile.php/1/a.pdf?forcedownload=1"
        );
        assert_eq!(
            strip_token_parameter("https://example.test/pluginfile.php/1/a.pdf?token=abc123"),
            "https://example.test/pluginfile.php/1/a.pdf"
        );
        assert_eq!(
            strip_token_parameter("not a url?token=abc123&x=1"),
            "not a url?x=1"
        );
    }

    #[test]
    fn parses_assignments_and_attachments() {
        let assignments = assignments(&fixture("assignments")).expect("assignments");
        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].id, 501);
        assert_eq!(assignments[0].module_id, 101);
        assert_eq!(
            assignments[0].due,
            DateTime::<Utc>::from_timestamp(1_800_000_000, 0)
        );
        assert_eq!(assignments[0].attachments.len(), 1);
        assert_eq!(assignments[0].attachments[0].filename, "Brief.pdf");
        assert_eq!(assignments[1].opens, None);
    }

    #[test]
    fn parses_submission_status_with_and_without_an_attempt() {
        let status = submission_status(501, &fixture("submission_status")).expect("status");
        assert_eq!(status.state, SubmissionState::Submitted);
        assert!(status.graded);
        assert_eq!(status.grade.as_deref(), Some("15,50"));

        let empty = serde_json::json!({"lastattempt": {}, "feedback": {"grade": "-"}});
        let status = submission_status(501, &empty).expect("empty status");
        assert_eq!(status.state, SubmissionState::New);
        assert!(!status.graded);
    }

    #[test]
    fn parses_events() {
        let events = events(&fixture("events")).expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, 900);
        assert_eq!(events[0].event_type, "course");
        assert_eq!(events[0].modname.as_deref(), Some("quiz"));
        assert_eq!(events[0].module_id, Some(77));
        assert_eq!(events[1].course_id, None);
    }

    #[test]
    fn keeps_only_news_forums() {
        let forums = forums(&fixture("forums")).expect("forums");
        assert_eq!(forums.len(), 1);
        assert_eq!(forums[0].id, 800);
        assert_eq!(forums[0].course_id, 7);
    }

    #[test]
    fn parses_announcements() {
        let announcements = discussions(7, 800, &fixture("discussions")).expect("discussions");
        assert_eq!(announcements.len(), 2);
        assert_eq!(announcements[0].discussion_id, 9001);
        assert_eq!(announcements[0].subject, "Data do 1.º teste");
        assert_eq!(announcements[0].author.as_deref(), Some("Docente Exemplo"));
        assert_eq!(announcements[0].attachments.len(), 1);
        assert_eq!(announcements[1].modified, announcements[1].posted);
    }

    #[test]
    fn parses_grades() {
        let grades = grades(7, &fixture("grades")).expect("grades");
        assert_eq!(grades.len(), 2);
        assert_eq!(grades[0].name, "Teste 1");
        assert_eq!(grades[0].grade.as_deref(), Some("15,50"));
        assert_eq!(grades[0].range.as_deref(), Some("0–20"));
        assert_eq!(grades[0].percentage.as_deref(), Some("77,5 %"));
        assert_eq!(grades[1].grade, None);
    }

    #[test]
    fn missing_required_fields_are_bad_responses() {
        assert!(matches!(
            site_info(&serde_json::json!({"sitename": "x"})),
            Err(MoodleError::BadResponse(_))
        ));
        assert!(matches!(
            courses(&serde_json::json!({"not": "an array"})),
            Err(MoodleError::BadResponse(_))
        ));
        assert!(matches!(
            events(&serde_json::json!({"events": [{"id": 1}]})),
            Err(MoodleError::BadResponse(_))
        ));
    }

    #[test]
    fn null_collections_are_empty() {
        assert!(courses(&Value::Null).is_err());
        assert_eq!(discussions(1, 2, &serde_json::json!({})).unwrap().len(), 0);
        assert_eq!(
            grades(1, &serde_json::json!({"usergrades": []}))
                .unwrap()
                .len(),
            0
        );
    }
}
