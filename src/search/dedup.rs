//! Work-key derivation and field-wise merge across connectors (§8).
//!
//! Pure functions, no I/O.

use super::types::{ConnectorHit, WorkKey, WorkRecord};
use crate::records::ContentHash;
use std::collections::BTreeMap;

/// Normalize a DOI: lowercase, resolver prefixes and `doi:` stripped.
pub fn normalize_doi(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_ascii_lowercase();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim().to_string();
        }
    }
    let s = s.trim();
    if s.starts_with("10.") {
        Some(s.to_string())
    } else {
        None
    }
}

/// Normalize an arXiv id: legacy `arXiv:` prefix and version suffix stripped.
pub fn normalize_arxiv_id(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    // Atom `<id>` URLs: http://arxiv.org/abs/2403.01234v3
    if let Some(idx) = s.find("arxiv.org/abs/") {
        s = &s[idx + "arxiv.org/abs/".len()..];
    }
    let lower = s.to_ascii_lowercase();
    let s = lower.strip_prefix("arxiv:").unwrap_or(&lower).trim();
    if s.is_empty() {
        return None;
    }
    // Strip a trailing vN version suffix.
    let stripped = match s.rfind('v') {
        Some(i)
            if i > 0
                && s[i + 1..].chars().all(|c| c.is_ascii_digit())
                && !s[i + 1..].is_empty() =>
        {
            &s[..i]
        }
        _ => s,
    };
    Some(stripped.to_string())
}

/// Title fingerprint normalization: diacritics folded, lowercased,
/// non-alphanumerics collapsed to single spaces. Deterministic; the
/// diacritic fold covers Latin-1 Supplement and Latin Extended-A, which is
/// what scholarly metadata overwhelmingly contains.
pub fn normalize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut last_was_space = true;
    for c in title.chars() {
        for folded in fold_char(c) {
            if folded.is_alphanumeric() {
                for lower in folded.to_lowercase() {
                    out.push(lower);
                }
                last_was_space = false;
            } else if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        }
    }
    out.trim_end().to_string()
}

/// Fold one character to its base letter(s). Identity for anything outside
/// the covered ranges.
fn fold_char(c: char) -> impl Iterator<Item = char> {
    let folded: &'static str = match c {
        'À'..='Å' | 'à'..='å' | 'Ā'..='ą' => "a",
        'Æ' | 'æ' => "ae",
        'Ç' | 'ç' | 'Ć'..='č' => "c",
        'Ď' | 'ď' | 'Đ' | 'đ' | 'Ð' | 'ð' => "d",
        'È'..='Ë' | 'è'..='ë' | 'Ē'..='ě' => "e",
        'Ĝ'..='ģ' => "g",
        'Ĥ' | 'ĥ' | 'Ħ' | 'ħ' => "h",
        'Ì'..='Ï' | 'ì'..='ï' | 'Ĩ'..='ı' => "i",
        'Ĵ' | 'ĵ' => "j",
        'Ķ' | 'ķ' | 'ĸ' => "k",
        'Ĺ'..='ł' => "l",
        'Ñ' | 'ñ' | 'Ń'..='ŋ' => "n",
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' | 'Ō'..='ő' => "o",
        'Œ' | 'œ' => "oe",
        'Ŕ'..='ř' => "r",
        'Ś'..='š' | 'ß' => "s",
        'Ţ'..='ŧ' => "t",
        'Ù'..='Ü' | 'ù'..='ü' | 'Ũ'..='ų' => "u",
        'Ŵ' | 'ŵ' => "w",
        'Ý' | 'ý' | 'ÿ' | 'Ŷ' | 'ŷ' | 'Ÿ' => "y",
        'Ź'..='ž' => "z",
        _ => return Fold::Keep(c).into_iter(),
    };
    Fold::Replace(folded.chars()).into_iter()
}

enum Fold {
    Keep(char),
    Replace(std::str::Chars<'static>),
}

impl IntoIterator for Fold {
    type Item = char;
    type IntoIter = FoldIter;
    fn into_iter(self) -> FoldIter {
        match self {
            Fold::Keep(c) => FoldIter::Once(Some(c)),
            Fold::Replace(chars) => FoldIter::Chars(chars),
        }
    }
}

enum FoldIter {
    Once(Option<char>),
    Chars(std::str::Chars<'static>),
}

impl Iterator for FoldIter {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        match self {
            FoldIter::Once(c) => c.take(),
            FoldIter::Chars(chars) => chars.next(),
        }
    }
}

/// Derive the canonical dedup key for a work (§8 priority order).
pub fn work_key(record: &WorkRecord) -> WorkKey {
    if let Some(doi) = record.doi.as_deref().and_then(normalize_doi) {
        return WorkKey(format!("doi:{doi}"));
    }
    if let Some(id) = record.arxiv_id.as_deref().and_then(normalize_arxiv_id) {
        return WorkKey(format!("arxiv:{id}"));
    }
    if let Some(pmid) = record
        .pmid
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        return WorkKey(format!("pmid:{pmid}"));
    }
    let first_author_family = record
        .authors
        .first()
        .map(|a| a.rsplit(' ').next().unwrap_or(a.as_str()).to_string())
        .unwrap_or_default();
    let fingerprint_input = format!(
        "{}|{}|{}",
        normalize_title(&record.title),
        record.year.map(|y| y.to_string()).unwrap_or_default(),
        normalize_title(&first_author_family),
    );
    let hash = ContentHash::of_str(&fingerprint_input);
    WorkKey(format!("fp:{}", &hash.as_str()[..16]))
}

/// Merge hits sharing a WorkKey field-wise.
///
/// Preference order: OpenAlex for abstracts/OA/crosswalk IDs, Crossref for
/// venue/authors/DOI metadata, arXiv for `oa_url`; first non-null wins
/// within each field's preference order.
pub fn merge_works(hits: &[(&str, &ConnectorHit)]) -> WorkRecord {
    let by: BTreeMap<&str, &WorkRecord> = hits.iter().map(|(c, h)| (*c, &h.work)).collect();
    let pick = |order: &[&str], f: &dyn Fn(&WorkRecord) -> Option<String>| -> Option<String> {
        for name in order {
            if let Some(w) = by.get(name)
                && let Some(v) = f(w)
            {
                return Some(v);
            }
        }
        // Any remaining connector, in deterministic (BTreeMap) order.
        by.values().find_map(|w| f(w))
    };

    let openalex_first = ["openalex", "crossref", "arxiv"];
    let crossref_first = ["crossref", "openalex", "arxiv"];
    let arxiv_first = ["arxiv", "openalex", "crossref"];

    let mut merged = WorkRecord {
        work_key: hits
            .first()
            .map(|(_, h)| h.work.work_key.clone())
            .unwrap_or_default(),
        title: pick(&crossref_first, &|w| {
            Some(w.title.clone()).filter(|t| !t.trim().is_empty())
        })
        .unwrap_or_default(),
        authors: vec![],
        year: None,
        venue: pick(&crossref_first, &|w| w.venue.clone()),
        doi: pick(&crossref_first, &|w| w.doi.clone()),
        arxiv_id: pick(&arxiv_first, &|w| w.arxiv_id.clone()),
        pmid: pick(&openalex_first, &|w| w.pmid.clone()),
        pmcid: pick(&openalex_first, &|w| w.pmcid.clone()),
        openalex_id: pick(&openalex_first, &|w| w.openalex_id.clone()),
        abstract_text: pick(&openalex_first, &|w| w.abstract_text.clone()),
        is_oa: None,
        oa_url: pick(&arxiv_first, &|w| w.oa_url.clone()),
        oa_pdf_url: pick(&openalex_first, &|w| w.oa_pdf_url.clone()),
        oa_landing_url: pick(&openalex_first, &|w| w.oa_landing_url.clone()),
        landing_url: pick(&openalex_first, &|w| w.landing_url.clone()),
        cited_by_count: None,
        hits: vec![],
    };
    for name in openalex_first {
        if let Some(w) = by.get(name) {
            if merged.is_oa.is_none() {
                merged.is_oa = w.is_oa;
            }
            if merged.cited_by_count.is_none() {
                merged.cited_by_count = w.cited_by_count;
            }
            if merged.year.is_none() {
                merged.year = w.year;
            }
        }
    }
    for name in crossref_first {
        if merged.authors.is_empty()
            && let Some(w) = by.get(name)
        {
            merged.authors = w.authors.clone();
        }
    }
    if merged.year.is_none() {
        merged.year = by.values().find_map(|w| w.year);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(doi: Option<&str>, arxiv: Option<&str>, pmid: Option<&str>) -> WorkRecord {
        WorkRecord {
            title: "A Study".into(),
            doi: doi.map(str::to_string),
            arxiv_id: arxiv.map(str::to_string),
            pmid: pmid.map(str::to_string),
            authors: vec!["Ada Lovelace".into()],
            year: Some(2024),
            ..Default::default()
        }
    }

    #[test]
    fn doi_normalization_handles_case_and_resolver_prefixes() {
        for raw in [
            "10.1038/S41586-024-1",
            "https://doi.org/10.1038/s41586-024-1",
            "http://dx.doi.org/10.1038/s41586-024-1",
            "doi:10.1038/s41586-024-1",
            "  DOI:10.1038/S41586-024-1  ",
        ] {
            assert_eq!(
                normalize_doi(raw).as_deref(),
                Some("10.1038/s41586-024-1"),
                "{raw}"
            );
        }
        assert_eq!(normalize_doi("not a doi"), None);
    }

    #[test]
    fn arxiv_versions_and_prefixes_are_stripped() {
        for raw in [
            "2403.01234",
            "2403.01234v3",
            "arXiv:2403.01234v1",
            "http://arxiv.org/abs/2403.01234v2",
        ] {
            assert_eq!(
                normalize_arxiv_id(raw).as_deref(),
                Some("2403.01234"),
                "{raw}"
            );
        }
        // Legacy ids keep their category; only the version goes.
        assert_eq!(
            normalize_arxiv_id("math.gt/0309136v2").as_deref(),
            Some("math.gt/0309136")
        );
    }

    #[test]
    fn key_priority_is_doi_then_arxiv_then_pmid_then_fingerprint() {
        let k = work_key(&record(Some("10.1/x"), Some("2403.1"), Some("42")));
        assert_eq!(k.as_str(), "doi:10.1/x");
        let k = work_key(&record(None, Some("2403.1"), Some("42")));
        assert_eq!(k.as_str(), "arxiv:2403.1");
        let k = work_key(&record(None, None, Some("42")));
        assert_eq!(k.as_str(), "pmid:42");
        let k = work_key(&record(None, None, None));
        assert!(k.as_str().starts_with("fp:"), "{k}");
        assert_eq!(k.as_str().len(), "fp:".len() + 16);
    }

    #[test]
    fn title_fingerprint_is_stable_under_diacritics_and_punctuation() {
        let a = WorkRecord {
            title: "Über die Entstehung: der Arten!".into(),
            authors: vec!["Jörg Müller".into()],
            year: Some(1999),
            ..Default::default()
        };
        let b = WorkRecord {
            title: "uber die entstehung der arten".into(),
            authors: vec!["Jorg Muller".into()],
            year: Some(1999),
            ..Default::default()
        };
        assert_eq!(work_key(&a), work_key(&b));

        let c = WorkRecord {
            year: Some(2000),
            ..a.clone()
        };
        assert_ne!(work_key(&a), work_key(&c), "year distinguishes");
    }

    #[test]
    fn merge_prefers_the_documented_connector_per_field() {
        use crate::records::ContentHash;
        let openalex = ConnectorHit {
            work: WorkRecord {
                title: "t".into(),
                abstract_text: Some("openalex abstract".into()),
                is_oa: Some(true),
                oa_url: Some("https://oa.example/openalex.pdf".into()),
                pmid: Some("7".into()),
                ..Default::default()
            },
            rank: 1,
            raw_hash: ContentHash::of_str("a"),
        };
        let crossref = ConnectorHit {
            work: WorkRecord {
                title: "Canonical Title".into(),
                venue: Some("Nature".into()),
                doi: Some("10.1/x".into()),
                authors: vec!["Ada Lovelace".into()],
                abstract_text: Some("crossref abstract".into()),
                ..Default::default()
            },
            rank: 2,
            raw_hash: ContentHash::of_str("b"),
        };
        let arxiv = ConnectorHit {
            work: WorkRecord {
                title: "t2".into(),
                arxiv_id: Some("2403.1".into()),
                oa_url: Some("https://arxiv.org/pdf/2403.1".into()),
                ..Default::default()
            },
            rank: 1,
            raw_hash: ContentHash::of_str("c"),
        };
        let merged = merge_works(&[
            ("openalex", &openalex),
            ("crossref", &crossref),
            ("arxiv", &arxiv),
        ]);
        assert_eq!(merged.title, "Canonical Title");
        assert_eq!(merged.venue.as_deref(), Some("Nature"));
        assert_eq!(merged.doi.as_deref(), Some("10.1/x"));
        assert_eq!(merged.abstract_text.as_deref(), Some("openalex abstract"));
        assert_eq!(
            merged.oa_url.as_deref(),
            Some("https://arxiv.org/pdf/2403.1")
        );
        assert_eq!(merged.pmid.as_deref(), Some("7"));
        assert_eq!(merged.authors, vec!["Ada Lovelace".to_string()]);
        assert_eq!(merged.is_oa, Some(true));
    }
}
