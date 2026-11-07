//! PDF text extraction with explicit uncertainty and no OCR or password handling.
use super::*;
use lopdf::{Document, Object, ObjectId};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

/// Bound PDF object graph work separately from uploaded bytes.
pub const MAX_PDF_OBJECTS: usize = 100_000;
/// Bound each decoded stream before passing it to the text extractor.
pub const MAX_PDF_STREAM_BYTES: usize = 16 * 1024 * 1024;

fn invalid() -> IngestError {
    IngestError::Invalid("malformed or unsupported PDF structure".into())
}
fn resolve<'a>(doc: &'a Document, object: &'a Object) -> Result<&'a Object, IngestError> {
    let mut current = object;
    for _ in 0..MAX_DEPTH {
        match current {
            Object::Reference(id) => {
                current = doc.objects.get(id).ok_or_else(invalid)?;
            }
            _ => return Ok(current),
        }
    }
    Err(IngestError::Limit("PDF references"))
}
fn string(object: &Object) -> String {
    let Ok(bytes) = object.as_str() else {
        return String::new();
    };
    if bytes.starts_with(&[0xfe, 0xff]) {
        String::from_utf16_lossy(
            &bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}
// lopdf's loader can decrypt automatically. Conservatively inspect decoded PDF names first,
// even if they occur in stream bytes; false positives are safer than attempted decryption.
fn preflight(bytes: &[u8]) -> Result<(), IngestError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(IngestError::Limit("PDF bytes"));
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err(invalid());
    }
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'/' {
            index += 1;
            continue;
        }
        index += 1;
        let mut name = Vec::new();
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && !b"/[]<>(){}%".contains(&bytes[index])
        {
            if name.len() >= 128 {
                break;
            }
            if bytes[index] == b'#' && index + 2 < bytes.len() {
                let hex = |b: u8| (b as char).to_digit(16);
                if let (Some(a), Some(b)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    name.push((a * 16 + b) as u8);
                    index += 3;
                    continue;
                }
            }
            name.push(bytes[index]);
            index += 1;
        }
        if name == b"Encrypt" {
            return Err(IngestError::Drm(
                "encrypted PDF; no decryption attempted".into(),
            ));
        }
        // These are decompressed inside lopdf::load_mem before we can apply an output cap.
        if name == b"ObjStm" || name == b"XRef" || name == b"Prev" {
            return Err(IngestError::Invalid(
                "compressed/incremental PDF structures need a bounded runner adapter".into(),
            ));
        }
    }
    Ok(())
}
fn bounded_streams(doc: &mut Document) -> Result<(), IngestError> {
    if doc.objects.len() > MAX_PDF_OBJECTS {
        return Err(IngestError::Limit("PDF objects"));
    }
    let mut total = 0;
    for object in doc.objects.values_mut() {
        let Object::Stream(stream) = object else {
            continue;
        };
        // Images are never decoded here; OCR is unavailable.
        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image") {
            continue;
        }
        let filters: Vec<&[u8]> = match stream.dict.get(b"Filter") {
            Ok(Object::Name(name)) => vec![name],
            Ok(Object::Array(names)) => names
                .iter()
                .map(Object::as_name)
                .collect::<Result<_, _>>()
                .map_err(|_| invalid())?,
            Ok(_) => return Err(invalid()),
            Err(_) => Vec::new(),
        };
        if filters.len() > 1 {
            return Err(IngestError::Invalid(
                "PDF filter chains need bounded runner extraction".into(),
            ));
        }
        let content = match filters.first().copied() {
            None => {
                if stream.content.len() > MAX_PDF_STREAM_BYTES {
                    return Err(IngestError::Limit("PDF stream bytes"));
                }
                stream.content.clone()
            }
            Some(b"FlateDecode" | b"Fl") => {
                // Predictors apply mostly to images; refuse them for text rather than corrupting it.
                if stream
                    .dict
                    .get(b"DecodeParms")
                    .is_ok_and(|v| !matches!(v, Object::Null))
                {
                    return Err(IngestError::Invalid(
                        "PDF predictors need bounded runner extraction".into(),
                    ));
                }
                let mut out = Vec::new();
                flate2::read::ZlibDecoder::new(stream.content.as_slice())
                    .take(MAX_PDF_STREAM_BYTES as u64 + 1)
                    .read_to_end(&mut out)
                    .map_err(|_| invalid())?;
                out
            }
            _ => return Err(IngestError::Invalid("unsupported PDF stream filter".into())),
        };
        if content.len() > MAX_PDF_STREAM_BYTES {
            return Err(IngestError::Limit("PDF stream bytes"));
        }
        total += content.len();
        if total > MAX_EXTRACTED_BYTES {
            return Err(IngestError::Limit("decoded PDF bytes"));
        }
        stream.set_content(content);
        stream.dict.remove(b"Filter");
        stream.dict.remove(b"DecodeParms");
    }
    Ok(())
}
fn pages(doc: &Document) -> Result<BTreeMap<u32, ObjectId>, IngestError> {
    let catalog = doc.catalog().map_err(|_| invalid())?;
    let root = catalog
        .get(b"Pages")
        .and_then(Object::as_reference)
        .map_err(|_| invalid())?;
    let mut pending = vec![(root, 0)];
    let mut seen = BTreeSet::new();
    let mut result = BTreeMap::new();
    while let Some((id, depth)) = pending.pop() {
        if depth >= MAX_DEPTH || seen.len() >= MAX_PDF_OBJECTS {
            return Err(IngestError::Limit("PDF page tree"));
        }
        if !seen.insert(id) {
            return Err(invalid());
        }
        let dict = doc.get_dictionary(id).map_err(|_| invalid())?;
        if dict.has_type(b"Page") {
            if result.len() >= MAX_PAGES {
                return Err(IngestError::Limit("PDF pages"));
            }
            result.insert(result.len() as u32 + 1, id);
        } else if dict.has_type(b"Pages") {
            let kids = dict
                .get(b"Kids")
                .and_then(Object::as_array)
                .map_err(|_| invalid())?;
            if pending.len() + kids.len() > MAX_PDF_OBJECTS {
                return Err(IngestError::Limit("PDF page tree"));
            }
            for child in kids.iter().rev() {
                pending.push((child.as_reference().map_err(|_| invalid())?, depth + 1));
            }
        } else {
            return Err(invalid());
        }
    }
    if result.is_empty() {
        return Err(invalid());
    }
    // Validate Parent chains used by lopdf resource lookup, independently of Kids traversal.
    for id in result.values() {
        let mut current = *id;
        let mut ancestors = BTreeSet::new();
        for depth in 0..=MAX_DEPTH {
            if depth == MAX_DEPTH || !ancestors.insert(current) {
                return Err(IngestError::Limit("PDF parent chain"));
            }
            let dict = doc.get_dictionary(current).map_err(|_| invalid())?;
            match dict.get(b"Parent") {
                Ok(parent) => current = parent.as_reference().map_err(|_| invalid())?,
                Err(_) => break,
            }
        }
    }
    Ok(result)
}
fn outlines(
    doc: &Document,
    pages: &BTreeMap<u32, ObjectId>,
) -> Result<BTreeMap<u32, Vec<String>>, IngestError> {
    let catalog = doc.catalog().map_err(|_| invalid())?;
    let Ok(root) = catalog.get(b"Outlines") else {
        return Ok(BTreeMap::new());
    };
    let root = resolve(doc, root)?.as_dict().map_err(|_| invalid())?;
    let Ok(first) = root.get(b"First") else {
        return Ok(BTreeMap::new());
    };
    let mut tasks = vec![(first.clone(), Vec::<String>::new())];
    let mut seen = BTreeSet::new();
    let mut map = BTreeMap::new();
    let mut count = 0;
    while let Some((object, mut path)) = tasks.pop() {
        count += 1;
        if count > MAX_FRAGMENTS || path.len() >= MAX_DEPTH {
            return Err(IngestError::Limit("PDF outlines"));
        }
        if let Object::Reference(id) = &object
            && !seen.insert(*id)
        {
            return Err(invalid());
        }
        let dict = resolve(doc, &object)?.as_dict().map_err(|_| invalid())?;
        if let Ok(next) = dict.get(b"Next") {
            tasks.push((next.clone(), path.clone()));
        }
        let title = dict.get(b"Title").map(string).unwrap_or_default();
        if title.len() > MAX_FRAGMENT_BYTES {
            return Err(IngestError::Limit("outline title"));
        }
        if !title.is_empty() {
            path.push(title);
        }
        let destination = dict.get(b"Dest").ok().or_else(|| {
            dict.get(b"A")
                .ok()
                .and_then(|v| resolve(doc, v).ok())
                .and_then(|v| v.as_dict().ok())
                .and_then(|a| a.get(b"D").ok())
        });
        if let Some(dest) = destination {
            let dest = resolve(doc, dest)?;
            // Named destinations are resolved from legacy catalog Dests when available.
            let dest = if matches!(dest, Object::Name(_) | Object::String(_, _)) {
                let name = dest
                    .as_name()
                    .or_else(|_| dest.as_str())
                    .map_err(|_| invalid())?;
                catalog
                    .get(b"Dests")
                    .ok()
                    .and_then(|v| resolve(doc, v).ok())
                    .and_then(|v| v.as_dict().ok())
                    .and_then(|d| d.get(name).ok())
                    .and_then(|v| resolve(doc, v).ok())
            } else {
                Some(dest)
            };
            if let Some(dest) = dest {
                let dest = if let Object::Dictionary(d) = dest {
                    d.get(b"D").map_err(|_| invalid())?
                } else {
                    dest
                };
                if let Ok(array) = dest.as_array()
                    && let Some(Object::Reference(id)) = array.first()
                    && let Some((number, _)) = pages.iter().find(|(_, page)| *page == id)
                {
                    map.insert(*number, path.clone());
                }
            }
        }
        if let Ok(first) = dict.get(b"First") {
            tasks.push((first.clone(), path));
        }
    }
    Ok(map)
}
fn roman(mut value: u32, upper: bool) -> String {
    let mut text = String::new();
    // PDF label starts are bounded so this loop cannot create unbounded labels.
    for (n, letters) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while value >= n {
            text.push_str(letters);
            value -= n;
        }
    }
    if upper {
        text.to_ascii_uppercase()
    } else {
        text
    }
}
fn labels(doc: &Document) -> Result<BTreeMap<u32, String>, IngestError> {
    let mut result = BTreeMap::new();
    let catalog = doc.catalog().map_err(|_| invalid())?;
    let Ok(root) = catalog.get(b"PageLabels") else {
        return Ok(result);
    };
    let mut tasks = vec![(root, 0)];
    let mut seen = BTreeSet::new();
    let mut definitions = BTreeMap::new();
    while let Some((node, depth)) = tasks.pop() {
        if depth >= MAX_DEPTH || seen.len() > MAX_PAGES {
            return Err(IngestError::Limit("PDF label tree"));
        }
        if let Object::Reference(id) = node
            && !seen.insert(*id)
        {
            return Err(invalid());
        }
        let dict = resolve(doc, node)?.as_dict().map_err(|_| invalid())?;
        if let Ok(nums) = dict.get(b"Nums").and_then(Object::as_array) {
            if nums.len() > MAX_PAGES * 2 || nums.len() % 2 != 0 {
                return Err(invalid());
            }
            for pair in nums.as_chunks::<2>().0.iter() {
                let start = pair[0].as_i64().map_err(|_| invalid())?;
                if !(0..MAX_PAGES as i64).contains(&start) {
                    return Err(invalid());
                }
                definitions.insert(
                    start as u32,
                    resolve(doc, &pair[1])?.as_dict().map_err(|_| invalid())?,
                );
            }
        }
        if let Ok(kids) = dict.get(b"Kids").and_then(Object::as_array) {
            if tasks.len() + kids.len() > MAX_PAGES {
                return Err(IngestError::Limit("PDF label nodes"));
            }
            for child in kids {
                tasks.push((child, depth + 1));
            }
        }
    }
    for page in 0..MAX_PAGES as u32 {
        if let Some((start, dict)) = definitions.range(..=page).next_back() {
            let first = dict.get(b"St").and_then(Object::as_i64).unwrap_or(1);
            if !(1..=100_000).contains(&first) {
                return Err(IngestError::Limit("PDF label number"));
            }
            let number = first as u32 + page - start;
            let prefix = dict.get(b"P").map(string).unwrap_or_default();
            if prefix.len() > 256 {
                return Err(IngestError::Limit("PDF label prefix"));
            }
            let suffix = match dict.get(b"S").and_then(Object::as_name).ok() {
                Some(b"r") => roman(number, false),
                Some(b"R") => roman(number, true),
                Some(b"a") | Some(b"A") => {
                    let upper = dict.get(b"S").and_then(Object::as_name).ok() == Some(b"A");
                    let letter =
                        (if upper { b'A' } else { b'a' } + ((number - 1) % 26) as u8) as char;
                    std::iter::repeat_n(letter, ((number - 1) / 26 + 1) as usize).collect()
                }
                Some(b"D") => number.to_string(),
                None => String::new(),
                _ => return Err(invalid()),
            };
            result.insert(page + 1, format!("{prefix}{suffix}"));
        }
    }
    Ok(result)
}

fn has_visual_content(doc: &Document, page_id: ObjectId) -> Result<bool, IngestError> {
    let (inline, inherited) = doc.get_page_resources(page_id).map_err(|_| invalid())?;
    let mut resources = Vec::new();
    if let Some(dict) = inline {
        resources.push(dict);
    }
    for id in inherited {
        resources.push(doc.get_dictionary(id).map_err(|_| invalid())?);
    }
    for resource in resources {
        let Ok(objects) = resource.get(b"XObject") else {
            continue;
        };
        let objects = resolve(doc, objects)?.as_dict().map_err(|_| invalid())?;
        if objects.len() > MAX_PDF_OBJECTS {
            return Err(IngestError::Limit("page visual objects"));
        }
        for (_, object) in objects.iter() {
            let object = resolve(doc, object)?;
            let dict = match object {
                Object::Stream(stream) => &stream.dict,
                Object::Dictionary(dict) => dict,
                _ => return Err(invalid()),
            };
            if matches!(
                dict.get(b"Subtype").and_then(Object::as_name).ok(),
                Some(b"Image" | b"Form")
            ) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Extract text-layer pages; empty pages remain explicit unvalidated scan placeholders.
pub fn extract(bytes: &[u8], source: &str, revision: u64) -> Result<Extracted, IngestError> {
    preflight(bytes)?;
    let mut doc = Document::load_mem(bytes).map_err(|_| invalid())?;
    if doc.is_encrypted() {
        return Err(IngestError::Drm("encrypted PDF".into()));
    }
    bounded_streams(&mut doc)?;
    let pages = pages(&doc)?;
    for id in pages.values() {
        let page = doc.get_dictionary(*id).map_err(|_| invalid())?;
        if let Ok(contents) = page.get(b"Contents") {
            let resolved = resolve(&doc, contents)?;
            let parts: Vec<&Object> = match resolved {
                Object::Array(items) => items.iter().collect(),
                Object::Stream(_) => vec![resolved],
                _ => return Err(invalid()),
            };
            if parts.len() > MAX_FRAGMENTS {
                return Err(IngestError::Limit("page content streams"));
            }
            let mut bytes = 0usize;
            for part in parts {
                let stream = resolve(&doc, part)?.as_stream().map_err(|_| invalid())?;
                bytes = bytes
                    .checked_add(stream.content.len())
                    .ok_or(IngestError::Limit("page content bytes"))?;
                if bytes > MAX_PDF_STREAM_BYTES {
                    return Err(IngestError::Limit("page content bytes"));
                }
            }
        }
    }
    let toc = outlines(&doc, &pages)?;
    let labels = labels(&doc)?;
    let mut builder = Builder::new(source, revision);
    let mut path = Vec::new();
    let mut blank = 0;
    let mut errors = 0;
    let mut visual_pages = 0;
    for physical in pages.keys() {
        let location = SourceLocation::Page {
            page: labels
                .get(physical)
                .cloned()
                .unwrap_or_else(|| physical.to_string()),
            physical: *physical,
        };
        let has_outline = !toc.is_empty();
        if let Some(chapters) = toc.get(physical) {
            path.clone_from(chapters);
        }
        let text = match doc.extract_text(&[*physical]) {
            Ok(text) => text,
            Err(_) => {
                errors += 1;
                String::new()
            }
        };
        if text.len() > MAX_PDF_STREAM_BYTES {
            return Err(IngestError::Limit("page text bytes"));
        }
        if text.trim().is_empty() {
            blank += 1;
            builder.push(
                "[page has no usable text; OCR not available]",
                FragmentKind::Figure,
                &path,
                location,
                QualityStatus::NeedsValidation {
                    reason: "page may be scanned or blank; OCR not available".into(),
                },
            )?;
            continue;
        }
        if has_visual_content(&doc, pages[physical])? {
            visual_pages += 1;
            builder.push(
                "[page contains image/form content; visual validation required]",
                FragmentKind::Figure,
                &path,
                location.clone(),
                QualityStatus::NeedsValidation {
                    reason: "image/form text and formulas were not extracted; OCR not available"
                        .into(),
                },
            )?;
        }
        for paragraph in text.split('\n').filter(|s| !s.trim().is_empty()) {
            let trimmed = paragraph.trim();
            let guessed = !has_outline
                && trimmed.len() < 120
                && (trimmed.starts_with("Chapter ") || trimmed.starts_with("Section "));
            if guessed {
                path = vec![trimmed.into()];
            }
            builder.push(
                trimmed,
                if guessed {
                    FragmentKind::Heading
                } else {
                    classify(trimmed)
                },
                &path,
                location.clone(),
                QualityStatus::LowConfidence {
                    reason: if has_outline {
                        "PDF text layout and formulas need review"
                    } else {
                        "PDF heading hierarchy inferred without bookmarks"
                    }
                    .into(),
                },
            )?;
        }
    }
    let scanned = blank == pages.len() && errors == 0;
    let mut result = indexed(
        if scanned {
            SourceType::PdfScanned
        } else {
            SourceType::PdfText
        },
        builder.fragments,
    );
    if blank > 0 || visual_pages > 0 {
        result.status = IngestionStatus::NeedsReview {
            reason: if scanned {
                "scanned; OCR not available".into()
            } else {
                format!(
                    "{blank} pages lack usable text ({errors} extraction failures); {visual_pages} text pages contain visual content; OCR not available"
                )
            },
        };
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{Stream, dictionary};
    fn fixture(scanned: bool) -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let font = doc.add_object(
            dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier" },
        );
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut ids = Vec::new();
        for text in [
            "Definition: a pulse marks an event.",
            "Example: a bell emits a pulse.",
        ] {
            let operations = if scanned {
                Vec::new()
            } else {
                vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 12.into()]),
                    Operation::new("Tj", vec![Object::string_literal(text)]),
                    Operation::new("ET", vec![]),
                ]
            };
            let stream = doc.add_object(Stream::new(
                dictionary! {},
                Content { operations }.encode().unwrap(),
            ));
            ids.push(doc.add_object(
                dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => stream },
            ));
        }
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(), "Count" => 2,
            "Resources" => resources }));
        let outlines = doc.new_object_id();
        let second = doc.add_object(dictionary! { "Title" => Object::string_literal("Applications"), "Parent" => outlines, "Dest" => vec![Object::Reference(ids[1]), "Fit".into()] });
        let first = doc.add_object(dictionary! { "Title" => Object::string_literal("Signals"), "Parent" => outlines, "Next" => second, "Dest" => vec![Object::Reference(ids[0]), "Fit".into()] });
        doc.objects.insert(outlines, Object::Dictionary(dictionary! { "Type" => "Outlines", "First" => first, "Last" => second, "Count" => 2 }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id, "Outlines" => outlines,
            "PageLabels" => dictionary! { "Nums" => vec![0.into(), Object::Dictionary(dictionary! { "S" => "r" })] } });
        doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }
    #[test]
    fn chapters_labels_kinds_and_stability() {
        let bytes = fixture(false);
        let a = extract(&bytes, "s", 1).unwrap();
        assert_eq!(a.fragments, extract(&bytes, "s", 1).unwrap().fragments);
        assert_eq!(a.fragments[0].chapter_path, ["Signals"]);
        assert_eq!(a.fragments[1].chapter_path, ["Applications"]);
        assert_eq!(a.fragments[0].kind, FragmentKind::Definition);
        assert_eq!(
            a.fragments[1].location,
            SourceLocation::Page {
                page: "ii".into(),
                physical: 2
            }
        );
    }
    #[test]
    fn scanned_drm_truncation_and_compressed_objects() {
        let scanned = extract(&fixture(true), "s", 1).unwrap();
        assert_eq!(scanned.source_type, SourceType::PdfScanned);
        assert!(
            scanned
                .fragments
                .iter()
                .all(|f| matches!(f.quality, QualityStatus::NeedsValidation { .. }))
        );
        assert!(
            matches!(scanned.status, IngestionStatus::NeedsReview { ref reason } if reason == "scanned; OCR not available")
        );
        assert!(matches!(
            extract(b"%PDF-1.4 /Encr#79pt 1 0 R", "s", 1),
            Err(IngestError::Drm(_))
        ));
        assert!(extract(b"%PDF-1.4 /ObjStm", "s", 1).is_err());
        assert!(extract(b"%PDF-1.4 truncated", "s", 1).is_err());
    }

    #[test]
    fn cyclic_page_and_outline_graphs_are_errors() {
        let mut doc = Document::load_mem(&fixture(false)).unwrap();
        let root = doc
            .catalog()
            .unwrap()
            .get(b"Pages")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_dictionary_mut(root)
            .unwrap()
            .set("Kids", vec![Object::Reference(root)]);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        assert!(extract(&bytes, "s", 1).is_err());
        let mut doc = Document::load_mem(&fixture(false)).unwrap();
        let root = doc
            .catalog()
            .unwrap()
            .get(b"Outlines")
            .unwrap()
            .as_reference()
            .unwrap();
        let first = doc
            .get_dictionary(root)
            .unwrap()
            .get(b"First")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_dictionary_mut(first).unwrap().set("Next", first);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        assert!(extract(&bytes, "s", 1).is_err());
    }

    #[test]
    fn image_content_on_text_pages_requires_validation() {
        let mut doc = Document::load_mem(&fixture(false)).unwrap();
        let root = doc
            .catalog()
            .unwrap()
            .get(b"Pages")
            .unwrap()
            .as_reference()
            .unwrap();
        let resources = doc
            .get_dictionary(root)
            .unwrap()
            .get(b"Resources")
            .unwrap()
            .as_reference()
            .unwrap();
        let image = doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Filter" => "DCTDecode" },
            vec![0, 1, 2],
        ));
        doc.get_dictionary_mut(resources)
            .unwrap()
            .set("XObject", dictionary! { "Diagram" => image });
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let result = extract(&bytes, "s", 1).unwrap();
        assert_eq!(result.source_type, SourceType::PdfText);
        assert!(matches!(result.status, IngestionStatus::NeedsReview { .. }));
        assert_eq!(result.fragments[0].kind, FragmentKind::Figure);
        assert!(matches!(
            result.fragments[0].quality,
            QualityStatus::NeedsValidation { .. }
        ));
        assert_eq!(result.fragments[1].kind, FragmentKind::Definition);
    }
    #[test]
    #[ignore = "owner-provided Rust Book; counts only, no source text logged"]
    fn rust_book_quality_counts() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../course-content/rust");
        let file = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| {
                p.file_name().is_some_and(|n| {
                    n.to_string_lossy()
                        .starts_with("The Rust Programming Language")
                        && n.to_string_lossy().ends_with(".pdf")
                })
            })
            .unwrap();
        let bytes = std::fs::read(file).unwrap();
        // Candidate evaluation only: this ignored owner-file test deliberately measures
        // lopdf directly. Production refuses its unbounded object/xref stream loader.
        let doc = Document::load_mem(&bytes).unwrap();
        eprintln!("candidate_loaded objects={}", doc.objects.len());
        let pages = pages(&doc).unwrap();
        assert!(pages.len() <= MAX_PAGES);
        let chapters = outlines(&doc, &pages).unwrap().len();
        eprintln!("candidate_loaded pages={} chapters={chapters}", pages.len());
        // Sample evenly across the whole book instead of spending an unbounded debug run
        // decoding the same large font maps on every page.
        const SAMPLE_PAGES: usize = 20;
        let stride = (pages.len() / SAMPLE_PAGES).max(1);
        let mut sampled = 0;
        let mut fragments = 0usize;
        let mut failures = 0usize;
        let mut kinds = BTreeMap::new();
        for page in pages.keys().step_by(stride).take(SAMPLE_PAGES) {
            sampled += 1;
            match doc.extract_text(&[*page]) {
                Ok(text) => {
                    assert!(text.len() <= MAX_PDF_STREAM_BYTES);
                    for part in text.lines().filter(|line| !line.trim().is_empty()) {
                        fragments += 1;
                        *kinds
                            .entry(format!("{:?}", classify(part.trim())))
                            .or_insert(0usize) += 1;
                    }
                }
                Err(_) => failures += 1,
            }
        }
        eprintln!(
            "candidate_only pages={} sampled_pages={sampled} chapters={chapters} fragments={fragments} failures={failures} kinds={kinds:?}",
            pages.len()
        );
    }
}
