//! Extraction document synthesis from Moodle course snapshots and mirrored files.
//!
//! Converts raw HTML descriptions and briefs into structured [`ExtractionDoc`] instances,
//! chunks oversized texts along paragraph boundaries, and computes deterministic content IDs
//! using SHA-256 over source metadata and extracted text.

use super::text::html_to_text;
use crate::contracts::{
    Course, CourseSnapshot, DocSource, ExtractionDoc, FileRef, MAX_EXTRACTION_TEXT_BYTES,
};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

/// Maximum chunk size in bytes for extraction documents.
pub const CHUNK_SIZE_BYTES: usize = MAX_EXTRACTION_TEXT_BYTES;

/// Extract plain-text extraction documents from a course snapshot.
///
/// Creates documents for all non-empty:
/// - Section summaries
/// - Module descriptions
/// - Assignment briefs (`intro_html`)
/// - Announcements (`subject` + `message_html`)
pub fn docs_from_snapshot(snapshot: &CourseSnapshot) -> Vec<ExtractionDoc> {
    let mut docs = Vec::new();
    let term = match (snapshot.course.start, snapshot.course.end) {
        (Some(s), Some(e)) => Some((s.date_naive(), e.date_naive())),
        _ => None,
    };

    // 1. Sections
    for section in &snapshot.sections {
        if !section.summary_html.trim().is_empty() {
            let plain = html_to_text(&section.summary_html);
            if !plain.is_empty() {
                let chunks = chunk_text(&plain, CHUNK_SIZE_BYTES);
                for (chunk_idx, chunk) in chunks.into_iter().enumerate() {
                    let source = DocSource::SectionSummary {
                        section_id: section.id,
                    };
                    let label = if chunk_idx == 0 {
                        format!("Secção: {}", section.name)
                    } else {
                        format!("Secção: {} (parte {})", section.name, chunk_idx + 1)
                    };
                    let id = compute_doc_id(&source, &chunk);
                    docs.push(ExtractionDoc {
                        id,
                        course_id: snapshot.course.id,
                        course_name: snapshot.course.full_name.clone(),
                        source,
                        label,
                        text: chunk,
                        observed_at: snapshot.fetched_at,
                        term,
                    });
                }
            }
        }

        // 2. Modules inside section
        for module in &section.modules {
            if !module.description_html.trim().is_empty() {
                let plain = html_to_text(&module.description_html);
                if !plain.is_empty() {
                    let chunks = chunk_text(&plain, CHUNK_SIZE_BYTES);
                    for (chunk_idx, chunk) in chunks.into_iter().enumerate() {
                        let source = DocSource::ModuleDescription {
                            module_id: module.id,
                        };
                        let label = if chunk_idx == 0 {
                            module.name.clone()
                        } else {
                            format!("{} (parte {})", module.name, chunk_idx + 1)
                        };
                        let id = compute_doc_id(&source, &chunk);
                        docs.push(ExtractionDoc {
                            id,
                            course_id: snapshot.course.id,
                            course_name: snapshot.course.full_name.clone(),
                            source,
                            label,
                            text: chunk,
                            observed_at: snapshot.fetched_at,
                            term,
                        });
                    }
                }
            }
        }
    }

    // 3. Assignments
    for assignment in &snapshot.assignments {
        if !assignment.intro_html.trim().is_empty() {
            let plain = html_to_text(&assignment.intro_html);
            if !plain.is_empty() {
                let chunks = chunk_text(&plain, CHUNK_SIZE_BYTES);
                for (chunk_idx, chunk) in chunks.into_iter().enumerate() {
                    let source = DocSource::AssignmentBrief {
                        assignment_id: assignment.id,
                    };
                    let label = if chunk_idx == 0 {
                        format!("Trabalho: {}", assignment.name)
                    } else {
                        format!("Trabalho: {} (parte {})", assignment.name, chunk_idx + 1)
                    };
                    let id = compute_doc_id(&source, &chunk);
                    docs.push(ExtractionDoc {
                        id,
                        course_id: snapshot.course.id,
                        course_name: snapshot.course.full_name.clone(),
                        source,
                        label,
                        text: chunk,
                        observed_at: snapshot.fetched_at,
                        term,
                    });
                }
            }
        }
    }

    // 4. Announcements
    for announcement in &snapshot.announcements {
        let body = html_to_text(&announcement.message_html);
        let full_text = if body.is_empty() {
            announcement.subject.clone()
        } else {
            format!("{}\n\n{}", announcement.subject, body)
        };

        if !full_text.trim().is_empty() {
            let chunks = chunk_text(&full_text, CHUNK_SIZE_BYTES);
            for (chunk_idx, chunk) in chunks.into_iter().enumerate() {
                let source = DocSource::Announcement {
                    discussion_id: announcement.discussion_id,
                };
                let label = if chunk_idx == 0 {
                    format!("Avisos: {}", announcement.subject)
                } else {
                    format!("Avisos: {} (parte {})", announcement.subject, chunk_idx + 1)
                };
                let id = compute_doc_id(&source, &chunk);
                docs.push(ExtractionDoc {
                    id,
                    course_id: snapshot.course.id,
                    course_name: snapshot.course.full_name.clone(),
                    source,
                    label,
                    text: chunk,
                    observed_at: snapshot.fetched_at,
                    term,
                });
            }
        }
    }

    docs
}

/// Extract plain-text extraction documents from mirrored file text pages.
///
/// Pages are combined or chunked to stay within [`CHUNK_SIZE_BYTES`].
pub fn docs_from_file_text(
    course: &Course,
    file: &FileRef,
    sha256: &str,
    pages: &[(u32, String)],
    observed: DateTime<Utc>,
) -> Vec<ExtractionDoc> {
    let mut docs = Vec::new();
    let term = match (course.start, course.end) {
        (Some(s), Some(e)) => Some((s.date_naive(), e.date_naive())),
        _ => None,
    };

    let mut current_chunk = 0u32;
    for (page_num, page_text) in pages {
        let clean = page_text.trim();
        if clean.is_empty() {
            continue;
        }

        let chunks = chunk_text(clean, CHUNK_SIZE_BYTES);
        for chunk in chunks {
            let source = DocSource::File {
                key: file.key(),
                sha256: sha256.to_string(),
                chunk: current_chunk,
            };
            let label = format!("{} p. {}", file.filename, page_num);
            let id = compute_doc_id(&source, &chunk);

            docs.push(ExtractionDoc {
                id,
                course_id: course.id,
                course_name: course.full_name.clone(),
                source,
                label,
                text: chunk,
                observed_at: observed,
                term,
            });
            current_chunk += 1;
        }
    }

    docs
}

/// Compute a deterministic SHA-256 doc id from source JSON and text.
fn compute_doc_id(source: &DocSource, text: &str) -> String {
    let mut hasher = Sha256::new();
    let source_json = serde_json::to_string(source).unwrap_or_default();
    hasher.update(source_json.as_bytes());
    hasher.update(b"|");
    hasher.update(text.as_bytes());
    let hash = hasher.finalize();
    let mut s = String::with_capacity(hash.len() * 2);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Split text into chunks bounded by `max_bytes` along paragraph boundaries.
fn chunk_text(text: &str, max_bytes: usize) -> Vec<String> {
    if text.len() <= max_bytes {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::with_capacity(max_bytes);

    for paragraph in text.split("\n\n") {
        if paragraph.trim().is_empty() {
            continue;
        }

        if paragraph.len() > max_bytes {
            // Paragraph itself exceeds max_bytes, split on single newlines
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            for line in paragraph.lines() {
                if line.len() > max_bytes {
                    // Split line on whitespace
                    for word in line.split_whitespace() {
                        if !current.is_empty() && current.len() + 1 + word.len() > max_bytes {
                            chunks.push(std::mem::take(&mut current));
                        }
                        if !current.is_empty() {
                            current.push(' ');
                        }
                        current.push_str(word);
                    }
                } else {
                    if !current.is_empty() && current.len() + 1 + line.len() > max_bytes {
                        chunks.push(std::mem::take(&mut current));
                    }
                    if !current.is_empty() {
                        current.push('\n');
                    }
                    current.push_str(line);
                }
            }
        } else {
            if !current.is_empty() && current.len() + 2 + paragraph.len() > max_bytes {
                chunks.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(paragraph);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::*;

    #[test]
    fn test_docs_from_snapshot() {
        let course = Course {
            id: 101,
            short_name: "SO".into(),
            full_name: "Sistemas Operativos".into(),
            start: None,
            end: None,
            visible: true,
            modified: None,
        };

        let section = Section {
            id: 1,
            number: 1,
            name: "Geral".into(),
            summary_html: "<p>Bem-vindos à UC de Sistemas Operativos.</p>".into(),
            visible: true,
            modules: vec![],
        };

        let assignment = Assignment {
            id: 201,
            module_id: 10,
            course_id: 101,
            name: "Projeto 1".into(),
            intro_html: "<p>Entrega até 15 de maio.</p>".into(),
            opens: None,
            due: None,
            cutoff: None,
            attachments: vec![],
        };

        let snapshot = CourseSnapshot {
            course,
            sections: vec![section],
            assignments: vec![assignment],
            submissions: vec![],
            events: vec![],
            announcements: vec![],
            grades: vec![],
            fetched_at: Utc::now(),
        };

        let docs = docs_from_snapshot(&snapshot);
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].source, DocSource::SectionSummary { section_id: 1 });
        assert!(docs[0].text.contains("Sistemas Operativos"));
        assert_eq!(
            docs[1].source,
            DocSource::AssignmentBrief { assignment_id: 201 }
        );
        assert!(docs[1].text.contains("15 de maio"));
    }
}
