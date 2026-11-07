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
