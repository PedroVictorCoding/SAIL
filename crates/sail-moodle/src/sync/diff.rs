//! What changed between two snapshots of a course.
//!
//! `course_list_diff` owns `CourseAdded`/`CourseRemoved`. `diff(None, …)` (first sight of a
//! course) reports nothing, so connecting to Moodle doesn't produce a change for every file
//! of every course. Change ids hash the course, kind, subject and new value: re-running a
//! diff on the same pair yields the same ids, and the store ignores duplicates.
use crate::contracts::*;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

/// Longest `detail` text a change carries, in characters.
pub const MAX_DETAIL_CHARS: usize = 300;

fn change_id(course_id: i64, kind: ChangeKind, subject: &ChangeSubject, value: &str) -> String {
    let mut h = Sha256::new();
    h.update(course_id.to_le_bytes());
    h.update(serde_json::to_vec(&kind).unwrap_or_default());
    h.update(serde_json::to_vec(subject).unwrap_or_default());
    h.update(value.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn make(
    course_id: i64,
    kind: ChangeKind,
    subject: ChangeSubject,
    value: &str,
    title: String,
    detail: Option<String>,
    at: DateTime<Utc>,
) -> Change {
    Change {
        id: change_id(course_id, kind, &subject, value),
        course_id,
        kind,
        title: clip(&title, 160),
        detail: detail.map(|d| clip(&d, MAX_DETAIL_CHARS)),
        subject,
        observed_at: at,
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// Whitespace-collapsed HTML, so re-indentation by the editor isn't a change.
fn collapse(html: &str) -> String {
    html.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn iso(t: Option<DateTime<Utc>>) -> String {
    t.map(|t| t.to_rfc3339()).unwrap_or_else(|| "none".into())
}

fn has_grade(g: &Option<String>) -> bool {
    g.as_deref()
        .is_some_and(|v| !v.trim().is_empty() && v.trim() != "-")
}

/// Courses enrolled in or dropped since the last sync.
pub fn course_list_diff(prev: &[Course], next: &[Course], at: DateTime<Utc>) -> Vec<Change> {
    let old: BTreeMap<i64, &Course> = prev.iter().map(|c| (c.id, c)).collect();
    let new: BTreeMap<i64, &Course> = next.iter().map(|c| (c.id, c)).collect();
    let mut out = Vec::new();
    for (id, course) in &new {
        if !old.contains_key(id) {
            out.push(make(
                *id,
                ChangeKind::CourseAdded,
                ChangeSubject::Course,
                &course.full_name,
                format!("New course: {}", course.full_name),
                None,
                at,
            ));
        }
    }
    for (id, course) in &old {
        if !new.contains_key(id) {
            out.push(make(
                *id,
                ChangeKind::CourseRemoved,
                ChangeSubject::Course,
                &at.date_naive().to_string(),
                format!("Course no longer listed: {}", course.full_name),
                None,
                at,
            ));
        }
    }
    out
}

/// Every change between `prev` and `next` of one course, in a fixed order.
pub fn diff(prev: Option<&CourseSnapshot>, next: &CourseSnapshot) -> Vec<Change> {
    let Some(prev) = prev else {
        return Vec::new();
    };
    let at = next.fetched_at;
    let cid = next.course.id;
    let mut out = Vec::new();

    // Sections.
    let old_sections: HashMap<i64, &Section> = prev.sections.iter().map(|s| (s.id, s)).collect();
    for s in &next.sections {
        match old_sections.get(&s.id) {
            None => out.push(make(
                cid,
                ChangeKind::SectionAdded,
                ChangeSubject::Section { section_id: s.id },
                &s.name,
                format!("New section: {}", s.name),
                None,
                at,
            )),
            Some(o)
                if o.name != s.name || collapse(&o.summary_html) != collapse(&s.summary_html) =>
            {
                out.push(make(
                    cid,
                    ChangeKind::SectionChanged,
                    ChangeSubject::Section { section_id: s.id },
                    &format!("{}|{}", s.name, collapse(&s.summary_html)),
                    format!("Section updated: {}", s.name),
                    (o.name != s.name).then(|| format!("{} → {}", o.name, s.name)),
                    at,
                ))
            }
            _ => {}
        }
    }

    // Modules.
    let modules = |snap: &CourseSnapshot| -> BTreeMap<i64, Module> {
        snap.sections
            .iter()
            .flat_map(|s| s.modules.iter())
            .map(|m| (m.id, m.clone()))
            .collect()
    };
    let old_mods = modules(prev);
    let new_mods = modules(next);
    for (id, m) in &new_mods {
        let subject = ChangeSubject::Module { module_id: *id };
        match old_mods.get(id) {
            None => out.push(make(
                cid,
                ChangeKind::ModuleAdded,
                subject,
                &m.name,
                format!("New {}: {}", m.modname, m.name),
                None,
                at,
            )),
            Some(o) => {
                if o.name != m.name {
                    out.push(make(
                        cid,
                        ChangeKind::ModuleRenamed,
                        subject.clone(),
                        &m.name,
                        format!("Renamed: {}", m.name),
                        Some(format!("{} → {}", o.name, m.name)),
                        at,
                    ));
                }
                if collapse(&o.description_html) != collapse(&m.description_html) {
                    out.push(make(
                        cid,
                        ChangeKind::ModuleDescriptionChanged,
                        subject.clone(),
                        &collapse(&m.description_html),
                        format!("Description updated: {}", m.name),
                        None,
                        at,
                    ));
                }
                if o.visible != m.visible {
                    out.push(make(
                        cid,
                        ChangeKind::ModuleVisibilityChanged,
                        subject,
                        &m.visible.to_string(),
                        format!(
                            "{}: {}",
                            if m.visible { "Now visible" } else { "Hidden" },
                            m.name
                        ),
                        None,
                        at,
                    ));
                }
            }
        }
    }
    for (id, m) in &old_mods {
        if !new_mods.contains_key(id) {
            out.push(make(
                cid,
                ChangeKind::ModuleRemoved,
                ChangeSubject::Module { module_id: *id },
                &at.date_naive().to_string(),
                format!("Removed: {}", m.name),
                None,
                at,
            ));
        }
    }

    // Files (module files, assignment and announcement attachments).
    let files = |snap: &CourseSnapshot| -> BTreeMap<FileKey, FileRef> {
        crate::sync::mirror::snapshot_files(snap)
            .into_iter()
            .map(|f| (f.key(), f))
            .collect()
    };
    let old_files = files(prev);
    let new_files = files(next);
    for (key, f) in &new_files {
        let subject = ChangeSubject::File { key: key.clone() };
        match old_files.get(key) {
            None => out.push(make(
                cid,
                ChangeKind::FileAdded,
                subject,
                &format!("{}|{}", f.size, iso(f.modified)),
                format!("New file: {}", f.filename),
                None,
                at,
            )),
            Some(o) if o.size != f.size || o.modified != f.modified => out.push(make(
                cid,
                ChangeKind::FileUpdated,
                subject,
                &format!("{}|{}", f.size, iso(f.modified)),
                format!("Updated file: {}", f.filename),
                Some(format!("{} → {} bytes", o.size, f.size)),
                at,
            )),
            _ => {}
        }
    }
    for (key, f) in &old_files {
        if !new_files.contains_key(key) {
            out.push(make(
                cid,
                ChangeKind::FileRemoved,
                ChangeSubject::File { key: key.clone() },
                &at.date_naive().to_string(),
                format!("File removed: {}", f.filename),
                None,
                at,
            ));
        }
    }

    // Assignments.
    let old_assign: HashMap<i64, &Assignment> =
        prev.assignments.iter().map(|a| (a.id, a)).collect();
    for a in &next.assignments {
        let subject = ChangeSubject::Assignment {
            assignment_id: a.id,
        };
        match old_assign.get(&a.id) {
            None => out.push(make(
                cid,
                ChangeKind::AssignmentAdded,
                subject,
                &a.name,
                format!("New assignment: {}", a.name),
                a.due.map(|d| format!("due {}", d.to_rfc3339())),
                at,
            )),
            Some(o) => {
                if o.due != a.due || o.cutoff != a.cutoff {
                    out.push(make(
                        cid,
                        ChangeKind::AssignmentDueChanged,
                        subject.clone(),
                        &format!("{}|{}", iso(a.due), iso(a.cutoff)),
                        format!("Due date changed: {}", a.name),
                        Some(format!("{} → {}", iso(o.due), iso(a.due))),
                        at,
                    ));
                }
                if collapse(&o.intro_html) != collapse(&a.intro_html) {
                    out.push(make(
                        cid,
                        ChangeKind::AssignmentBriefChanged,
                        subject,
                        &collapse(&a.intro_html),
                        format!("Brief updated: {}", a.name),
                        None,
                        at,
                    ));
                }
            }
        }
    }

    // Submissions.
    let names: HashMap<i64, &str> = next
        .assignments
        .iter()
        .map(|a| (a.id, a.name.as_str()))
        .collect();
    let old_subs: HashMap<i64, &SubmissionStatus> = prev
        .submissions
        .iter()
        .map(|s| (s.assignment_id, s))
        .collect();
    for s in &next.submissions {
        if let Some(o) = old_subs.get(&s.assignment_id)
            && (o.state != s.state || o.graded != s.graded)
        {
            let name = names.get(&s.assignment_id).copied().unwrap_or("assignment");
            out.push(make(
                cid,
                ChangeKind::SubmissionChanged,
                ChangeSubject::Assignment {
                    assignment_id: s.assignment_id,
                },
                &format!("{:?}|{}", s.state, s.graded),
                if s.graded && !o.graded {
                    format!("Graded: {name}")
                } else {
                    format!("Submission status: {name}")
                },
                Some(format!("{:?} → {:?}", o.state, s.state).to_lowercase()),
                at,
            ));
        }
    }

    // Events. The window slides forward, so an event that has simply passed isn't removed.
    let old_events: HashMap<i64, &CalendarEvent> = prev.events.iter().map(|e| (e.id, e)).collect();
    let new_events: HashMap<i64, &CalendarEvent> = next.events.iter().map(|e| (e.id, e)).collect();
    let mut event_ids: Vec<i64> = new_events.keys().copied().collect();
    event_ids.sort_unstable();
    for id in event_ids {
        let e = new_events[&id];
        let subject = ChangeSubject::Event { event_id: id };
        match old_events.get(&id) {
            None => out.push(make(
                cid,
                ChangeKind::EventAdded,
                subject,
                &e.starts.to_rfc3339(),
                format!("New event: {}", e.name),
                Some(e.starts.to_rfc3339()),
                at,
            )),
            Some(o) if o.starts != e.starts || o.duration_seconds != e.duration_seconds => out
                .push(make(
                    cid,
                    ChangeKind::EventMoved,
                    subject,
                    &format!("{}|{}", e.starts.to_rfc3339(), e.duration_seconds),
                    format!("Event moved: {}", e.name),
                    Some(format!(
                        "{} → {}",
                        o.starts.to_rfc3339(),
                        e.starts.to_rfc3339()
                    )),
                    at,
                )),
            _ => {}
        }
    }
    let mut old_ids: Vec<i64> = old_events.keys().copied().collect();
    old_ids.sort_unstable();
    for id in old_ids {
        let o = old_events[&id];
        if !new_events.contains_key(&id) && o.starts > next.fetched_at {
            out.push(make(
                cid,
                ChangeKind::EventRemoved,
                ChangeSubject::Event { event_id: id },
                &at.date_naive().to_string(),
                format!("Event removed: {}", o.name),
                Some(format!("was {}", o.starts.to_rfc3339())),
                at,
            ));
        }
    }

    // Announcements.
    let old_ann: HashMap<i64, &Announcement> = prev
        .announcements
        .iter()
        .map(|a| (a.discussion_id, a))
        .collect();
    for a in &next.announcements {
        let subject = ChangeSubject::Announcement {
            discussion_id: a.discussion_id,
        };
        match old_ann.get(&a.discussion_id) {
            None => out.push(make(
                cid,
                ChangeKind::AnnouncementPosted,
                subject,
                &a.modified.to_rfc3339(),
                format!("Announcement: {}", a.subject),
                a.author.clone(),
                at,
            )),
            Some(o) if o.modified != a.modified => out.push(make(
                cid,
                ChangeKind::AnnouncementEdited,
                subject,
                &a.modified.to_rfc3339(),
                format!("Announcement edited: {}", a.subject),
                None,
                at,
            )),
            _ => {}
        }
    }

    // Grades.
    let old_grades: HashMap<i64, &GradeItem> = prev.grades.iter().map(|g| (g.item_id, g)).collect();
    for g in &next.grades {
        let Some(o) = old_grades.get(&g.item_id) else {
            if has_grade(&g.grade) {
                out.push(grade_change(cid, g, ChangeKind::GradeReleased, None, at));
            }
            continue;
        };
        match (has_grade(&o.grade), has_grade(&g.grade)) {
            (false, true) => out.push(grade_change(cid, g, ChangeKind::GradeReleased, None, at)),
            (true, true) if o.grade != g.grade => out.push(grade_change(
                cid,
                g,
                ChangeKind::GradeChanged,
                o.grade.clone(),
                at,
            )),
            _ => {}
        }
    }
    out
}

fn grade_change(
    cid: i64,
    g: &GradeItem,
    kind: ChangeKind,
    old: Option<String>,
    at: DateTime<Utc>,
) -> Change {
    let value = g.grade.clone().unwrap_or_default();
    make(
        cid,
        kind,
        ChangeSubject::Grade { item_id: g.item_id },
        &value,
        if kind == ChangeKind::GradeReleased {
            format!("Grade released: {}", g.name)
        } else {
            format!("Grade changed: {}", g.name)
        },
        Some(match old {
            Some(old) => format!("{old} → {value}"),
            None => value.clone(),
        }),
        at,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    pub fn t(days: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap() + Duration::days(days)
    }

    pub fn file(module_id: i64, name: &str, size: u64) -> FileRef {
        FileRef {
            course_id: 7,
            module_id,
            filename: name.into(),
            filepath: "/".into(),
            size,
            modified: Some(t(0)),
            mimetype: Some("application/pdf".into()),
            url: format!("https://moodle.test/pluginfile.php/1/{name}"),
        }
    }

    pub fn snapshot(at: DateTime<Utc>) -> CourseSnapshot {
        CourseSnapshot {
            course: Course {
                id: 7,
                short_name: "POO".into(),
                full_name: "Programação Orientada a Objetos".into(),
                start: None,
                end: None,
                visible: true,
                modified: None,
            },
            sections: vec![Section {
                id: 1,
                number: 0,
                name: "Geral".into(),
                summary_html: "<p>Bem-vindos</p>".into(),
                visible: true,
                modules: vec![Module {
                    id: 10,
                    modname: "resource".into(),
                    instance: 100,
                    name: "Slides 1".into(),
                    description_html: String::new(),
                    url: None,
                    visible: true,
                    dates: vec![],
                    files: vec![file(10, "slides1.pdf", 1000)],
                }],
            }],
            assignments: vec![Assignment {
                id: 50,
                module_id: 20,
                course_id: 7,
                name: "Projeto".into(),
                intro_html: "<p>Enunciado</p>".into(),
                opens: None,
                due: Some(t(20)),
                cutoff: None,
                attachments: vec![],
            }],
            submissions: vec![SubmissionStatus {
                assignment_id: 50,
                state: SubmissionState::New,
                graded: false,
                grade: None,
                modified: None,
            }],
            events: vec![CalendarEvent {
                id: 900,
                name: "Teste 1".into(),
                description_html: String::new(),
                course_id: Some(7),
                module_id: None,
                event_type: "course".into(),
                modname: None,
                starts: t(5),
                duration_seconds: 3600,
                location: None,
            }],
            announcements: vec![],
            grades: vec![GradeItem {
                course_id: 7,
                item_id: 3,
                name: "Teste 1".into(),
                grade: Some("-".into()),
                range: Some("0–20".into()),
                percentage: None,
                feedback_html: None,
                modified: None,
            }],
            fetched_at: at,
        }
    }

    fn kinds(changes: &[Change]) -> Vec<ChangeKind> {
        changes.iter().map(|c| c.kind).collect()
    }

    #[test]
    fn first_sight_is_quiet_and_identical_snapshots_are_quiet() {
        let a = snapshot(t(0));
        assert!(diff(None, &a).is_empty());
        assert!(diff(Some(&a), &snapshot(t(1))).is_empty());
    }

    #[test]
    fn files_modules_and_grades() {
        let a = snapshot(t(0));
        let mut b = snapshot(t(1));
        b.sections[0].modules[0].files[0].size = 2000;
        b.sections[0].modules[0]
            .files
            .push(file(10, "slides2.pdf", 10));
        b.sections[0].modules[0].name = "Slides".into();
        b.sections[0].modules[0].description_html = "  ".into();
        b.grades[0].grade = Some("15,5".into());
        let changes = diff(Some(&a), &b);
        assert_eq!(
            kinds(&changes),
            vec![
                ChangeKind::ModuleRenamed,
                ChangeKind::FileUpdated,
                ChangeKind::FileAdded,
                ChangeKind::GradeReleased
            ]
        );
        let mut c = b.clone();
        c.fetched_at = t(2);
        c.grades[0].grade = Some("16".into());
        c.sections[0].modules.clear();
        let changes = diff(Some(&b), &c);
        assert_eq!(
            kinds(&changes),
            vec![
                ChangeKind::ModuleRemoved,
                ChangeKind::FileRemoved,
                ChangeKind::FileRemoved,
                ChangeKind::GradeChanged
            ]
        );
    }

    #[test]
    fn whitespace_only_edits_are_not_changes() {
        let a = snapshot(t(0));
        let mut b = snapshot(t(1));
        b.sections[0].summary_html = "<p>Bem-vindos</p>\n\n".into();
        b.assignments[0].intro_html = "<p>Enunciado</p>  ".into();
        assert!(diff(Some(&a), &b).is_empty());
    }

    #[test]
    fn due_dates_submissions_and_events() {
        let a = snapshot(t(0));
        let mut b = snapshot(t(1));
        b.assignments[0].due = Some(t(22));
        b.submissions[0].state = SubmissionState::Submitted;
        b.events[0].starts = t(6);
        let changes = diff(Some(&a), &b);
        assert_eq!(
            kinds(&changes),
            vec![
                ChangeKind::AssignmentDueChanged,
                ChangeKind::SubmissionChanged,
                ChangeKind::EventMoved
            ]
        );
        assert!(changes[0].detail.as_deref().unwrap().contains("→"));
    }

    #[test]
    fn a_past_event_leaving_the_window_is_not_a_removal() {
        let a = snapshot(t(0));
        let mut later = snapshot(t(10));
        later.events.clear();
        assert!(
            diff(Some(&a), &later).is_empty(),
            "event at t(5) is in the past at t(10)"
        );
        let mut soon = snapshot(t(1));
        soon.events.clear();
        assert_eq!(
            kinds(&diff(Some(&a), &soon)),
            vec![ChangeKind::EventRemoved]
        );
    }

    #[test]
    fn ids_are_stable_across_reruns_and_distinct_per_value() {
        let a = snapshot(t(0));
        let mut b = snapshot(t(1));
        b.assignments[0].due = Some(t(22));
        let first = diff(Some(&a), &b);
        let mut b2 = b.clone();
        b2.fetched_at = t(3);
        let again = diff(Some(&a), &b2);
        assert_eq!(first[0].id, again[0].id);
        let mut c = b.clone();
        c.assignments[0].due = Some(t(25));
        assert_ne!(diff(Some(&b), &c)[0].id, first[0].id);
    }

    #[test]
    fn course_list_owns_added_and_removed() {
        let a = snapshot(t(0)).course;
        let mut b = a.clone();
        b.id = 8;
        b.full_name = "Cálculo".into();
        let changes = course_list_diff(std::slice::from_ref(&a), std::slice::from_ref(&b), t(0));
        assert_eq!(
            kinds(&changes),
            vec![ChangeKind::CourseAdded, ChangeKind::CourseRemoved]
        );
        assert!(
            course_list_diff(std::slice::from_ref(&a), std::slice::from_ref(&a), t(0)).is_empty()
        );
    }
}
