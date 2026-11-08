//! Structured academic item extraction from Moodle assignments and calendar events.
//!
//! Converts authoritative dates already stored in Moodle (such as assignment due dates
//! and calendar events) directly into [`AcademicItem`] instances without model inference,
//! setting their timezone to `Europe/Lisbon` and marking them as `Confirmed`.

use super::dates::find_dates;
use super::merge::item_id;
use crate::contracts::{
    AcademicItem, Confidence, CourseSnapshot, DocSource, Evidence, ItemKind, ItemOrigin, ItemWhen,
    MAX_QUOTE_CHARS,
};
use chrono_tz::Europe::Lisbon;

/// Extract authoritative academic items from structured Moodle fields in `snapshot`.
///
/// Assignments with a `due` date become [`ItemKind::Deadline`]. Calendar events with
/// `modname == "quiz"` become [`ItemKind::Quiz`]; events with `modname == "assign"` are skipped
/// to avoid duplicating assignment items; other calendar events infer their [`ItemKind`] from
/// keywords in their title, or default to [`ItemKind::Other`]. All timestamps are converted
/// to `Europe/Lisbon` local time.
pub fn items_from_snapshot(snapshot: &CourseSnapshot) -> Vec<AcademicItem> {
    let mut items = Vec::new();
    let course_id = snapshot.course.id;

    // 1. Assignments with due date
    for assignment in &snapshot.assignments {
        if let Some(due) = assignment.due {
            let local_due = due.with_timezone(&Lisbon);
            let date = local_due.date_naive();
            let time = Some(local_due.time());

            let mut title = assignment.name.clone();
            if title.len() > 120 {
                let mut cut = 120;
                while !title.is_char_boundary(cut) && cut > 0 {
                    cut -= 1;
                }
                title.truncate(cut);
            }

            let mut quote = assignment.name.clone();
            if quote.len() > MAX_QUOTE_CHARS {
                let mut cut = MAX_QUOTE_CHARS;
                while !quote.is_char_boundary(cut) && cut > 0 {
                    cut -= 1;
                }
                quote.truncate(cut);
            }

            let id = item_id(course_id, ItemKind::Deadline, date, &title);
            let evidence = Evidence {
                doc_id: format!("moodle:assign:{}", assignment.id),
                source: DocSource::AssignmentBrief {
                    assignment_id: assignment.id,
                },
                label: title.clone(),
                quote,
            };

            items.push(AcademicItem {
                id,
                course_id,
                kind: ItemKind::Deadline,
                title,
                when: ItemWhen {
                    date,
                    time,
                    end_date: None,
                    end_time: None,
                },
                location: None,
                weight_percent: None,
                origin: ItemOrigin::Moodle,
                confidence: Confidence::Confirmed,
                evidence: vec![evidence],
            });
        }
    }

    // 2. Calendar events
    for event in &snapshot.events {
        // Skip assignment events because they duplicate assignments
        if event.modname.as_deref() == Some("assign") {
            continue;
        }

        let kind = if event.modname.as_deref() == Some("quiz") {
            ItemKind::Quiz
        } else {
            // Check kind hint from event title
            let hits = find_dates(&event.name, snapshot.fetched_at, None);
            hits.iter()
                .find_map(|h| h.kind_hint)
                .unwrap_or_else(|| infer_kind_from_name(&event.name))
        };

        let local_starts = event.starts.with_timezone(&Lisbon);
        let date = local_starts.date_naive();
        let time = Some(local_starts.time());

        let (end_date, end_time) = if event.duration_seconds > 0 {
            let local_end = (event.starts + chrono::Duration::seconds(event.duration_seconds))
                .with_timezone(&Lisbon);
            let end_d = if local_end.date_naive() != date {
                Some(local_end.date_naive())
            } else {
                None
            };
            (end_d, Some(local_end.time()))
        } else {
            (None, None)
        };

        let mut title = event.name.clone();
        if title.len() > 120 {
            let mut cut = 120;
            while !title.is_char_boundary(cut) && cut > 0 {
                cut -= 1;
            }
            title.truncate(cut);
        }

        let mut quote = event.name.clone();
        if quote.len() > MAX_QUOTE_CHARS {
            let mut cut = MAX_QUOTE_CHARS;
            while !quote.is_char_boundary(cut) && cut > 0 {
                cut -= 1;
            }
            quote.truncate(cut);
        }

        let id = item_id(course_id, kind, date, &title);
        let evidence = Evidence {
            doc_id: format!("moodle:event:{}", event.id),
            source: DocSource::Event { event_id: event.id },
            label: title.clone(),
            quote,
        };

        items.push(AcademicItem {
            id,
            course_id,
            kind,
            title,
            when: ItemWhen {
                date,
                time,
                end_date,
                end_time,
            },
            location: event.location.clone(),
            weight_percent: None,
            origin: ItemOrigin::Moodle,
            confidence: Confidence::Confirmed,
            evidence: vec![evidence],
        });
    }

    items
}

/// Fallback item kind inference from title text keywords.
fn infer_kind_from_name(name: &str) -> ItemKind {
    let lower = name.to_lowercase();
    if lower.contains("exame") || lower.contains("recurso") || lower.contains("exam") {
        ItemKind::Exam
    } else if lower.contains("teste")
        || lower.contains("frequência")
        || lower.contains("frequencia")
        || lower.contains("midterm")
    {
        ItemKind::Test
    } else if lower.contains("entrega")
        || lower.contains("prazo")
        || lower.contains("submissão")
        || lower.contains("submissao")
        || lower.contains("deadline")
        || lower.contains("due")
    {
        ItemKind::Deadline
    } else if lower.contains("apresentação")
        || lower.contains("apresentacao")
        || lower.contains("defesa")
    {
        ItemKind::Presentation
    } else if lower.contains("quiz") || lower.contains("questionário") {
        ItemKind::Quiz
    } else if lower.contains("laboratório")
        || lower.contains("laboratorio")
        || lower.contains("prática")
        || lower.contains("pratica")
    {
        ItemKind::Lab
    } else if lower.contains("cancelada")
        || lower.contains("adiada")
        || lower.contains("reposição")
        || lower.contains("reposicao")
    {
        ItemKind::ScheduleChange
    } else if lower.contains("inscrição") || lower.contains("inscricao") {
        ItemKind::Registration
    } else {
        ItemKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn test_assignment_and_events_conversion() {
        let course = Course {
            id: 10,
            short_name: "BD".into(),
            full_name: "Bases de Dados".into(),
            start: None,
            end: None,
            visible: true,
            modified: None,
        };

        // 2026-05-15 13:00:00 UTC is 14:00:00 Western European Summer Time (WEST / Lisbon UTC+1)
        let due_utc = Utc.with_ymd_and_hms(2026, 5, 15, 13, 0, 0).unwrap();

        let assign = Assignment {
            id: 1,
            module_id: 101,
            course_id: 10,
            name: "Entrega Trabalho 1".into(),
            intro_html: "<p>Instruções</p>".into(),
            opens: None,
            due: Some(due_utc),
            cutoff: None,
            attachments: vec![],
        };

        let ev_quiz = CalendarEvent {
            id: 2,
            name: "Quiz de Revisões".into(),
            description_html: "".into(),
            course_id: Some(10),
            module_id: None,
            event_type: "course".into(),
            modname: Some("quiz".into()),
            starts: due_utc,
            duration_seconds: 3600,
            location: Some("Sala 201".into()),
        };

        let ev_dup_assign = CalendarEvent {
            id: 3,
            name: "Entrega Trabalho 1".into(),
            description_html: "".into(),
            course_id: Some(10),
            module_id: None,
            event_type: "due".into(),
            modname: Some("assign".into()),
            starts: due_utc,
            duration_seconds: 0,
            location: None,
        };

        let snapshot = CourseSnapshot {
            course,
            sections: vec![],
            assignments: vec![assign],
            submissions: vec![],
            events: vec![ev_quiz, ev_dup_assign],
            announcements: vec![],
            grades: vec![],
            fetched_at: Utc::now(),
        };

        let items = items_from_snapshot(&snapshot);
        assert_eq!(items.len(), 2); // Duplicate assign event skipped!

        let deadline = items.iter().find(|i| i.kind == ItemKind::Deadline).unwrap();
        assert_eq!(
            deadline.when.date,
            chrono::NaiveDate::from_ymd_opt(2026, 5, 15).unwrap()
        );
        // 13:00 UTC in Lisbon summer time is 14:00
        assert_eq!(
            deadline.when.time,
            Some(chrono::NaiveTime::from_hms_opt(14, 0, 0).unwrap())
        );
        assert_eq!(deadline.confidence, Confidence::Confirmed);
        assert_eq!(deadline.origin, ItemOrigin::Moodle);

        let quiz = items.iter().find(|i| i.kind == ItemKind::Quiz).unwrap();
        assert_eq!(
            quiz.when.time,
            Some(chrono::NaiveTime::from_hms_opt(14, 0, 0).unwrap())
        );
        assert_eq!(
            quiz.when.end_time,
            Some(chrono::NaiveTime::from_hms_opt(15, 0, 0).unwrap())
        );
        assert_eq!(quiz.location, Some("Sala 201".into()));
    }
}
