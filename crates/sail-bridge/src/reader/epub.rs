//! EPUB 2/3 to plain-text blocks.
//!
//! `META-INF/container.xml` names the package (OPF). Its spine gives the reading order and its
//! manifest the files. Each spine document is parsed as XML and flattened into [`Block`]s of
//! plain text. Nothing executable or remote survives. Scripts, styles, media, embedded
//! objects and footnote bodies are dropped (footnotes would otherwise interrupt narration),
//! and so are note reference markers. The table of contents comes from the EPUB 3 navigation
//! document or the EPUB 2 NCX, and falls back to section headings.
use std::collections::HashMap;

use roxmltree::{Document, Node, ParsingOptions};

use super::zip::Archive;
use super::{Block, BlockKind, Parsed, ReaderError, Section, TocNode};

const OPS_NS: &str = "http://www.idpf.org/2007/ops";
/// Where a link into each spine file lands: its section, and the block of each element id.
type Targets = HashMap<String, (u32, HashMap<String, u32>)>;
const MAX_TOC_NODES: usize = 4_000;
const MAX_TOC_DEPTH: usize = 8;
/// Font obfuscation is not DRM; the reader does not use embedded fonts anyway.
const FONT_OBFUSCATION: [&str; 2] = [
    "http://www.idpf.org/2008/embedding",
    "http://ns.adobe.com/pdf/enc#RC",
];

fn invalid(message: &str) -> ReaderError {
    ReaderError::Invalid(format!("This EPUB could not be read: {message}."))
}

fn parse_xml(text: &str) -> Result<Document<'_>, roxmltree::Error> {
    Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: true,
            nodes_limit: 2_000_000,
        },
    )
}

pub fn parse(bytes: &[u8]) -> Result<Parsed, ReaderError> {
    let archive = Archive::open(bytes)?;
    if archive.contains("mimetype")
        && archive.read("mimetype")?.trim_ascii() != b"application/epub+zip"
    {
        return Err(invalid("it is not an EPUB package"));
    }
    refuse_drm(&archive)?;

    let container = archive.read_text("META-INF/container.xml")?;
    let container = parse_xml(&container).map_err(|_| invalid("broken container.xml"))?;
    let opf_path = container
        .descendants()
        .find(|n| n.has_tag_name("rootfile"))
        .and_then(|n| n.attribute("full-path"))
        .ok_or_else(|| invalid("no package document"))?
        .to_string();
    let opf_text = archive.read_text(&opf_path)?;
    let opf = parse_xml(&opf_text).map_err(|_| invalid("broken package document"))?;
    let opf_dir = parent(&opf_path);

    let metadata = opf
        .descendants()
        .find(|n| n.tag_name().name() == "metadata");
    let dc = |name: &str| -> Vec<String> {
        metadata
            .into_iter()
            .flat_map(|m| m.descendants())
            .filter(|n| n.is_element() && n.tag_name().name() == name)
            .map(|n| collapse(&text_of(n)))
            .filter(|s| !s.is_empty())
            .collect()
    };
    let title = dc("title").into_iter().next();
    let authors = dc("creator");
    let language = dc("language").into_iter().next();

    struct Item {
        href: String,
        media: String,
        properties: String,
    }
    let mut manifest = HashMap::new();
    for item in opf.descendants().filter(|n| n.tag_name().name() == "item") {
        if let (Some(id), Some(href)) = (item.attribute("id"), item.attribute("href")) {
            manifest.insert(
                id.to_string(),
                Item {
                    href: join(&opf_dir, href)?,
                    media: item.attribute("media-type").unwrap_or("").to_string(),
                    properties: item.attribute("properties").unwrap_or("").to_string(),
                },
            );
        }
    }
    let spine = opf
        .descendants()
        .find(|n| n.tag_name().name() == "spine")
        .ok_or_else(|| invalid("no reading order"))?;

    // Spine documents → sections. `targets` maps every spine file (kept or not) to the
    // section a link into it should open, so TOC entries into skipped pages still resolve.
    let mut sections: Vec<Section> = Vec::new();
    let mut targets: Targets = HashMap::new();
    let mut pending: Vec<String> = Vec::new();
    let mut total_chars: u64 = 0;
    for itemref in spine
        .children()
        .filter(|n| n.tag_name().name() == "itemref")
    {
        if itemref.attribute("linear") == Some("no") {
            continue;
        }
        let Some(item) = itemref.attribute("idref").and_then(|id| manifest.get(id)) else {
            continue;
        };
        if !(item.media.contains("xhtml") || item.media.contains("html")) {
            continue;
        }
        pending.push(item.href.clone());
        let Ok(text) = archive.read_text(&item.href) else {
            continue;
        };
        let text = numeric_entities(&text);
        let Ok(doc) = parse_xml(&text) else {
            tracing::warn!(
                section = sections.len(),
                "Reader skipped an unparsable EPUB page"
            );
            continue;
        };
        let (blocks, ids) = blocks_of(&doc);
        if blocks.is_empty() {
            continue;
        }
        total_chars += blocks
            .iter()
            .map(|b| b.text.chars().count() as u64)
            .sum::<u64>();
        if total_chars > super::MAX_BOOK_CHARS {
            return Err(ReaderError::TooLarge);
        }
        let index = sections.len() as u32;
        let own = pending.pop();
        for href in pending.drain(..) {
            targets.insert(href, (index, HashMap::new()));
        }
        if let Some(href) = own {
            targets.insert(href, (index, ids));
        }
        sections.push(Section {
            title: blocks
                .iter()
                .find(|b| b.kind.is_heading())
                .map(|b| b.text.clone()),
            blocks,
        });
    }
    if sections.is_empty() {
        return Err(invalid("it has no readable text"));
    }
    // Trailing skipped pages point at the last section.
    for href in pending {
        targets.insert(href, (sections.len() as u32 - 1, HashMap::new()));
    }

    let mut toc = Vec::new();
    let nav = manifest
        .values()
        .find(|i| i.properties.split_whitespace().any(|p| p == "nav"));
    if let Some(nav) = nav
        && let Ok(text) = archive.read_text(&nav.href)
    {
        let text = numeric_entities(&text);
        if let Ok(doc) = parse_xml(&text) {
            toc = nav_toc(&doc, &parent(&nav.href), &targets);
        }
    }
    if toc.is_empty()
        && let Some(ncx) = spine.attribute("toc").and_then(|id| manifest.get(id))
        && let Ok(text) = archive.read_text(&ncx.href)
        && let Ok(doc) = parse_xml(&numeric_entities(&text))
    {
        toc = ncx_toc(&doc, &parent(&ncx.href), &targets);
    }
    if toc.is_empty() {
        toc = sections
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                s.title.clone().map(|title| TocNode {
                    title: title.replace('\n', " "),
                    section: i as u32,
                    block: 0,
                    children: Vec::new(),
                })
            })
            .collect();
    }
    Ok(Parsed {
        title,
        authors,
        language,
        sections,
        toc,
    })
}

fn refuse_drm(archive: &Archive) -> Result<(), ReaderError> {
    if archive.contains("META-INF/rights.xml") {
        return Err(ReaderError::Protected);
    }
    if archive.contains("META-INF/encryption.xml") {
        let text = archive.read_text("META-INF/encryption.xml")?;
        let doc = parse_xml(&text).map_err(|_| ReaderError::Protected)?;
        let content_encrypted = doc
            .descendants()
            .filter(|n| n.tag_name().name() == "EncryptionMethod")
            .any(|n| !FONT_OBFUSCATION.contains(&n.attribute("Algorithm").unwrap_or("")));
        if content_encrypted {
            return Err(ReaderError::Protected);
        }
    }
    Ok(())
}

fn parent(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/"))
        .unwrap_or_default()
}

/// Resolve an href against a directory inside the archive, percent-decoded and without its
/// fragment. Paths that climb out of the archive are refused.
fn join(dir: &str, href: &str) -> Result<String, ReaderError> {
    let href = href.split('#').next().unwrap_or("");
    let href = percent_decode(href);
    let mut parts: Vec<&str> = Vec::new();
    for part in dir.split('/').chain(href.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(invalid("a link points outside the book"));
                }
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The (section, block) a link opens; an unknown fragment opens the section's start.
fn link_target(dir: &str, href: &str, targets: &Targets) -> Option<(u32, u32)> {
    let (section, ids) = targets.get(&join(dir, href).ok()?)?;
    let block = href
        .split_once('#')
        .and_then(|(_, fragment)| ids.get(&percent_decode(fragment)))
        .copied()
        .unwrap_or(0);
    Some((*section, block))
}

fn nav_toc(doc: &Document, dir: &str, targets: &Targets) -> Vec<TocNode> {
    let navs: Vec<Node> = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "nav")
        .collect();
    let nav = navs
        .iter()
        .find(|n| {
            n.attribute((OPS_NS, "type"))
                .is_some_and(|t| t.split_whitespace().any(|t| t == "toc"))
        })
        .or(navs.first());
    let Some(list) = nav.and_then(|n| n.children().find(|c| c.tag_name().name() == "ol")) else {
        return Vec::new();
    };
    let mut budget = MAX_TOC_NODES;
    nav_list(list, dir, targets, 0, &mut budget)
}

fn nav_list(
    list: Node,
    dir: &str,
    targets: &Targets,
    depth: usize,
    budget: &mut usize,
) -> Vec<TocNode> {
    let mut out = Vec::new();
    if depth >= MAX_TOC_DEPTH {
        return out;
    }
    for li in list.children().filter(|n| n.tag_name().name() == "li") {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let label = li
            .children()
            .find(|c| matches!(c.tag_name().name(), "a" | "span"));
        let title = label.map(|l| collapse(&text_of(l))).unwrap_or_default();
        let target = label
            .and_then(|l| l.attribute("href"))
            .and_then(|href| link_target(dir, href, targets));
        let children = li
            .children()
            .find(|c| c.tag_name().name() == "ol")
            .map(|ol| nav_list(ol, dir, targets, depth + 1, budget))
            .unwrap_or_default();
        push_toc(&mut out, title, target, children);
    }
    out
}

fn ncx_toc(doc: &Document, dir: &str, targets: &Targets) -> Vec<TocNode> {
    let Some(map) = doc.descendants().find(|n| n.tag_name().name() == "navMap") else {
        return Vec::new();
    };
    let mut budget = MAX_TOC_NODES;
    ncx_points(map, dir, targets, 0, &mut budget)
}

fn ncx_points(
    parent: Node,
    dir: &str,
    targets: &Targets,
    depth: usize,
    budget: &mut usize,
) -> Vec<TocNode> {
    let mut out = Vec::new();
    if depth >= MAX_TOC_DEPTH {
        return out;
    }
    for point in parent
        .children()
        .filter(|n| n.tag_name().name() == "navPoint")
    {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let title = point
            .children()
            .find(|c| c.tag_name().name() == "navLabel")
            .map(|l| collapse(&text_of(l)))
            .unwrap_or_default();
        let target = point
            .children()
            .find(|c| c.tag_name().name() == "content")
            .and_then(|c| c.attribute("src"))
            .and_then(|src| link_target(dir, src, targets));
        let children = ncx_points(point, dir, targets, depth + 1, budget);
        push_toc(&mut out, title, target, children);
    }
    out
}

/// Keep an entry that has a title and a target; an entry without a target is replaced by its
/// children so nested chapters still appear.
fn push_toc(
    out: &mut Vec<TocNode>,
    title: String,
    target: Option<(u32, u32)>,
    children: Vec<TocNode>,
) {
    match target {
        Some((section, block)) if !title.is_empty() => out.push(TocNode {
            title: title.replace('\n', " "),
            section,
            block,
            children,
        }),
        _ => out.extend(children),
    }
}

fn text_of(node: Node) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

const BREAK: char = '\u{e000}';

/// Collapse whitespace runs to single spaces, keep line breaks from `<br>`, drop soft hyphens.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c == '\u{ad}' {
            continue;
        }
        if c == BREAK {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
            space = false;
        } else if c.is_whitespace() && c != '\u{a0}' && c != '\u{202f}' {
            space = !out.is_empty() && !out.ends_with('\n');
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    out.trim_matches('\n').to_string()
}

fn skipped(node: Node) -> bool {
    let name = node.tag_name().name();
    if matches!(
        name,
        "head"
            | "script"
            | "style"
            | "noscript"
            | "template"
            | "svg"
            | "math"
            | "iframe"
            | "object"
            | "embed"
            | "video"
            | "audio"
            | "canvas"
            | "img"
            | "picture"
            | "map"
            | "button"
            | "input"
            | "select"
            | "textarea"
            | "form"
            | "rt"
            | "rp"
    ) {
        return true;
    }
    let notes = ["footnote", "endnote", "rearnote", "noteref", "note"];
    let typed = node
        .attribute((OPS_NS, "type"))
        .is_some_and(|t| t.split_whitespace().any(|t| notes.contains(&t)));
    let role = node.attribute("role").is_some_and(|r| {
        matches!(
            r,
            "doc-footnote" | "doc-endnote" | "doc-noteref" | "doc-endnotes"
        )
    });
    typed || role || node.attribute("hidden").is_some()
}

fn block_kind(name: &str, inherited: BlockKind) -> Option<BlockKind> {
    Some(match name {
        "h1" => BlockKind::H1,
        "h2" => BlockKind::H2,
        "h3" => BlockKind::H3,
        "h4" => BlockKind::H4,
        "h5" => BlockKind::H5,
        "h6" => BlockKind::H6,
        "blockquote" => BlockKind::Quote,
        "li" | "dt" | "dd" => BlockKind::Li,
        "pre" => BlockKind::Pre,
        "p" | "div" | "section" | "article" | "body" | "figure" | "figcaption" | "header"
        | "footer" | "main" | "aside" | "table" | "tr" | "td" | "th" | "caption" | "ul" | "ol"
        | "dl" | "nav" | "address" | "hgroup" | "details" | "summary" => {
            if matches!(inherited, BlockKind::Quote | BlockKind::Li | BlockKind::Pre) {
                inherited
            } else {
                BlockKind::P
            }
        }
        _ => return None,
    })
}

struct Flatten {
    blocks: Vec<Block>,
    buffer: String,
    /// Element id → the block it starts in, so `chapter.xhtml#ch3` links land on chapter 3.
    ids: HashMap<String, u32>,
}

impl Flatten {
    fn flush(&mut self, kind: BlockKind) {
        if self.buffer.is_empty() {
            return;
        }
        let text = if kind == BlockKind::Pre {
            self.buffer
                .replace(BREAK, "\n")
                .replace('\u{ad}', "")
                .trim_matches('\n')
                .to_string()
        } else {
            collapse(&self.buffer)
        };
        self.buffer.clear();
        if !text.trim().is_empty() && self.blocks.len() < super::MAX_SECTION_BLOCKS {
            self.blocks.push(Block { kind, text });
        }
    }

    /// The id of an element starts at the block being built now (or the next one).
    fn mark(&mut self, node: Node) {
        if let Some(id) = node.attribute("id").filter(|id| id.len() <= 256)
            && self.ids.len() < 100_000
        {
            self.ids
                .entry(id.to_string())
                .or_insert(self.blocks.len() as u32);
        }
    }

    fn walk(&mut self, node: Node, kind: BlockKind) {
        for child in node.children() {
            if let Some(text) = child.text().filter(|_| child.is_text()) {
                self.buffer.push_str(text);
                continue;
            }
            // A drop cap drawn as an image carries its letter in `alt`; keep it so the first
            // word reads "When", not "hen". Other images are dropped.
            if child.has_tag_name("img")
                && let Some(alt) = child.attribute("alt").map(str::trim)
                && (1..=2).contains(&alt.chars().count())
            {
                self.buffer.push_str(alt);
                continue;
            }
            if !child.is_element() || skipped(child) {
                continue;
            }
            let name = child.tag_name().name();
            if name == "br" {
                self.buffer.push(BREAK);
            } else if name == "hr" {
                self.flush(kind);
                self.mark(child);
                self.blocks.push(Block {
                    kind: BlockKind::Hr,
                    text: String::new(),
                });
            } else if let Some(inner) = block_kind(name, kind) {
                self.flush(kind);
                self.mark(child);
                self.walk(child, inner);
                self.flush(inner);
            } else {
                self.mark(child);
                self.walk(child, kind);
            }
        }
    }
}

fn blocks_of(doc: &Document) -> (Vec<Block>, HashMap<String, u32>) {
    let Some(body) = doc.descendants().find(|n| n.tag_name().name() == "body") else {
        return (Vec::new(), HashMap::new());
    };
    let mut flatten = Flatten {
        blocks: Vec::new(),
        buffer: String::new(),
        ids: HashMap::new(),
    };
    flatten.walk(body, BlockKind::P);
    flatten.flush(BlockKind::P);
    // A trailing rule carries no meaning.
    while flatten
        .blocks
        .last()
        .is_some_and(|b| b.kind == BlockKind::Hr)
    {
        flatten.blocks.pop();
    }
    let last = flatten.blocks.len().saturating_sub(1) as u32;
    for block in flatten.ids.values_mut() {
        *block = (*block).min(last);
    }
    (flatten.blocks, flatten.ids)
}

/// XHTML in EPUBs often uses HTML named entities without declaring them, which XML parsers
/// reject. Rewrite the common ones as numeric references; leave the five XML ones alone.
fn numeric_entities(text: &str) -> String {
    const NAMED: &[(&str, u32)] = &[
        ("nbsp", 0xa0),
        ("ensp", 0x2002),
        ("emsp", 0x2003),
        ("thinsp", 0x2009),
        ("shy", 0xad),
        ("mdash", 0x2014),
        ("ndash", 0x2013),
        ("hellip", 0x2026),
        ("lsquo", 0x2018),
        ("rsquo", 0x2019),
        ("ldquo", 0x201c),
        ("rdquo", 0x201d),
        ("sbquo", 0x201a),
        ("bdquo", 0x201e),
        ("laquo", 0xab),
        ("raquo", 0xbb),
        ("lsaquo", 0x2039),
        ("rsaquo", 0x203a),
        ("copy", 0xa9),
        ("reg", 0xae),
        ("trade", 0x2122),
        ("deg", 0xb0),
        ("middot", 0xb7),
        ("bull", 0x2022),
        ("dagger", 0x2020),
        ("Dagger", 0x2021),
        ("sect", 0xa7),
        ("para", 0xb6),
        ("times", 0xd7),
        ("divide", 0xf7),
        ("frac12", 0xbd),
        ("frac14", 0xbc),
        ("frac34", 0xbe),
        ("prime", 0x2032),
        ("Prime", 0x2033),
        ("zwj", 0x200d),
        ("zwnj", 0x200c),
        ("eacute", 0xe9),
        ("egrave", 0xe8),
        ("aacute", 0xe1),
        ("agrave", 0xe0),
        ("iacute", 0xed),
        ("oacute", 0xf3),
        ("uacute", 0xfa),
        ("ntilde", 0xf1),
        ("ccedil", 0xe7),
        ("atilde", 0xe3),
        ("otilde", 0xf5),
        ("acirc", 0xe2),
        ("ecirc", 0xea),
        ("ocirc", 0xf4),
        ("uuml", 0xfc),
        ("ouml", 0xf6),
        ("auml", 0xe4),
        ("euml", 0xeb),
        ("iuml", 0xef),
        ("szlig", 0xdf),
        ("Eacute", 0xc9),
        ("Agrave", 0xc0),
        ("Ccedil", 0xc7),
        ("pound", 0xa3),
        ("euro", 0x20ac),
        ("yen", 0xa5),
        ("cent", 0xa2),
        ("iexcl", 0xa1),
        ("iquest", 0xbf),
    ];
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|&e| e <= 10);
        let replaced = end.and_then(|e| {
            let name = &rest[1..e];
            NAMED.iter().find(|(n, _)| *n == name).map(|(_, v)| (e, *v))
        });
        match replaced {
            Some((end, value)) => {
                out.push_str(&format!("&#{value};"));
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::zip::build;

    const CONTAINER: &[u8] = br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;

    fn opf(extra_manifest: &str, spine_attr: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>The Test Book</dc:title><dc:creator>Ada Writer</dc:creator><dc:language>en</dc:language></metadata><manifest><item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/><item id="c1" href="text/ch%201.xhtml" media-type="application/xhtml+xml"/><item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>{extra_manifest}</manifest><spine{spine_attr}><itemref idref="cover"/><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#
        )
    }

    const COVER: &[u8] = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><img src="cover.jpg" alt="Cover"/></body></html>"#;
    const CH1: &[u8] = br##"<?xml version="1.0"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>x</title><style>p{color:red}</style><script>alert('x')</script></head><body onload="evil()"><h1>Chapter&nbsp;One</h1><p><span class="letra"><img alt="I" src="i.png"/></span>t was a <em>dark</em>
      and stormy night.<a epub:type="noteref" href="#n1">1</a> The end&hellip;</p><p>Line one<br/>line two</p><aside epub:type="footnote" id="n1"><p>A footnote.</p></aside><blockquote><p>Quoted words.</p></blockquote><ul><li>First</li><li>Sec&shy;ond</li></ul><hr/><pre>  code
  block</pre><iframe src="https://evil.example/"></iframe></body></html>"##;
    const CH2: &[u8] = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><section><h2>Chapter Two</h2><div id="later">Bare div text <span>with inline</span> parts.</div></section></body></html>"#;
    const NAV: &[u8] = br##"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="cover.xhtml">Cover</a></li><li><span>Part I</span><ol><li><a href="text/ch%201.xhtml#start">One</a></li><li><a href="text/ch2.xhtml">Two</a></li><li><a href="text/ch2.xhtml#later">Later</a></li></ol></li></ol></nav></body></html>"##;

    fn book(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut all: Vec<(&str, &[u8], bool)> = vec![
            ("mimetype", b"application/epub+zip", false),
            ("META-INF/container.xml", CONTAINER, true),
        ];
        all.extend(files.iter().map(|(n, d)| (*n, *d, true)));
        build(&all)
    }

    #[test]
    fn flattens_epub3_into_safe_blocks_with_nav_toc() {
        let opf = opf(
            r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#,
            "",
        );
        let bytes = book(&[
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/cover.xhtml", COVER),
            ("OEBPS/text/ch 1.xhtml", CH1),
            ("OEBPS/text/ch2.xhtml", CH2),
            ("OEBPS/nav.xhtml", NAV),
        ]);
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.title.as_deref(), Some("The Test Book"));
        assert_eq!(parsed.authors, ["Ada Writer"]);
        assert_eq!(parsed.language.as_deref(), Some("en"));
        // The image-only cover has no text and is skipped.
        assert_eq!(parsed.sections.len(), 2);
        let ch1: Vec<(BlockKind, &str)> = parsed.sections[0]
            .blocks
            .iter()
            .map(|b| (b.kind, b.text.as_str()))
            .collect();
        assert_eq!(
            ch1,
            [
                (BlockKind::H1, "Chapter\u{a0}One"),
                (BlockKind::P, "It was a dark and stormy night. The end…"),
                (BlockKind::P, "Line one\nline two"),
                (BlockKind::Quote, "Quoted words."),
                (BlockKind::Li, "First"),
                (BlockKind::Li, "Second"),
                (BlockKind::Hr, ""),
                (BlockKind::Pre, "  code\n  block"),
            ]
        );
        assert_eq!(
            parsed.sections[0].title.as_deref(),
            Some("Chapter\u{a0}One")
        );
        let all: String = parsed
            .sections
            .iter()
            .flat_map(|s| &s.blocks)
            .map(|b| b.text.as_str())
            .collect();
        for leaked in ["alert", "color:red", "footnote", "evil", "Cover"] {
            assert!(!all.contains(leaked), "{leaked} leaked");
        }
        assert_eq!(
            parsed.sections[1].blocks[1].text,
            "Bare div text with inline parts."
        );
        // The cover link resolves to the first readable section; "Part I" has no target and
        // is replaced by its children.
        let toc: Vec<(&str, u32, usize)> = parsed
            .toc
            .iter()
            .map(|t| (t.title.as_str(), t.section, t.children.len()))
            .collect();
        assert_eq!(
            toc,
            [
                ("Cover", 0, 0),
                ("One", 0, 0),
                ("Two", 1, 0),
                ("Later", 1, 0)
            ]
        );
        assert_eq!((parsed.toc[3].block, parsed.toc[2].block), (1, 0));
    }

    #[test]
    fn reads_epub2_ncx_and_falls_back_to_headings() {
        let ncx = br#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint id="a"><navLabel><text>First chapter</text></navLabel><content src="text/ch%201.xhtml"/><navPoint id="b"><navLabel><text>Second</text></navLabel><content src="text/ch2.xhtml"/></navPoint></navPoint></navMap></ncx>"#;
        let opf2 = opf(
            r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#,
            r#" toc="ncx""#,
        );
        let bytes = book(&[
            ("OEBPS/content.opf", opf2.as_bytes()),
            ("OEBPS/cover.xhtml", COVER),
            ("OEBPS/text/ch 1.xhtml", CH1),
            ("OEBPS/text/ch2.xhtml", CH2),
            ("OEBPS/toc.ncx", ncx),
        ]);
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.toc.len(), 1);
        assert_eq!(parsed.toc[0].title, "First chapter");
        assert_eq!(parsed.toc[0].children[0].section, 1);

        let plain = opf("", "");
        let bytes = book(&[
            ("OEBPS/content.opf", plain.as_bytes()),
            ("OEBPS/cover.xhtml", COVER),
            ("OEBPS/text/ch 1.xhtml", CH1),
            ("OEBPS/text/ch2.xhtml", CH2),
        ]);
        let titles: Vec<String> = parse(&bytes)
            .unwrap()
            .toc
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(titles, ["Chapter\u{a0}One", "Chapter Two"]);
    }

    #[test]
    fn refuses_drm_and_escaping_links() {
        let plain = opf("", "");
        let enc = br#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/></enc:EncryptedData></encryption>"#;
        let bytes = book(&[
            ("META-INF/encryption.xml", enc),
            ("OEBPS/content.opf", plain.as_bytes()),
            ("OEBPS/text/ch2.xhtml", CH2),
        ]);
        assert!(matches!(parse(&bytes), Err(ReaderError::Protected)));
        let fonts = br#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/></enc:EncryptedData></encryption>"#;
        let bytes = book(&[
            ("META-INF/encryption.xml", fonts),
            ("OEBPS/content.opf", plain.as_bytes()),
            ("OEBPS/text/ch2.xhtml", CH2),
        ]);
        assert_eq!(parse(&bytes).unwrap().sections.len(), 1);

        let escaping = plain.replace("text/ch2.xhtml", "../../../etc/passwd");
        let bytes = book(&[
            ("OEBPS/content.opf", escaping.as_bytes()),
            ("OEBPS/text/ch2.xhtml", CH2),
        ]);
        assert!(matches!(parse(&bytes), Err(ReaderError::Invalid(_))));
    }

    #[test]
    fn entity_rewrite_keeps_xml_entities_and_unknown_ampersands() {
        assert_eq!(
            numeric_entities("a&nbsp;b &amp; c & d &bogus;"),
            "a&#160;b &amp; c & d &bogus;"
        );
    }
}
