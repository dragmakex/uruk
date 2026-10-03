//! Deterministic, offset-preserving chunking of extracted text.
//!
//! The artifact file remains the canonical full text: every chunk carries
//! byte offsets into it, so a passage maps back to an exact span that
//! `Locator::Span` citations can cite. No overlap: overlap complicates
//! span-based citation and dedup of quoted evidence, and BM25 on
//! paragraph-aligned chunks does not need it.

/// One chunk of an artifact's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub seq: u32,
    /// Byte offsets into the UTF-8 artifact text, on char boundaries.
    pub byte_start: usize,
    pub byte_end: usize,
    pub text: String,
}

/// Chunking parameters, in bytes of UTF-8 text.
#[derive(Debug, Clone)]
pub struct ChunkConfig {
    /// Greedy packing target (~400-450 tokens at ~4 bytes/token).
    pub target: usize,
    /// A paragraph that would push a chunk past this starts a new chunk;
    /// single sentences beyond it are hard-split. Values below 4 are treated
    /// as 4: a hard split must always clear a multibyte character, or it
    /// could fail to advance.
    pub max: usize,
    /// A trailing chunk smaller than this merges into its predecessor.
    pub min_tail: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target: 1_800,
            max: 2_400,
            min_tail: 200,
        }
    }
}

/// Split `text` into deterministic chunks. Pure; same input, same output.
pub fn chunk_text(text: &str, cfg: &ChunkConfig) -> Vec<Chunk> {
    // See `ChunkConfig::max`: below 4 bytes a hard split could land on the
    // same boundary forever.
    let max = cfg.max.max(4);
    let paragraphs = split_paragraphs(text);
    if paragraphs.is_empty() {
        return vec![];
    }

    // Oversized paragraphs are split before packing, so packing only ever
    // sees pieces that fit within `max`.
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    for (start, end) in paragraphs {
        if end - start <= max {
            pieces.push((start, end));
        } else {
            split_oversized(text, start, end, max, &mut pieces);
        }
    }

    // Greedy packing: whole pieces while the chunk stays within target.
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    for (start, end) in pieces {
        match current {
            None => current = Some((start, end)),
            Some((cs, ce)) => {
                let packed_len = end - cs;
                if ce - cs <= cfg.target && packed_len <= max {
                    current = Some((cs, end));
                } else {
                    chunks.push((cs, ce));
                    current = Some((start, end));
                }
            }
        }
    }
    if let Some(span) = current {
        chunks.push(span);
    }

    // Runt rule: a trailing chunk below min_tail merges into its predecessor
    // unless it is the only chunk.
    if chunks.len() >= 2 {
        let (ls, le) = chunks[chunks.len() - 1];
        if le - ls < cfg.min_tail {
            let n = chunks.len();
            chunks[n - 2].1 = le;
            chunks.truncate(n - 1);
        }
    }

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, (start, end))| Chunk {
            seq: i as u32,
            byte_start: start,
            byte_end: end,
            text: text[start..end].to_string(),
        })
        .collect()
}

/// Paragraph spans: maximal runs of non-blank content separated by runs of
/// two or more newlines (with optional interleaved whitespace). Offsets are
/// trimmed to the paragraph content itself.
fn split_paragraphs(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Skip leading whitespace between paragraphs.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        // Advance until a blank line (>= 2 newlines separated only by
        // horizontal whitespace) or end of text.
        let mut end = i;
        while end < bytes.len() {
            if bytes[end] == b'\n' {
                let mut j = end + 1;
                let mut newlines = 1;
                while j < bytes.len()
                    && (bytes[j] == b'\n'
                        || bytes[j] == b'\r'
                        || bytes[j] == b' '
                        || bytes[j] == b'\t')
                {
                    if bytes[j] == b'\n' {
                        newlines += 1;
                    }
                    j += 1;
                }
                if newlines >= 2 {
                    break;
                }
            }
            end += 1;
        }
        // Trim trailing whitespace from the paragraph span.
        let mut trimmed_end = end;
        while trimmed_end > start && bytes[trimmed_end - 1].is_ascii_whitespace() {
            trimmed_end -= 1;
        }
        if trimmed_end > start {
            out.push((start, trimmed_end));
        }
        i = end;
    }
    out
}

/// Split one oversized paragraph at sentence boundaries; a single sentence
/// beyond `max` is hard-split at the last UTF-8 boundary within `max`.
fn split_oversized(
    text: &str,
    start: usize,
    end: usize,
    max: usize,
    out: &mut Vec<(usize, usize)>,
) {
    let sentences = split_sentences(text, start, end);
    let mut cur_start = start;
    let mut cur_end = start;
    for (ss, se) in sentences {
        if se - cur_start > max && cur_end > cur_start {
            out.push((cur_start, cur_end));
            cur_start = ss;
        }
        if se - cur_start > max {
            // A single sentence exceeds max: hard-split on char boundaries.
            let mut piece_start = cur_start.max(ss);
            while se - piece_start > max {
                let split = last_char_boundary(text, piece_start + max);
                out.push((piece_start, split));
                piece_start = split;
            }
            cur_start = piece_start;
        }
        cur_end = se;
    }
    if cur_end > cur_start {
        out.push((cur_start, cur_end));
    }
}

/// Sentence spans within `[start, end)`, using a deterministic ASCII
/// heuristic: `.`, `?`, or `!` followed by whitespace then an uppercase
/// letter, a digit, or a newline ends a sentence.
fn split_sentences(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut sent_start = start;
    let mut i = start;
    while i < end {
        if matches!(bytes[i], b'.' | b'?' | b'!') {
            let mut j = i + 1;
            while j < end && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            let boundary = j < end
                && j > i + 1
                && (bytes[j].is_ascii_uppercase()
                    || bytes[j].is_ascii_digit()
                    || bytes[j] == b'\n');
            if boundary {
                out.push((sent_start, j));
                sent_start = j;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    if sent_start < end {
        out.push((sent_start, end));
    }
    out
}

/// The largest char boundary `<= at` that is strictly greater than zero.
fn last_char_boundary(text: &str, at: usize) -> usize {
    let mut i = at.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ChunkConfig {
        ChunkConfig::default()
    }

    #[test]
    fn chunking_is_deterministic_and_offsets_round_trip() {
        let text = "First paragraph with several words.\n\nSecond paragraph here.\n\n\nThird one.";
        let a = chunk_text(text, &cfg());
        let b = chunk_text(text, &cfg());
        assert_eq!(a, b, "same input must yield identical chunks");
        assert!(!a.is_empty());
        for c in &a {
            assert_eq!(&text[c.byte_start..c.byte_end], c.text, "offset round-trip");
        }
    }

    #[test]
    fn small_paragraphs_pack_into_one_chunk() {
        let text = "Alpha.\n\nBeta.\n\nGamma.";
        let chunks = chunk_text(text, &cfg());
        assert_eq!(chunks.len(), 1, "{chunks:?}");
        assert_eq!(chunks[0].seq, 0);
        assert!(chunks[0].text.contains("Alpha") && chunks[0].text.contains("Gamma"));
    }

    #[test]
    fn target_overflow_starts_a_new_chunk() {
        let paragraph = "word ".repeat(300); // ~1500 bytes
        let text = format!("{p}\n\n{p}\n\n{p}", p = paragraph.trim_end());
        let chunks = chunk_text(&text, &cfg());
        assert!(
            chunks.len() >= 2,
            "three 1.5 KiB paragraphs cannot fit one chunk"
        );
        for c in &chunks {
            assert!(c.byte_end - c.byte_start <= cfg().max, "chunk exceeds max");
            assert_eq!(&text[c.byte_start..c.byte_end], c.text);
        }
        let seqs: Vec<u32> = chunks.iter().map(|c| c.seq).collect();
        let expected: Vec<u32> = (0..chunks.len() as u32).collect();
        assert_eq!(seqs, expected);
    }

    #[test]
    fn oversized_paragraph_splits_at_sentence_boundaries() {
        let sentence = format!("{}. ", "X".repeat(500));
        let text = format!("{0}{0}{0}{0}{0}{0}", sentence); // one ~3 KiB paragraph
        let chunks = chunk_text(&text, &cfg());
        assert!(chunks.len() >= 2, "{}", chunks.len());
        for c in &chunks {
            assert!(c.byte_end - c.byte_start <= cfg().max);
            assert_eq!(&text[c.byte_start..c.byte_end], c.text);
        }
    }

    #[test]
    fn single_giant_sentence_is_hard_split_on_utf8_boundaries() {
        // Multibyte text with no sentence boundaries at all.
        let text = "é".repeat(3_000); // 6000 bytes, 2-byte chars
        let chunks = chunk_text(&text, &cfg());
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(text.is_char_boundary(c.byte_start));
            assert!(text.is_char_boundary(c.byte_end));
            assert!(c.byte_end - c.byte_start <= cfg().max);
            assert_eq!(&text[c.byte_start..c.byte_end], c.text);
        }
        // Contiguous coverage: hard splits lose no bytes.
        for w in chunks.windows(2) {
            assert_eq!(w[0].byte_end, w[1].byte_start);
        }
    }

    #[test]
    fn trailing_runt_merges_into_predecessor() {
        let big = "word ".repeat(400); // ~2000 bytes, fills a chunk
        let text = format!("{}\n\ntiny tail", big.trim_end());
        let chunks = chunk_text(&text, &cfg());
        assert_eq!(chunks.len(), 1, "{chunks:?}");
        assert!(chunks[0].text.ends_with("tiny tail"));
    }

    #[test]
    fn a_lone_runt_is_kept() {
        let chunks = chunk_text("tiny", &cfg());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "tiny");
    }

    #[test]
    fn degenerate_max_terminates_and_still_covers_multibyte_text() {
        // Regression: `max` below one UTF-8 character used to make the hard
        // split fail to advance. It must clamp to 4 and terminate.
        let text = "é".repeat(50); // 100 bytes of 2-byte chars, no sentences
        let tiny = ChunkConfig {
            target: 1,
            max: 1,
            min_tail: 0,
        };
        let chunks = chunk_text(&text, &tiny);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(c.byte_end > c.byte_start, "every chunk advances");
            assert!(c.byte_end - c.byte_start <= 4, "clamped max");
            assert_eq!(&text[c.byte_start..c.byte_end], c.text);
        }
        for w in chunks.windows(2) {
            assert_eq!(w[0].byte_end, w[1].byte_start, "no bytes lost");
        }
    }

    #[test]
    fn empty_and_blank_input_yield_no_chunks() {
        assert!(chunk_text("", &cfg()).is_empty());
        assert!(chunk_text("\n\n \n\t\n", &cfg()).is_empty());
    }
}
