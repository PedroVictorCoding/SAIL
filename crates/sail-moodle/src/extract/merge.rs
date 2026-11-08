//! Item identity, corroboration and conflict resolution across multiple extractions.
//!
//! Merges academic items discovered across different documents, structured Moodle feeds,
//! and model predictions. Computes deterministic item IDs based on normalized titles,
//! corroborates claims across distinct documents, and detects scheduling conflicts.

use crate::contracts::{AcademicItem, Confidence, ItemKind, ItemOrigin};
use chrono::NaiveDate;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Compute a stable identifier for an academic item.
///
/// Hashes the course id, kind, date and a normalized title (lowercased, accents stripped,
/// ordinal phrases like `1.º` or `primeiro` normalized to digits, and punctuation removed).
pub fn item_id(course_id: i64, kind: ItemKind, date: NaiveDate, title: &str) -> String {
    let norm_title = normalize_title(title);
    let mut hasher = Sha256::new();
    let key = format!("{course_id}|{kind:?}|{date}|{norm_title}");
    hasher.update(key.as_bytes());
    let hash = hasher.finalize();
    let mut s = String::with_capacity(hash.len() * 2);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Normalize an item title for fuzzy identity and deduplication.
pub fn normalize_title(title: &str) -> String {
    let mut lower = title.to_lowercase();

    // Replace accents
    lower = strip_accents(&lower);

    // Normalize ordinals (Portuguese & English)
    let replacements = [
        ("1.º", "1"),
        ("1º", "1"),
        ("1.ª", "1"),
        ("1ª", "1"),
        ("primeiro", "1"),
        ("primeira", "1"),
        ("first", "1"),
        ("2.º", "2"),
        ("2º", "2"),
        ("2.ª", "2"),
        ("2ª", "2"),
        ("segundo", "2"),
        ("segunda", "2"),
        ("second", "2"),
        ("3.º", "3"),
        ("3º", "3"),
        ("3.ª", "3"),
        ("3ª", "3"),
        ("terceiro", "3"),
        ("terceira", "3"),
        ("third", "3"),
        ("4.º", "4"),
        ("4º", "4"),
        ("quarto", "4"),
        ("fourth", "4"),
    ];

    for (from, to) in replacements {
        lower = lower.replace(from, to);
    }

    // Keep only alphanumeric and whitespace
    let mut clean = String::with_capacity(lower.len());
    let mut in_space = false;
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            clean.push(ch);
            in_space = false;
        } else if ch.is_whitespace() && !in_space && !clean.is_empty() {
            clean.push(' ');
            in_space = true;
        }
    }

    clean.trim().to_string()
}

/// Strip diacritical marks from common Western European characters.
fn strip_accents(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// Return the most trusted origin between two origins.
///
/// In SAIL, `Moodle` is authoritative, followed by deterministic `Pattern`, followed by `Model`.
fn most_trusted_origin(a: ItemOrigin, b: ItemOrigin) -> ItemOrigin {
    match (a, b) {
        (ItemOrigin::Moodle, _) | (_, ItemOrigin::Moodle) => ItemOrigin::Moodle,
        (ItemOrigin::Pattern, _) | (_, ItemOrigin::Pattern) => ItemOrigin::Pattern,
        (ItemOrigin::Model, ItemOrigin::Model) => ItemOrigin::Model,
    }
}

/// Merge existing and incoming academic items.
///
/// Deduplicates items by item ID, merges evidence lists, updates confidence based on
/// multi-source corroboration, detects conflicts (same course, kind, normalized title with
/// different dates), and returns a deterministically ordered list sorted by date.
pub fn merge(existing: &[AcademicItem], incoming: &[AcademicItem]) -> Vec<AcademicItem> {
    // 1. Group items by exact ID (same course, kind, date, normalized title)
    let mut by_id: BTreeMap<String, AcademicItem> = BTreeMap::new();

    for item in existing.iter().chain(incoming.iter()) {
        if let Some(existing_item) = by_id.get_mut(&item.id) {
            // Merge evidence without duplicates (doc_id + quote)
            for ev in &item.evidence {
                let already_present = existing_item
                    .evidence
                    .iter()
                    .any(|e| e.doc_id == ev.doc_id && e.quote == ev.quote);
                if !already_present {
                    existing_item.evidence.push(ev.clone());
                }
            }

            // Keep the most trusted origin
            existing_item.origin = most_trusted_origin(existing_item.origin, item.origin);

            // Merge details: time, location, weight
            if existing_item.when.time.is_none() && item.when.time.is_some() {
                existing_item.when.time = item.when.time;
            }
            if existing_item.when.end_time.is_none() && item.when.end_time.is_some() {
                existing_item.when.end_time = item.when.end_time;
            }
            if existing_item.location.is_none() && item.location.is_some() {
                existing_item.location = item.location.clone();
            }
            if existing_item.weight_percent.is_none() && item.weight_percent.is_some() {
                existing_item.weight_percent = item.weight_percent;
            }
        } else {
            by_id.insert(item.id.clone(), item.clone());
        }
    }

    // 2. Compute corroboration confidence for each item
    for item in by_id.values_mut() {
        let has_moodle = item.origin == ItemOrigin::Moodle
            || item
                .evidence
                .iter()
                .any(|e| e.doc_id.starts_with("moodle:"));

        // Distinct non-empty doc_ids
        let distinct_doc_ids: BTreeSet<&str> = item
            .evidence
            .iter()
            .map(|e| e.doc_id.as_str())
            .filter(|id| !id.is_empty())
            .collect();

        if has_moodle || distinct_doc_ids.len() >= 2 {
            item.confidence = Confidence::Confirmed;
        } else if item.confidence < Confidence::Likely {
            item.confidence = Confidence::Likely;
        }
    }

    // 3. Detect conflicts: same course, kind, normalized title with different dates
    // Group by (course_id, kind, normalized_title)
    let mut title_groups: HashMap<(i64, ItemKind, String), Vec<String>> = HashMap::new();
    for (id, item) in &by_id {
        let key = (item.course_id, item.kind, normalize_title(&item.title));
        title_groups.entry(key).or_default().push(id.clone());
    }

    for ids in title_groups.values() {
        if ids.len() > 1 {
            // Check if they have different dates
            let dates: BTreeSet<NaiveDate> = ids
                .iter()
                .map(|id| by_id.get(id).unwrap().when.date)
                .collect();

            if dates.len() > 1 {
                // Conflict!
                // If any item has Moodle origin, Moodle wins; others become Uncertain.
                // Otherwise all become Uncertain.
                let has_moodle = ids
                    .iter()
                    .any(|id| by_id.get(id).unwrap().origin == ItemOrigin::Moodle);

                for id in ids {
                    let item = by_id.get_mut(id).unwrap();
                    if has_moodle {
                        if item.origin != ItemOrigin::Moodle {
                            item.confidence = Confidence::Uncertain;
                        }
                    } else {
                        item.confidence = Confidence::Uncertain;
                    }
                }
            }
        }
    }

    // 4. Deterministic sort: by date, then time, then title, then id
    let mut result: Vec<AcademicItem> = by_id.into_values().collect();
    result.sort_by(|a, b| {
        a.when
            .date
            .cmp(&b.when.date)
            .then_with(|| a.when.time.cmp(&b.when.time))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{DocSource, Evidence, ItemWhen};

    fn make_test_item(
        id_suffix: &str,
        date: NaiveDate,
        title: &str,
        origin: ItemOrigin,
        doc_id: &str,
    ) -> AcademicItem {
        let id = item_id(10, ItemKind::Test, date, title);
        AcademicItem {
            id,
            course_id: 10,
            kind: ItemKind::Test,
            title: title.to_string(),
            when: ItemWhen {
                date,
                time: None,
                end_date: None,
                end_time: None,
            },
            location: None,
            weight_percent: None,
            origin,
            confidence: Confidence::Likely,
            evidence: vec![Evidence {
                doc_id: doc_id.to_string(),
                source: DocSource::Announcement { discussion_id: 1 },
                label: "Avisos".into(),
                quote: format!("{title} {id_suffix}"),
            }],
        }
    }

    #[test]
    fn test_corroboration_confirms_item() {
        let d = NaiveDate::from_ymd_opt(2026, 12, 10).unwrap();
        let it1 = make_test_item("A", d, "1.º Teste", ItemOrigin::Pattern, "doc_1");
        let it2 = make_test_item("B", d, "1º Teste", ItemOrigin::Model, "doc_2");

        assert_eq!(it1.id, it2.id);

        let merged = merge(&[it1], &[it2]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].confidence, Confidence::Confirmed);
        assert_eq!(merged[0].origin, ItemOrigin::Pattern); // Pattern is more trusted than Model
        assert_eq!(merged[0].evidence.len(), 2);
    }

    #[test]
    fn test_conflict_resolution() {
        let d1 = NaiveDate::from_ymd_opt(2026, 12, 10).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 12, 12).unwrap();

        let it_moodle = make_test_item("M", d1, "1.º Teste", ItemOrigin::Moodle, "moodle:event:1");
        let it_model = make_test_item("N", d2, "1.º Teste", ItemOrigin::Model, "doc_announcement");

        let merged = merge(&[it_moodle], &[it_model]);
        assert_eq!(merged.len(), 2);

        let moodle_res = merged.iter().find(|i| i.when.date == d1).unwrap();
        let model_res = merged.iter().find(|i| i.when.date == d2).unwrap();

        assert_eq!(moodle_res.confidence, Confidence::Confirmed);
        assert_eq!(model_res.confidence, Confidence::Uncertain);
    }
}
