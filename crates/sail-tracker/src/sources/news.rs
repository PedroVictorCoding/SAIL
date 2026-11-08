//! RSS/Atom headlines: parse, clean, and cluster near-duplicates so one story reported by
//! five outlets shows once, with its five sources.
use chrono::DateTime;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct RawItem {
    pub title: String,
    pub link: String,
    pub published: Option<i64>,
    /// The outlet named inside the item (aggregators such as Google News carry one).
    pub source: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("feed is not RSS or Atom: {0}")]
pub struct FeedError(String);

fn clean(text: &str) -> String {
    // Some feeds put markup in titles; drop tags and collapse whitespace.
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(if c.is_whitespace() { ' ' } else { c }),
            _ => {}
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(300).collect()
}

/// All text inside the first child element called `name`, including text in nested markup.
fn child_text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    let child = node
        .children()
        .find(|c| c.is_element() && c.tag_name().name() == name)?;
    let text: String = child
        .descendants()
        .filter(|d| d.is_text())
        .filter_map(|d| d.text())
        .collect();
    Some(text)
}

fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    DateTime::parse_from_rfc2822(s)
        .or_else(|_| DateTime::parse_from_rfc3339(s))
        .ok()
        .map(|d| d.timestamp())
}

fn web_link(s: &str) -> Option<String> {
    let u = url::Url::parse(s.trim()).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

/// XML 1.0 forbids most C0 controls (and U+FFFE/U+FFFF), but real feeds occasionally
/// contain them. Drop only the illegal characters before parsing so one stray backspace in
/// a publisher's description cannot take the whole source offline.
fn valid_xml_char(c: char) -> bool {
    matches!(c, '\u{9}' | '\u{A}' | '\u{D}')
        || ('\u{20}'..='\u{D7FF}').contains(&c)
        || ('\u{E000}'..='\u{FFFD}').contains(&c)
        || c >= '\u{10000}'
}

fn sanitize_xml(xml: &str) -> Cow<'_, str> {
    if xml.chars().all(valid_xml_char) {
        Cow::Borrowed(xml)
    } else {
        Cow::Owned(xml.chars().filter(|c| valid_xml_char(*c)).collect())
    }
}

pub fn parse_feed(xml: &str) -> Result<Vec<RawItem>, FeedError> {
    let xml = sanitize_xml(xml);
    let doc = roxmltree::Document::parse(xml.as_ref()).map_err(|e| FeedError(e.to_string()))?;
    let root = doc.root_element();
    let mut items = Vec::new();
    match root.tag_name().name() {
        "rss" | "RDF" => {
            for item in root
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "item")
            {
                let Some(title) = child_text(item, "title")
                    .as_deref()
                    .map(clean)
                    .filter(|t| !t.is_empty())
                else {
                    continue;
                };
                let Some(link) = child_text(item, "link").as_deref().and_then(web_link) else {
                    continue;
                };
                let published = child_text(item, "pubDate")
                    .or_else(|| child_text(item, "date"))
                    .as_deref()
                    .and_then(parse_date);
                let source = child_text(item, "source")
                    .as_deref()
                    .map(clean)
                    .filter(|s| !s.is_empty());
                items.push(RawItem {
                    title,
                    link,
                    published,
                    source,
                });
            }
        }
        "feed" => {
            for entry in root
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "entry")
            {
                let Some(title) = child_text(entry, "title")
                    .as_deref()
                    .map(clean)
                    .filter(|t| !t.is_empty())
                else {
                    continue;
                };
                let link = entry
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "link")
                    .find(|n| n.attribute("rel").is_none_or(|r| r == "alternate"))
                    .and_then(|n| n.attribute("href"))
                    .and_then(web_link);
                let Some(link) = link else { continue };
                let published = child_text(entry, "published")
                    .or_else(|| child_text(entry, "updated"))
                    .as_deref()
                    .and_then(parse_date);
                items.push(RawItem {
                    title,
                    link,
                    published,
                    source: None,
                });
            }
        }
        other => return Err(FeedError(format!("root element <{other}>"))),
    }
    Ok(items)
}

/// Google News titles end in " - Outlet"; split that off when the outlet is known.
pub fn strip_outlet_suffix(title: &str, outlet: &str) -> String {
    title
        .strip_suffix(outlet)
        .and_then(|t| t.strip_suffix(" - "))
        .map(str::to_string)
        .unwrap_or_else(|| title.to_string())
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "in", "is",
    "it", "its", "of", "on", "or", "says", "that", "the", "to", "was", "were", "will", "with",
    "after", "over", "new", "amid", "into", "about", "more", "than", "this", "who", "what",
];

pub fn tokens(title: &str) -> HashSet<String> {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1 && !STOPWORDS.contains(w))
        .map(|w| w.trim_end_matches('s').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

pub fn similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    // Overlap relative to the shorter title: rewrites often add words.
    inter / a.len().min(b.len()) as f64
}

/// Stable id for a normalized title (FNV-1a), so the same headline from a feed refresh
/// is recognised as already seen.
pub fn headline_id(title: &str) -> String {
    let mut words: Vec<String> = tokens(title).into_iter().collect();
    words.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in words.join(" ").bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SourceLink {
    pub source: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Story {
    pub id: String,
    pub title: String,
    pub url: String,
    pub source: String,
    /// Other outlets carrying the same story.
    pub also: Vec<SourceLink>,
    pub topic: Option<String>,
    pub published: i64,
    pub first_seen: i64,
}

/// Groups near-duplicate headlines. Input should be newest first; each story keeps the
/// earliest report as its lead so the timestamp says when the story broke.
pub fn cluster(items: Vec<crate::store::StoredHeadline>, threshold: f64) -> Vec<Story> {
    let mut stories: Vec<(HashSet<String>, Story)> = Vec::new();
    for h in items {
        let t = tokens(&h.title);
        if let Some((_, story)) = stories
            .iter_mut()
            .find(|(st, _)| similarity(st, &t) >= threshold && t.len() >= 3)
        {
            if story.source != h.source && !story.also.iter().any(|s| s.source == h.source) {
                story.also.push(SourceLink {
                    source: h.source.clone(),
                    url: h.url.clone(),
                });
            }
            if story.topic.is_none() {
                story.topic = h.topic.clone();
            }
            if h.published < story.published {
                // Older report becomes the lead; the previous lead becomes an "also".
                let previous = SourceLink {
                    source: story.source.clone(),
                    url: story.url.clone(),
                };
                story.also.retain(|s| s.source != h.source);
                if previous.source != h.source {
                    story.also.push(previous);
                }
                story.title = h.title;
                story.url = h.url;
                story.source = h.source;
                story.published = h.published;
                story.first_seen = story.first_seen.min(h.first_seen);
            }
            continue;
        }
        stories.push((
            t,
            Story {
                id: h.id,
                title: h.title,
                url: h.url,
                source: h.source,
                also: vec![],
                topic: h.topic,
                published: h.published,
                first_seen: h.first_seen,
            },
        ));
    }
    let mut out: Vec<Story> = stories.into_iter().map(|(_, s)| s).collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.published));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoredHeadline;

    #[test]
    fn strips_invalid_xml_control_characters_instead_of_failing() {
        let rss = "<rss><channel><item><title>Good \u{1f600}</title>\
            <link>https://example.com/a</link>\
            <description>bad\u{8}text</description></item></channel></rss>";
        let items = parse_feed(rss).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Good \u{1f600}");
        assert_eq!(
            sanitize_xml("\u{0}\u{1}\u{8}\u{b}\u{c}\u{1f}\u{fffe}\u{ffff}"),
            ""
        );
        assert_eq!(sanitize_xml("a\tb\nc\rd\u{7f}"), "a\tb\nc\rd\u{7f}");
    }

    #[test]
    fn parses_rss_and_atom() {
        let rss = r#"<?xml version="1.0"?><rss><channel><title>X</title>
            <item><title>Port &amp; rail <b>strike</b> spreads</title><link>https://ex.com/a</link>
            <pubDate>Wed, 04 Nov 2026 10:00:00 GMT</pubDate><source url="https://o.com">Outlet</source></item>
            <item><title>No link</title></item>
            <item><title>Bad link</title><link>javascript:alert(1)</link></item>
            </channel></rss>"#;
        let items = parse_feed(rss).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Port & rail strike spreads");
        assert_eq!(items[0].source.as_deref(), Some("Outlet"));
        assert_eq!(items[0].published, Some(1_793_786_400));

        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><title>Atom item</title>
            <link rel="self" href="https://ex.com/self"/><link href="https://ex.com/b"/>
            <updated>2026-11-04T10:00:00Z</updated></entry></feed>"#;
        let items = parse_feed(atom).unwrap();
        assert_eq!(items[0].link, "https://ex.com/b");
        assert!(parse_feed("<html></html>").is_err());
    }

    #[test]
    fn strips_outlet() {
        assert_eq!(
            strip_outlet_suffix("Big news - Reuters", "Reuters"),
            "Big news"
        );
        assert_eq!(strip_outlet_suffix("Big news", "Reuters"), "Big news");
    }

    fn h(id: &str, title: &str, source: &str, published: i64) -> StoredHeadline {
        StoredHeadline {
            id: id.into(),
            title: title.into(),
            url: format!("https://{source}.example/{id}"),
            source: source.into(),
            topic: None,
            published,
            first_seen: published,
        }
    }

    #[test]
    fn clusters_rewrites_and_keeps_earliest_lead() {
        let stories = cluster(
            vec![
                h(
                    "1",
                    "Suez Canal traffic halted after ship runs aground",
                    "BBC",
                    300,
                ),
                h(
                    "2",
                    "Ship runs aground, Suez Canal traffic halted",
                    "NPR",
                    200,
                ),
                h("3", "Central bank holds interest rates steady", "BBC", 250),
            ],
            0.7,
        );
        assert_eq!(stories.len(), 2);
        let suez = stories.iter().find(|s| s.title.contains("Suez")).unwrap();
        assert_eq!(suez.source, "NPR");
        assert_eq!(suez.published, 200);
        assert_eq!(suez.also.len(), 1);
        assert_eq!(suez.also[0].source, "BBC");
    }

    #[test]
    fn ids_ignore_word_order_and_case() {
        assert_eq!(
            headline_id("Suez canal halted"),
            headline_id("HALTED: Suez Canal")
        );
    }
}
