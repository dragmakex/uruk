import { describe, expect, it } from "vitest";
import { snapshotFixture } from "./fixtures";
import {
  ParseError,
  parseApiError,
  parseReport,
  parseRunOverviews,
  parseRunSnapshot,
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
