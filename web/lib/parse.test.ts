import { describe, expect, it } from "vitest";
import { snapshotFixture } from "./fixtures";
import {
  ParseError,
  parseApiError,
  parseCitations,
  parseLibraryPassageHits,
  parsePassagePage,
  parseReport,
  parseRunOverviews,
  parseRunSnapshot,
  parseSourceDetail,
  parseSourcePassageHits,
  parseSources,
  parseStartedRun,
} from "./parse";

describe("parseRunSnapshot", () => {
  it("accepts a complete snapshot and keeps every fact", () => {
    const snap = parseRunSnapshot(snapshotFixture());
    expect(snap.run.id).toBe("run_0412aa");
    expect(snap.run.state).toBe("running");
    expect(snap.goal.budget.max_iterations).toBe(4);
    expect(snap.agents).toHaveLength(2);
    expect(snap.agents[0]?.assignments?.[0]?.role).toBe("generation");
    expect(snap.agents[1]?.current?.strategy).toBe("literature");
    expect(snap.stats.leader?.rating).toBeCloseTo(1412.2);
    expect(snap.usage.cost_usd).toBeNull();
    expect(snap.items[0]?.review_count).toBe(3);
  });

  it("rejects a payload missing the run object", () => {
    const bad = { goal: {} };
    expect(() => parseRunSnapshot(bad)).toThrow(ParseError);
  });

  it("rejects a snapshot whose agents are not an array", () => {
    const bad = { ...(snapshotFixture() as Record<string, unknown>), agents: "nope" };
    expect(() => parseRunSnapshot(bad)).toThrow(/agents/);
  });

  it("rejects non-numeric stats instead of coercing", () => {
    const fixture = snapshotFixture() as { stats: Record<string, unknown> };
    fixture.stats.matches = "34";
    expect(() => parseRunSnapshot(fixture)).toThrow(/matches/);
  });

  it("tolerates an absent plan (a run can predate planning)", () => {
    const fixture = snapshotFixture() as Record<string, unknown>;
    fixture.plan = null;
    const snap = parseRunSnapshot(fixture);
    expect(snap.plan).toBeNull();
  });
});

describe("parseRunOverviews", () => {
  it("parses the run listing payload", () => {
    const runs = parseRunOverviews({
      ok: true,
      runs: [
        {
          id: "run_1",
          state: "completed",
          stop_condition: "deliverable_satisfied",
          iterations: 1,
          question: "q",
          mode: "task",
          created_at: "2026-10-04T14:00:00Z",
          updated_at: "2026-10-04T14:05:00Z",
        },
      ],
    });
    expect(runs).toHaveLength(1);
    expect(runs[0]?.state).toBe("completed");
    expect(runs[0]?.stop_condition).toBe("deliverable_satisfied");
  });

  it("rejects a listing without a runs array", () => {
    expect(() => parseRunOverviews({ ok: true })).toThrow(ParseError);
  });
});

describe("parseSources", () => {
  it("parses sources with origin and access facts", () => {
    const sources = parseSources({
      ok: true,
      sources: [
        {
          id: "src_1",
          run_id: "run_1",
          origin: { kind: "local_file", at: "paper.pdf" },
          title: "A paper",
          authors: "Cho et al.",
          date: "2023",
          identifier: null,
          retrieved_at: "2026-10-04T14:00:00Z",
          content_hash: "abc123",
          access: "full_text",
          access_limitations: null,
          text_artifact: "art_1",
        },
      ],
    });
    expect(sources[0]?.access).toBe("full_text");
    expect(sources[0]?.origin).toEqual({ kind: "local_file", at: "paper.pdf" });
  });

  it("accepts the inline 'supplied' origin which has no location", () => {
    const sources = parseSources({
      ok: true,
      sources: [
        {
          id: "src_2",
          run_id: "run_1",
          origin: { kind: "supplied" },
          title: null,
          authors: null,
          date: null,
          identifier: null,
          retrieved_at: "2026-10-04T14:00:00Z",
          content_hash: "def",
          access: "metadata_only",
          access_limitations: "pasted text only",
          text_artifact: null,
        },
      ],
    });
    expect(sources[0]?.origin).toEqual({ kind: "supplied", at: null });
  });
});

describe("parseReport", () => {
  it("returns markdown and the optional manifest", () => {
    const report = parseReport({
      ok: true,
      run_id: "run_1",
      markdown: "# Report\n",
      manifest: { state: "completed", partial: false },
    });
    expect(report.markdown).toContain("# Report");
    expect(report.manifest).toEqual({ state: "completed", partial: false });
  });

  it("requires markdown to be a string", () => {
    expect(() => parseReport({ ok: true, run_id: "r", markdown: 7 })).toThrow(/markdown/);
  });
});

describe("parseStartedRun and parseApiError", () => {
  it("extracts the new run id", () => {
    expect(parseStartedRun({ ok: true, run_id: "run_9" })).toBe("run_9");
  });

  it("reads the stable error body shape", () => {
    const err = parseApiError({ ok: false, error: "goal must not be empty", kind: "validation" });
    expect(err).toEqual({ error: "goal must not be empty", kind: "validation" });
  });

  it("falls back to an unknown kind for non-conforming bodies", () => {
    const err = parseApiError("<html>502</html>");
    expect(err.kind).toBe("unknown");
    expect(err.error.length).toBeGreaterThan(0);
  });

  it("describes an empty or null body instead of stringifying it", () => {
    for (const body of [null, undefined]) {
      const err = parseApiError(body);
      expect(err.kind).toBe("unknown");
      expect(err.error).toBe("unrecognized error response");
    }
  });
});

function sourcePayload(): Record<string, unknown> {
  return {
    id: "src_1",
    run_id: "run_1",
    origin: { kind: "url", at: "https://example.org/p" },
    title: "A paper",
    authors: null,
    date: null,
    identifier: null,
    retrieved_at: "2026-10-04T14:00:00Z",
    content_hash: "abc123",
    access: "full_text",
    access_limitations: null,
    text_artifact: "art_1",
  };
}

describe("parseSourceDetail", () => {
  it("parses the source record with its passage count", () => {
    const detail = parseSourceDetail({
      ok: true,
      run_id: "run_1",
      source: sourcePayload(),
      passage_count: 12,
    });
    expect(detail.run_id).toBe("run_1");
    expect(detail.source.id).toBe("src_1");
    expect(detail.passage_count).toBe(12);
  });

  it("rejects a non-numeric passage count", () => {
    expect(() =>
      parseSourceDetail({
        ok: true,
        run_id: "run_1",
        source: sourcePayload(),
        passage_count: "12",
      }),
    ).toThrow(/passage_count/);
  });
});

describe("parsePassagePage", () => {
  it("parses a browse page with its totals and byte offsets", () => {
    const page = parsePassagePage({
      ok: true,
      run_id: "run_1",
      source_id: "src_1",
      total: 40,
      offset: 20,
      limit: 20,
      passages: [{ seq: 20, byte_start: 100, byte_end: 180, text: "…" }],
    });
    expect(page.total).toBe(40);
    expect(page.offset).toBe(20);
    expect(page.passages[0]?.byte_end).toBe(180);
  });

  it("rejects a passage without byte offsets", () => {
    expect(() =>
      parsePassagePage({
        ok: true,
        total: 1,
        offset: 0,
        limit: 20,
        passages: [{ seq: 0, text: "x" }],
      }),
    ).toThrow(/byte_start/);
  });
});

describe("parseSourcePassageHits", () => {
  it("parses ranked hits with snippet and score", () => {
    const hits = parseSourcePassageHits({
      ok: true,
      query: "heat",
      passages: [
        {
          source_id: "src_1",
          seq: 3,
          byte_start: 10,
          byte_end: 90,
          text: "full passage",
          snippet: "…heat…",
          score: 1.25,
        },
      ],
    });
    expect(hits[0]?.score).toBeCloseTo(1.25);
    expect(hits[0]?.snippet).toContain("heat");
  });
});

describe("parseLibraryPassageHits", () => {
  it("parses owner-wide hits naming run and source", () => {
    const hits = parseLibraryPassageHits({
      ok: true,
      query: "heat",
      passages: [
        {
          run_id: "run_1",
          source_id: "src_1",
          source_title: "A paper",
          seq: 3,
          byte_start: 10,
          byte_end: 90,
          text: "full passage",
          snippet: "…heat…",
          score: 1.25,
        },
      ],
    });
    expect(hits[0]?.run_id).toBe("run_1");
    expect(hits[0]?.source_title).toBe("A paper");
  });

  it("tolerates an untitled source", () => {
    const hits = parseLibraryPassageHits({
      ok: true,
      query: "x",
      passages: [
        {
          run_id: "run_1",
          source_id: "src_1",
          source_title: null,
          seq: 0,
          byte_start: 0,
          byte_end: 5,
          text: "x",
          snippet: "x",
          score: 0.5,
        },
      ],
    });
    expect(hits[0]?.source_title).toBeNull();
  });
});

describe("parseCitations", () => {
  it("parses each of the three honest resolution shapes", () => {
    const citations = parseCitations({
      ok: true,
      run_id: "run_1",
      citations: [
        {
          origin: { kind: "deliverable_claim", basis: "source_reported" },
          claim: "the alloy melts at 900K",
          source_id: "src_1",
          source_title: "A paper",
          locator: "chars 10..90",
          resolution: {
            kind: "span",
            start: 10,
            end: 90,
            text: "the exact cited bytes",
            truncated: false,
          },
        },
        {
          origin: { kind: "review_observation", review_id: "rev_1" },
          claim: "figure 3 contradicts this",
          source_id: "src_1",
          source_title: null,
          locator: "p. 4",
          resolution: { kind: "source_locator" },
        },
        {
          origin: { kind: "deliverable_claim", basis: "model_asserted" },
          claim: "ghost claim",
          source_id: "src_404",
          source_title: null,
          locator: null,
          resolution: {
            kind: "unresolved",
            reason: "the cited source src_404 is not recorded by this run",
          },
        },
      ],
    });
    expect(citations).toHaveLength(3);
    expect(citations[0]?.resolution).toEqual({
      kind: "span",
      start: 10,
      end: 90,
      text: "the exact cited bytes",
      truncated: false,
    });
    expect(citations[1]?.origin).toEqual({
      kind: "review_observation",
      review_id: "rev_1",
    });
    expect(citations[2]?.resolution.kind).toBe("unresolved");
  });

  it("rejects an unknown resolution kind instead of guessing", () => {
    expect(() =>
      parseCitations({
        ok: true,
        citations: [
          {
            origin: { kind: "deliverable_claim", basis: "source_reported" },
            claim: "c",
            source_id: "s",
            source_title: null,
            locator: null,
            resolution: { kind: "approximate", text: "fabricated" },
          },
        ],
      }),
    ).toThrow(/resolution/);
  });

  it("rejects an unknown origin kind", () => {
    expect(() =>
      parseCitations({
        ok: true,
        citations: [
          {
            origin: { kind: "vibes" },
            claim: "c",
            source_id: "s",
            source_title: null,
            locator: null,
            resolution: { kind: "source_locator" },
          },
        ],
      }),
    ).toThrow(/origin/);
  });
});
