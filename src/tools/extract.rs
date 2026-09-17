//! Text extraction from PDFs and web pages (SPEC §4.2, §4.3).
//!
//! Extraction reports what it actually recovered. A PDF without a text layer
//! yields no text and says so; a page whose main content could not be
//! isolated falls back to the visible text with that noted. Nothing here
//! claims full-text access it did not obtain.

use crate::{Error, Result};

/// How much of a document the extractor recovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// Every page (or the whole page body) yielded text.
    Full,
    /// Some pages yielded text and others none.
    Partial { extracted: usize, total: usize },
    /// No text could be extracted, for the stated reason.
    None { reason: String },
}

/// Extracted text with whatever bibliographic fields the document carried.
#[derive(Debug, Clone)]
pub struct Extracted {
    pub text: String,
    pub title: Option<String>,
    pub authors: Option<String>,
    pub date: Option<String>,
    pub coverage: Coverage,
}

impl Extracted {
    /// Plain text supplied as-is.
    pub fn plain(text: String) -> Self {
        Self {
            text,
            title: None,
            authors: None,
            date: None,
            coverage: Coverage::Full,
        }
    }

    /// Nothing could be extracted.
    pub fn none(reason: String) -> Self {
        Self {
            text: String::new(),
            title: None,
            authors: None,
            date: None,
            coverage: Coverage::None { reason },
        }
    }
}

/// Fewest non-whitespace characters for a page to count as carrying text.
const MIN_PAGE_CHARS: usize = 20;

/// Extract a PDF's text page by page, off the async executor (SPEC §9.1:
/// keep CPU-heavy work off executor threads).
pub async fn pdf(bytes: Vec<u8>) -> Result<Extracted> {
    tokio::task::spawn_blocking(move || pdf_blocking(bytes))
        .await
        .map_err(|e| Error::validation(format!("PDF extraction task failed: {e}")))?
}

fn pdf_blocking(bytes: Vec<u8>) -> Result<Extracted> {
    let doc = match pdf_oxide::PdfDocument::from_bytes(bytes.clone()) {
        Ok(d) => d,
        Err(e) => {
            return Ok(Extracted::none(format!(
                "the PDF could not be parsed ({e}); only metadata was recorded"
            )));
        }
    };
    let total = doc.page_count().unwrap_or(0);
    if total == 0 {
        return Ok(Extracted::none(
            "the PDF has no pages; only metadata was recorded".into(),
        ));
    }

    let mut text = String::new();
    let mut extracted = 0usize;
    for page in 0..total {
        match doc.extract_text(page) {
            Ok(t) if t.chars().filter(|c| !c.is_whitespace()).count() >= MIN_PAGE_CHARS => {
                extracted += 1;
                text.push_str(&format!("\n\n--- page {} ---\n", page + 1));
                text.push_str(t.trim_end());
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(page, error = %e, "page yielded no text"),
        }
    }

    // Bibliographic fields from the Info dictionary, when present.
    let (mut title, mut authors, mut date) = (None, None, None);
    if let Ok(mut editor) = pdf_oxide::editor::DocumentEditor::from_bytes(bytes) {
        title = editor
            .title()
            .ok()
            .flatten()
            .filter(|t| !t.trim().is_empty());
        authors = editor
            .author()
            .ok()
            .flatten()
            .filter(|a| !a.trim().is_empty());
        date = editor
            .creation_date()
            .ok()
            .flatten()
            .filter(|d| !d.trim().is_empty());
    }
    // Otherwise the first line of the first page is the best available title.
    if title.is_none() {
        title = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("--- page"))
            .filter(|l| l.len() <= 200)
            .map(str::to_string);
    }

    let coverage = if extracted == 0 {
        Coverage::None {
            reason: "the PDF carries no text layer (scanned or image-only); only metadata \
                     was recorded and nothing in it has been read"
                .into(),
        }
    } else if extracted == total {
        Coverage::Full
    } else {
        Coverage::Partial { extracted, total }
    };

    Ok(Extracted {
        text: text.trim_start().to_string(),
        title,
        authors,
        date,
        coverage,
    })
}

/// Extract the main content of a web page with a readability heuristic,
/// falling back to the page's visible text when no article can be isolated.
pub fn html(html: &str, url: Option<&str>) -> Extracted {
    if let Ok(mut readability) = dom_smoothie::Readability::new(html, url, None) {
        if let Ok(article) = readability.parse() {
            let text = article.text_content.trim().to_string();
            if !text.is_empty() {
                return Extracted {
                    text,
                    title: Some(article.title.trim().to_string()).filter(|t| !t.is_empty()),
                    authors: article.byline.filter(|b| !b.trim().is_empty()),
                    date: article.published_time.filter(|d| !d.trim().is_empty()),
                    coverage: Coverage::Full,
                };
            }
        }
        let title = readability.get_article_title().trim().to_string();
        let text = visible_text(html);
        if !text.trim().is_empty() {
            return Extracted {
                text,
                title: Some(title).filter(|t| !t.is_empty()),
                authors: None,
                date: None,
                coverage: Coverage::Full,
            };
        }
    }
    Extracted::none("the page yielded no readable text; only metadata was recorded".into())
}

/// Visible text of a page: script and style blocks removed, tags stripped,
/// the common entities decoded.
fn visible_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i..];
            let skip_to = if rest.starts_with("<script") {
                rest.find("</script>").map(|e| i + e + "</script>".len())
            } else if rest.starts_with("<style") {
                rest.find("</style>").map(|e| i + e + "</style>".len())
            } else {
                None
            };
            if let Some(end) = skip_to {
                i = end;
                continue;
            }
            match html[i..].find('>') {
                Some(end) => {
                    out.push(' ');
                    i += end + 1;
                }
                None => break,
            }
            continue;
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_text_drops_scripts_and_tags() {
        let t = visible_text(
            "<p>Hello&nbsp;<b>world</b></p><script>var x = 1;</script><style>p{}</style>",
        );
        assert_eq!(t, "Hello world");
    }

    #[test]
    fn unreadable_html_falls_back_honestly() {
        let e = html("<html><body><div>tiny</div></body></html>", None);
        assert_eq!(e.coverage, Coverage::Full);
        assert!(e.text.contains("tiny"));
        let empty = html("<html><body></body></html>", None);
        assert!(matches!(empty.coverage, Coverage::None { .. }));
    }
}
