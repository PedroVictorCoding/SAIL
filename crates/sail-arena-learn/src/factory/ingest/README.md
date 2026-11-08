# Source ingestion integration

`SourceRegistry::new(Connection, blob_dir)` owns its connection behind a mutex. The
`open(db_path, blob_dir)` and `open_in_memory(blob_dir)` helpers choose no global paths.
All tables use `learn_factory_sources_*`; pass the existing Arena Learn database path.
The caller remains responsible for authorization and choosing mounted local storage.

`import(file_name, bytes, title, authors, rights, scope)` extracts and stores atomically
in SQLite after persisting the SHA-256-addressed blob. Duplicate active bytes return
the existing record without changing rights or scope. `replace(id, file_name, bytes)`
preserves metadata and increments revision; identical active bytes are a no-op.
`remove(id)` tombstones the current record. Historical metadata/fragments and blobs
are retained locally for invalidation/history, so removal is not physical erasure.
A crash between blob persistence and database commit can leave an unreferenced blob.

`FragmentLookup` resolves only active current revisions. `fragments(id)` supplies
reading-order retrieval data. `affected_by(id)` returns all known revisions, including
the current one, so the lead can construct invalidation before or after a mutation.
`list()` returns at most 1,000 sources; use `list_page(after_id, limit)` to continue.

The format parsers are pure. They never invoke a model, a process, a renderer or
network IO. Markdown/text use line anchors; EPUB uses spine href and element ids
(or stable block indices), with EPUB3 navigation or EPUB2 NCX hierarchy. Image text
remains `NeedsValidation`; no OCR is claimed. ZIP64, multi-volume ZIP, non-UTF-8 XML,
DTD/entity declarations and unsupported ZIP methods are rejected explicitly.

The classic-PDF path preserves direct/legacy bookmark destinations and page labels.
Its hierarchy and layout remain low-confidence; absent text and visual XObjects
produce unvalidated placeholders, never invented OCR text. Encrypted PDF names are
refused before loading, including escaped names. Because lopdf 0.38 can decompress
object/xref streams and authenticate passwords during loading, production refuses
compressed object/xref streams and incremental (`Prev`) structures. Filter chains,
non-Flate text streams and text predictors are also refused. Modern name-tree PDF
destinations are not implemented. Conservative name scanning can refuse names that
occur inside content. This path does **not** yet import the owner's Rust Book PDF.
A bounded `sail-runner` PDF adapter has been proposed to the lead in the handoff.

The ignored `rust_book_quality_counts` test evaluates lopdf directly on the owner's
fixture, independently of production acceptance. It samples at most 20 pages evenly
across the book and emits counts only. Run it under an external timeout; it does not
certify safe production loading or validate extracted formulas/layout.


## Resumable modern PDF extraction

`IngestError::NeedsSandbox` distinguishes unsupported structures/filters from malformed or
DRM-protected inputs. Imports/replacements preserve the blob and create a `Queued` revision
with no fragments. Jobs use the shared `contracts::PdfTextExtractor` port; the concrete
`RunnerPdfText` adapter is in Dialogue and requires the Guardian broker and runner setup
specified in `docs/SANDBOX_RUNTIME.md`. No new in-process modern-PDF loader is enabled.

`pdf_text::fragments_from_pages` handles a standalone <=20-page chunk. For sequential jobs,
use `fragments_from_pages_with_carry(..., &mut PdfCarry)` and persist the returned carry only
when the chunk commits. Fragment identities derive from source/revision/physical page/local
slot; changing chunk boundaries does not change ids. Printed labels are used when supplied.
Prose is split at paragraph boundaries; headings are heuristic and low-confidence. Empty
pages are explicit OCR-unavailable placeholders. NULs, oversized pages/chunks and invalid
ordering are refused.

`SourceRegistry::append_fragments(id, revision, fragments)` atomically appends an entire chunk
with identical retries as no-ops; conflicting retries and stale/removed revisions fail.
`set_status(id, revision, status)` protects status from stale workers as well. The caller
must serialize jobs per source revision and checkpoint carry after durable append; after a
crash it can replay the same chunk and carry safely. Existing per-source fragment/byte caps
still apply. A runner that is unavailable must leave work waiting/needs-review, never report
an empty successful import.
