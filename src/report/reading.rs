//! Reader-facing source titles, safe links, and honest access descriptions.

use crate::records::*;

pub(super) fn title(source: &Source) -> String {
    let label = source
        .title
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| source.short_citation());
    let label = label
        .replace(['\n', '\r'], " ")
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    match public_url(source) {
        Some(url) => format!("[{label}](<{url}>)"),
        None => label,
    }
}

/// Keep the recorded URL, rather than guessing a DOI or inventing a link.
fn public_url(source: &Source) -> Option<&str> {
    let url = match &source.origin {
        Origin::Url(url) | Origin::Code(url) | Origin::Dataset(url) => url.as_str(),
        _ => return None,
    };
    let parsed = reqwest::Url::parse(url).ok()?;
    (matches!(parsed.scheme(), "https" | "http")
        && parsed.has_host()
        && !url.contains(['<', '>', '\n', '\r']))
    .then_some(url)
}

pub(super) fn access_priority(source: &Source) -> u8 {
    match source.access {
        AccessLevel::FullText => 0,
        AccessLevel::AbstractOnly => 1,
        AccessLevel::MetadataOnly => 2,
        AccessLevel::Unavailable => 3,
    }
}

pub(super) fn details(out: &mut String, source: &Source) {
    let credit: Vec<_> = [source.authors.as_deref(), source.date.as_deref()]
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect();
    if !credit.is_empty() {
        out.push_str(&format!("   - {}.\n", credit.join(" · ")));
    }
    let access = match source.access {
        AccessLevel::FullText => "Full text was available to this run.",
        AccessLevel::AbstractOnly => {
            "Only the abstract was available; the full paper was not checked."
        }
        AccessLevel::MetadataOnly => {
            "Only publication details were available; the contents were not read."
        }
        AccessLevel::Unavailable => "Could not be read during this run.",
    };
    out.push_str(&format!("   - {access}\n"));
    if let Some(limit) = &source.access_limitations {
        out.push_str(&format!("   - Access limitation: {limit}\n"));
    }
    if public_url(source).is_none() {
        out.push_str("   - No public reading link was recorded.\n");
    }
    if source.text_artifact.is_some()
        && matches!(
            source.access,
            AccessLevel::FullText | AccessLevel::AbstractOnly
        )
    {
        out.push_str(&format!(
            "   - [View this source in Urukai](/runs/{}/sources/{})\n",
            source.run_id, source.id
        ));
    }
}
