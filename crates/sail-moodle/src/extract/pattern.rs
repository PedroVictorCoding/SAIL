//! Pattern-based academic item extraction from deterministic date mentions.
//!
//! Converts date mentions that carry a recognized [`ItemKind`] hint into [`AcademicItem`]
//! instances marked with [`ItemOrigin::Pattern`] and [`Confidence::Likely`], extracting
//! enclosing sentence quotes and preserving ordinal phrases in item titles.

use super::dates::DateMention;
use super::merge::item_id;
use crate::contracts::{
    AcademicItem, Confidence, ExtractionDoc, ItemKind, ItemOrigin, ItemWhen, MAX_QUOTE_CHARS,
};

/// Extract academic items from deterministic date mentions found in `doc`.
///
/// Only date mentions with a non-null `kind_hint` are converted. Item titles combine
/// the recognized kind with any nearby ordinal indicator (such as `1.º Teste`), and quotes
/// are extracted directly as verbatim substrings of `doc.text` bounded by sentence terminators
/// and [`MAX_QUOTE_CHARS`].
pub fn items_from_patterns(doc: &ExtractionDoc, hits: &[DateMention]) -> Vec<AcademicItem> {
    let mut items = Vec::new();

    for hit in hits {
        let kind = match hit.kind_hint {
            Some(k) => k,
            None => continue,
        };

        let title = extract_title_with_ordinal(&doc.text, hit, kind);
        let quote = extract_surrounding_quote(&doc.text, &hit.span);

        let id = item_id(doc.course_id, kind, hit.date, &title);
        let evidence = crate::contracts::Evidence {
            doc_id: doc.id.clone(),
            source: doc.source.clone(),
            label: doc.label.clone(),
            quote,
        };

        items.push(AcademicItem {
            id,
            course_id: doc.course_id,
            kind,
            title,
            when: ItemWhen {
                date: hit.date,
                time: hit.time,
                end_date: hit.end_date,
                end_time: hit.end_time,
            },
            location: None,
            weight_percent: None,
            origin: ItemOrigin::Pattern,
            confidence: Confidence::Likely,
            evidence: vec![evidence],
        });
    }

    items
}

/// Extract an item title containing the base kind name and any nearby ordinal phrase.
fn extract_title_with_ordinal(text: &str, hit: &DateMention, kind: ItemKind) -> String {
    let base_name = match kind {
        ItemKind::Exam => "Exame",
        ItemKind::Test => "Teste",
        ItemKind::Quiz => "Quiz",
        ItemKind::Deadline => "Entrega",
        ItemKind::Presentation => "Apresentação",
        ItemKind::Lab => "Laboratório",
        ItemKind::ScheduleChange => "Alteração de Horário",
        ItemKind::Registration => "Inscrição",
        ItemKind::Other => "Evento",
    };

    // Look around hit.span for ordinals: 1.º, 1º, 2.º, 2º, etc.
    let start = hit.span.start.saturating_sub(60);
    let end = (hit.span.end + 60).min(text.len());
    let mut window_start = start;
    while !text.is_char_boundary(window_start) {
        window_start = window_start.saturating_sub(1);
    }
    let mut window_end = end;
    while !text.is_char_boundary(window_end) {
        window_end = (window_end + 1).min(text.len());
    }

    // Keep only the date's own sentence: "Mini-teste a 19/11. Exame de época normal…" must
    // not title the mini-teste after the exam. Split on ". " and newlines, not on the dot
    // of "1.º".
    let mut window = &text[window_start..window_end];
    let date_at = hit.span.start - window_start;
    if let Some(cut) = window[..date_at]
        .rfind(". ")
        .or_else(|| window[..date_at].rfind('\n'))
    {
        window = &window[cut + 1..];
    }
    let date_at = hit.span.start - (window.as_ptr() as usize - text.as_ptr() as usize);
    let after_date = &window[date_at.min(window.len())..];
    if let Some(cut) = after_date.find(". ").or_else(|| after_date.find('\n')) {
        window = &window[..date_at + cut];
    }
    let base_name = if kind == ItemKind::Test && window.to_lowercase().contains("mini-teste") {
        "Mini-teste"
    } else {
        base_name
    };
    let ordinals = [
        ("1.º", "1.º"),
        ("1º", "1.º"),
        ("1.ª", "1.ª"),
        ("1ª", "1.ª"),
        ("primeiro", "1.º"),
        ("primeira", "1.ª"),
        ("1st", "1st"),
        ("2.º", "2.º"),
        ("2º", "2.º"),
        ("2.ª", "2.ª"),
        ("2ª", "2.ª"),
        ("segundo", "2.º"),
        ("segunda", "2.ª"),
        ("2nd", "2nd"),
        ("3.º", "3.º"),
        ("3º", "3.º"),
        ("3.ª", "3.ª"),
        ("3ª", "3.ª"),
        ("terceiro", "3.º"),
        ("terceira", "3.ª"),
        ("3rd", "3rd"),
        ("época normal", "Época Normal"),
        ("epoca normal", "Época Normal"),
        ("época de recurso", "Época de Recurso"),
        ("epoca de recurso", "Época de Recurso"),
        ("recurso", "Época de Recurso"),
        ("época especial", "Época Especial"),
        ("epoca especial", "Época Especial"),
    ];

    // Weekdays aren't ordinals ("segunda-feira" is Monday, "terça-feira" Tuesday).
    let lower = window.to_lowercase().replace("-feira", " feira");
    for (ord_key, ord_display) in ordinals {
        let is_season =
            ord_key.contains("época") || ord_key.contains("epoca") || ord_key.contains("recurso");
        if is_season && kind != ItemKind::Exam {
            continue;
        }
        let found = lower.match_indices(ord_key).any(|(at, m)| {
            let next = lower[at + m.len()..].chars().next().unwrap_or(' ');
            !(ord_key.chars().all(char::is_alphabetic) && next.is_alphabetic())
                && !lower[at + m.len()..].starts_with(" feira")
        });
        if found {
            if kind == ItemKind::Exam && ord_key.contains("recurso") {
                return "Exame - Época de Recurso".to_string();
            } else if kind == ItemKind::Exam && ord_key.contains("especial") {
                return "Exame - Época Especial".to_string();
            } else if kind == ItemKind::Exam && ord_key.contains("normal") {
                return "Exame - Época Normal".to_string();
            } else {
                return format!("{ord_display} {base_name}");
            }
        }
    }

    base_name.to_string()
}

/// Extract a sentence or bounded quote around `span` that is an exact substring of `text`.
fn extract_surrounding_quote(text: &str, span: &std::ops::Range<usize>) -> String {
    // 1. Find beginning of sentence or line
    let mut quote_start = span.start;
    let mut chars_before = 0;
    while quote_start > 0 && chars_before < 150 {
        let prev_idx = text[..quote_start]
            .char_indices()
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        let ch = text[prev_idx..].chars().next().unwrap_or('\n');
        if ch == '\n' || ch == '.' || ch == '!' || ch == '?' {
            // If punctuation, quote starts after it
            break;
        }
        quote_start = prev_idx;
        chars_before += 1;
    }

    // Advance quote_start past leading whitespace
    while quote_start < span.start {
        let ch = text[quote_start..].chars().next().unwrap_or('\n');
        if ch.is_whitespace() || ch == '.' || ch == '!' || ch == '?' {
            quote_start += ch.len_utf8();
        } else {
            break;
        }
    }

    // 2. Find end of sentence or line
    let mut quote_end = span.end;
    let mut chars_after = 0;
    while quote_end < text.len() && chars_after < 150 {
        let ch = text[quote_end..].chars().next().unwrap_or('\n');
        quote_end += ch.len_utf8();
        chars_after += 1;
        if ch == '\n' || ch == '.' || ch == '!' || ch == '?' {
            break;
        }
    }

    // Ensure within MAX_QUOTE_CHARS and valid UTF-8 boundary
    let mut raw_quote = &text[quote_start..quote_end];
    if raw_quote.chars().count() > MAX_QUOTE_CHARS {
        // Narrow window centered on span
        let target_len = MAX_QUOTE_CHARS;
        let span_len_chars = text[span.start..span.end].chars().count();
        let pad = (target_len.saturating_sub(span_len_chars)) / 2;

        let mut start_idx = span.start;
        for _ in 0..pad {
            if start_idx == 0 {
                break;
            }
            start_idx = text[..start_idx]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }

        let mut end_idx = span.end;
        for _ in 0..pad {
            if end_idx >= text.len() {
                break;
            }
            let ch_len = text[end_idx..].chars().next().unwrap_or('\n').len_utf8();
            end_idx += ch_len;
        }

        raw_quote = &text[start_idx..end_idx];
    }

    raw_quote.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(text: &str) -> Vec<String> {
        let doc = ExtractionDoc {
            id: "d".into(),
            course_id: 1,
            course_name: "C".into(),
            source: DocSource::SectionSummary { section_id: 1 },
            label: "l".into(),
            text: text.into(),
            observed_at: chrono::DateTime::from_timestamp(1_762_600_000, 0).unwrap(),
            term: None,
        };
        let hits = crate::extract::find_dates(text, doc.observed_at, None);
        items_from_patterns(&doc, &hits)
            .into_iter()
            .map(|i| i.title)
            .collect()
    }

    #[test]
    fn titles_stay_inside_their_sentence_and_skip_weekdays() {
        assert_eq!(
            titles("Mini-teste no dia 19/11/2026. Exame de época normal a 15 de janeiro de 2027."),
            vec!["Mini-teste", "Exame - Época Normal"]
        );
        assert_eq!(titles("Teste na segunda-feira, 16/11/2026."), vec!["Teste"]);
        assert_eq!(titles("O 2.º teste é a 14/12/2026."), vec!["2.º Teste"]);
    }
    use crate::contracts::{DocSource, ItemKind};
    use chrono::{NaiveDate, Utc};

    #[test]
    fn test_pattern_items_extraction() {
        let text = "Informamos que o 1.º Teste será realizado no dia 15 de novembro de 2026 às 10h na Sala B1.";
        let doc = ExtractionDoc {
            id: "doc123".into(),
            course_id: 42,
            course_name: "Matemática Discreta".into(),
            source: DocSource::Announcement { discussion_id: 5 },
            label: "Avisos".into(),
            text: text.into(),
            observed_at: Utc::now(),
            term: None,
        };

        let mention = DateMention {
            span: 49..74,
            date: NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
            time: Some(chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap()),
            end_date: None,
            end_time: None,
            kind_hint: Some(ItemKind::Test),
            raw: "15 de novembro de 2026 às 10h".into(),
        };

        let items = items_from_patterns(&doc, &[mention]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "1.º Teste");
        assert_eq!(items[0].kind, ItemKind::Test);
        assert_eq!(items[0].origin, ItemOrigin::Pattern);
        assert_eq!(items[0].confidence, Confidence::Likely);
        // Quote must be an exact substring of text
        assert!(text.contains(&items[0].evidence[0].quote));
    }
}
