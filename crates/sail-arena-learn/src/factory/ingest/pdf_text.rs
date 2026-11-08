//! Pure conversion of sandboxed pages into stable fragments. Page-local identities make
//! retrying or regrouping chunks safe; carry state is small enough for job checkpoints.
use super::*;
use crate::factory::contracts::{PDF_TEXT_PAGES_PER_CALL, PdfPageText};
use serde::{Deserialize, Serialize};

/// Reject pathological individual pages before splitting or allocating derived fragments.
pub const MAX_PAGE_TEXT_BYTES: usize = 128 * 1024;
/// Match the runner's complete text-output ceiling for one page chunk.
pub const MAX_CHUNK_TEXT_BYTES: usize = 128 * 1024;

/// Persist between sequential page jobs; heading guesses are never verified source structure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfCarry {
    /// Last inferred chapter/section path, bounded by the ordinary fragment limits.
    pub chapter_path: Vec<String>,
    /// Last completed physical page; zero starts extraction at any physical page.
    pub last_page: u32,
}

/// Convert a standalone chunk. Use the carry variant when continuing a prior chapter.
pub fn fragments_from_pages(
    source: &str,
    revision: u64,
    pages: &[PdfPageText],
) -> Result<Vec<SourceFragment>, IngestError> {
    fragments_from_pages_with_carry(source, revision, pages, &mut PdfCarry::default())
}

/// Update carry only after a complete, bounded chunk succeeds, so a failed job can retry.
pub fn fragments_from_pages_with_carry(
    source: &str,
    revision: u64,
    pages: &[PdfPageText],
    carry: &mut PdfCarry,
) -> Result<Vec<SourceFragment>, IngestError> {
    if source.is_empty() || source.len() > 256 || revision == 0 {
        return Err(IngestError::Invalid("invalid source identity".into()));
    }
    if pages.is_empty() || pages.len() > PDF_TEXT_PAGES_PER_CALL as usize {
        return Err(IngestError::Limit("PDF chunk pages"));
    }
    if carry.last_page as usize > MAX_PAGES
        || carry.chapter_path.len() > MAX_DEPTH
        || carry
            .chapter_path
            .iter()
            .any(|p| p.len() > MAX_FRAGMENT_BYTES || p.contains('\0'))
    {
        return Err(IngestError::Limit("PDF carry"));
    }
    let mut next = carry.clone();
    let mut result = Vec::new();
    let mut total = 0;
    for page in pages {
        if page.page == 0
            || page.page as usize > MAX_PAGES
            || (next.last_page != 0 && page.page != next.last_page + 1)
        {
            return Err(IngestError::Invalid(
                "PDF pages must be consecutive and in bounds".into(),
            ));
        }
        total += page.text.len();
        if page.text.len() > MAX_PAGE_TEXT_BYTES || total > MAX_CHUNK_TEXT_BYTES {
            return Err(IngestError::Limit("PDF page/chunk text bytes"));
        }
        if page.text.contains(['\0', '\u{c}'])
            || page
                .label
                .as_ref()
                .is_some_and(|l| l.len() > 256 || l.contains('\0'))
        {
            return Err(IngestError::Invalid(
                "invalid PDF page text or label".into(),
            ));
        }
        let location = SourceLocation::Page {
            page: page.label.clone().unwrap_or_else(|| page.page.to_string()),
            physical: page.page,
        };
        // Reset the Builder per page: fragment ids never depend on preceding chunk size.
        let mut builder = Builder::new(source, revision);
        if page.text.trim().is_empty() {
            builder.push(
                "[no text layer on this page; OCR not available]",
                FragmentKind::Figure,
                &next.chapter_path,
                location,
                QualityStatus::NeedsValidation {
                    reason: "no text layer on this page; OCR not available".into(),
                },
            )?;
        } else {
            let mut code = false;
            for paragraph in page.text.split("\n\n") {
                let mut pending = String::new();
                let mut pending_kind = FragmentKind::Prose;
                for line in paragraph.lines().map(str::trim).filter(|s| !s.is_empty()) {
                    let fence = line.starts_with("```");
                    let heading = !code && !fence && is_heading(line);
                    let structural = classify(line);
                    if fence || heading || structural != FragmentKind::Prose {
                        push_pending(
                            &mut builder,
                            &mut pending,
                            pending_kind,
                            &next.chapter_path,
                            &location,
                        )?;
                    }
                    if fence {
                        code = !code;
                    }
                    if heading {
                        if line.starts_with("Chapter ") || next.chapter_path.is_empty() {
                            next.chapter_path = vec![line.into()];
                        } else {
                            next.chapter_path.truncate(1);
                            next.chapter_path.push(line.into());
                        }
                    }
                    if pending.is_empty() {
                        pending_kind = if code || fence {
                            FragmentKind::Code
                        } else if heading {
                            FragmentKind::Heading
                        } else {
                            structural
                        };
                    } else {
                        pending.push('\n');
                    }
                    pending.push_str(line);
                    if heading || fence {
                        push_pending(
                            &mut builder,
                            &mut pending,
                            pending_kind,
                            &next.chapter_path,
                            &location,
                        )?;
                    }
                }
                push_pending(
                    &mut builder,
                    &mut pending,
                    pending_kind,
                    &next.chapter_path,
                    &location,
                )?;
            }
        }
        // Location labels and text are not identity: a page-local slot is stable across
        // different grouping/carry. The separate content_hash still verifies cited text.
        for (index, fragment) in builder.fragments.iter_mut().enumerate() {
            let identity = serde_json::to_vec(&(source, revision, page.page, index))
                .map_err(|_| IngestError::Invalid("PDF fragment identity".into()))?;
            fragment.fragment_id = sha256_hex(&identity);
        }
        result.extend(builder.fragments);
        if result.len() > MAX_FRAGMENTS {
            return Err(IngestError::Limit("PDF chunk fragments"));
        }
        next.last_page = page.page;
    }
    *carry = next;
    Ok(result)
}
fn push_pending(
    builder: &mut Builder,
    text: &mut String,
    kind: FragmentKind,
    path: &[String],
    location: &SourceLocation,
) -> Result<(), IngestError> {
    if !text.is_empty() {
        builder.push(text, kind, path, location.clone(), QualityStatus::LowConfidence {
            reason: "PDF text and inferred hierarchy need review; images/formulas not visually verified".into(),
        })?;
        text.clear();
    }
    Ok(())
}
fn is_heading(line: &str) -> bool {
    if line.len() > 120 || line.ends_with(['.', ':', ';', '?', '!']) {
        return false;
    }
    if line.starts_with("Chapter ") || line.starts_with("Section ") {
        return true;
    }
    let letters: Vec<char> = line.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() >= 3 && letters.iter().all(|c| c.is_uppercase()) {
        return true;
    }
    let words: Vec<&str> = line.split_whitespace().take(9).collect();
    (2..=8).contains(&words.len())
        && words
            .iter()
            .all(|w| w.chars().next().is_some_and(char::is_uppercase))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page(page: u32, text: &str) -> PdfPageText {
        PdfPageText {
            page,
            label: None,
            text: text.into(),
        }
    }
    #[test]
    fn identities_do_not_depend_on_chunking_and_carry_survives() {
        let pages = [
            page(1, "Chapter 1 Signals\nDefinition: a pulse is an event."),
            page(2, "Example: a bell emits a pulse."),
        ];
        let together = fragments_from_pages("s", 1, &pages).unwrap();
        let mut carry = PdfCarry::default();
        let mut separate =
            fragments_from_pages_with_carry("s", 1, &pages[..1], &mut carry).unwrap();
        separate.extend(fragments_from_pages_with_carry("s", 1, &pages[1..], &mut carry).unwrap());
        assert_eq!(together, separate);
        let alone = fragments_from_pages("s", 1, &pages[1..]).unwrap();
        assert_eq!(alone[0].fragment_id, together[2].fragment_id);
        assert_eq!(together[2].chapter_path, ["Chapter 1 Signals"]);
        assert_eq!(together[2].kind, FragmentKind::Example);
        assert_eq!(carry.last_page, 2);
    }
    #[test]
    fn labels_empty_pages_and_hostile_text() {
        let blank = PdfPageText {
            page: 7,
            label: Some("vii".into()),
            text: String::new(),
        };
        let f = fragments_from_pages("s", 1, &[blank]).unwrap();
        assert!(matches!(
            f[0].quality,
            QualityStatus::NeedsValidation { .. }
        ));
        assert_eq!(
            f[0].location,
            SourceLocation::Page {
                page: "vii".into(),
                physical: 7
            }
        );
        assert!(fragments_from_pages("s", 1, &[page(1, "bad\0text")]).is_err());
        assert!(fragments_from_pages("s", 1, &[page(1, &"a".repeat(10 * 1024 * 1024))]).is_err());
        assert!(fragments_from_pages("s", 1, &[page(2, "a"), page(1, "b")]).is_err());
        let mut carry = PdfCarry::default();
        assert!(
            fragments_from_pages_with_carry(
                "s",
                1,
                &[page(1, "Chapter One"), page(2, "bad\0")],
                &mut carry
            )
            .is_err()
        );
        assert_eq!(carry, PdfCarry::default());
    }
    #[test]
    #[ignore = "owner PDF host compatibility probe; counts only, not sandbox certification"]
    fn rust_book_poppler_quality_counts() {
        use std::io::Read;
        use std::process::{Command, Stdio};
        fn bounded(tool: &str, args: &[String]) -> Vec<u8> {
            let mut child = Command::new("/usr/bin/prlimit")
                .args([
                    "--as=536870912",
                    "--cpu=10",
                    "--fsize=33554432",
                    "--",
                    "/usr/bin/timeout",
                    "30",
                    tool,
                ])
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let out = std::thread::spawn(move || {
                let mut v = Vec::new();
                stdout
                    .take(MAX_PAGE_TEXT_BYTES as u64 + 1)
                    .read_to_end(&mut v)
                    .unwrap();
                v
            });
            let err = std::thread::spawn(move || {
                let mut v = Vec::new();
                stderr.take(32769).read_to_end(&mut v).unwrap();
                v
            });
            let status = child.wait().unwrap();
            let out = out.join().unwrap();
            let err = err.join().unwrap();
            assert!(
                status.success(),
                "host PDF probe failed (no source text logged)"
            );
            assert!(
                err.is_empty(),
                "host PDF probe emitted diagnostics (no source text logged)"
            );
            assert!(out.len() <= MAX_PAGE_TEXT_BYTES);
            out
        }
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../course-content/rust");
        let file = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| {
                p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with("The Rust Programming Language") && n.ends_with(".pdf")
                })
            })
            .unwrap();
        let path = file.to_string_lossy().into_owned();
        let info = String::from_utf8(bounded("/usr/bin/pdfinfo", std::slice::from_ref(&path)))
            .unwrap_or_else(|_| panic!("PDF metadata probe returned invalid UTF-8"));
        assert!(info.lines().any(|l| {
            l.strip_prefix("Encrypted:")
                .is_some_and(|s| s.trim() == "no")
        }));
        let pages: u32 = info
            .lines()
            .find_map(|l| l.strip_prefix("Pages:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pages as usize <= MAX_PAGES);
        let limit = std::env::var("SAIL_PDF_PROBE_PAGES")
            .ok()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(pages)
            .min(pages);
        let mut carry = PdfCarry::default();
        let mut kinds = std::collections::BTreeMap::new();
        let mut chapters = std::collections::BTreeSet::new();
        let mut empty = 0;
        for page in 1..=limit {
            let output = bounded(
                "/usr/bin/pdftotext",
                &[
                    "-f".into(),
                    page.to_string(),
                    "-l".into(),
                    page.to_string(),
                    "-layout".into(),
                    "-enc".into(),
                    "UTF-8".into(),
                    path.clone(),
                    "-".into(),
                ],
            );
            let text = String::from_utf8(output)
                .unwrap_or_else(|_| panic!("PDF probe returned invalid UTF-8"));
            let text = text.strip_suffix('\u{c}').unwrap();
            if text.trim().is_empty() {
                empty += 1;
            }
            let fragments = fragments_from_pages_with_carry(
                "probe",
                1,
                &[PdfPageText {
                    page,
                    label: None,
                    text: text.into(),
                }],
                &mut carry,
            )
            .unwrap();
            for f in fragments {
                *kinds.entry(format!("{:?}", f.kind)).or_insert(0usize) += 1;
                if !f.chapter_path.is_empty() {
                    chapters.insert(f.chapter_path);
                }
            }
            if page % 100 == 0 {
                eprintln!("poppler_probe pages_done={page} pages_total={pages}");
            }
        }
        let fragments: usize = kinds.values().sum();
        eprintln!(
            "poppler_probe pages_total={pages} pages_evaluated={limit} empty_pages={empty} fragments={fragments} distinct_inferred_paths={} kinds={kinds:?}",
            chapters.len()
        );
    }
    #[test]
    #[ignore = "owner PDF stdin compatibility probe; counts only, not sandbox certification"]
    fn rust_book_poppler_stdin_counts() {
        use std::io::{Read, Write};
        use std::process::{Command, Stdio};
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../course-content/rust");
        let file = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| {
                p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with("The Rust Programming Language") && n.ends_with(".pdf")
                })
            })
            .unwrap();
        assert!(file.metadata().unwrap().len() <= MAX_FILE_BYTES as u64);
        let bytes = std::fs::read(file).unwrap();
        let mut child = Command::new("/usr/bin/prlimit")
            .args([
                "--as=536870912",
                "--cpu=10",
                "--fsize=33554432",
                "--",
                "/usr/bin/timeout",
                "30",
                "/usr/bin/pdftotext",
                "-layout",
                "-enc",
                "UTF-8",
                "-eol",
                "unix",
                "-f",
                "1",
                "-l",
                "20",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let writer = std::thread::spawn(move || stdin.write_all(&bytes));
        let output = std::thread::spawn(move || {
            let mut v = Vec::new();
            stdout
                .take(MAX_CHUNK_TEXT_BYTES as u64 + 1)
                .read_to_end(&mut v)
                .unwrap();
            v
        });
        let diagnostics = std::thread::spawn(move || {
            let mut v = Vec::new();
            stderr.take(32769).read_to_end(&mut v).unwrap();
            v
        });
        let status = child.wait().unwrap();
        let wrote = writer.join().unwrap().is_ok();
        let output = output.join().unwrap();
        let diagnostics = diagnostics.join().unwrap();
        eprintln!(
            "poppler_stdin_probe success={} wrote_complete={wrote} text_bytes={} diagnostic_bytes={}",
            status.success(),
            output.len(),
            diagnostics.len()
        );
        assert!(status.success());
        assert!(wrote);
        assert!(diagnostics.is_empty());
        assert!(output.len() <= MAX_CHUNK_TEXT_BYTES);
        let text = String::from_utf8(output)
            .unwrap_or_else(|_| panic!("PDF probe returned invalid UTF-8"));
        let parts: Vec<_> = text.strip_suffix('\u{c}').unwrap().split('\u{c}').collect();
        assert_eq!(parts.len(), 20);
        let pages: Vec<_> = parts
            .iter()
            .enumerate()
            .map(|(i, text)| PdfPageText {
                page: i as u32 + 1,
                label: None,
                text: (*text).into(),
            })
            .collect();
        let fragments = fragments_from_pages("stdin-probe", 1, &pages).unwrap();
        eprintln!(
            "poppler_stdin_probe pages={} fragments={}",
            pages.len(),
            fragments.len()
        );
    }
}
