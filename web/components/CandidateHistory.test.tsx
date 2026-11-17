import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CandidateHistory as History } from "@/lib/types";

const fetchCandidateHistory = vi.fn();

vi.mock("@/lib/api", () => ({
  fetchCandidateHistory: (...args: unknown[]) => fetchCandidateHistory(...args),
}));

const { CandidateHistory } = await import("./CandidateHistory");

function entry(overrides: Partial<History["matches"][number]> = {}) {
  return {
    id: "match_1",
    opponent_id: "item_beta",
    opponent_title: "Beta candidate",
    result: "won" as const,
    method: "debate" as const,
    judge_model: "judge-test",
    rationale: "Alpha explains the observed effect; Beta does not.",
    rating_before: 1200,
    rating_after: 1216,
    delta: 16,
    created_at: "2026-01-01T00:00:00Z",
    produced_by: null,
    ...overrides,
  };
}

function history(overrides: Partial<History> = {}): History {
  return {
    run_id: "run_test",
    item_id: "item_alpha",
    title: "Alpha candidate",
    matches: [entry()],
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  fetchCandidateHistory.mockReset();
});

describe("CandidateHistory", () => {
  it("says it is reading, then shows what the record says", async () => {
    fetchCandidateHistory.mockResolvedValue(history());
    const { container } = render(
      <CandidateHistory runId="run_test" itemId="item_alpha" />,
    );

    expect(screen.getByText("reading history…")).toBeDefined();
    await waitFor(() => {
      expect(screen.getByText("Beta candidate")).toBeDefined();
    });
    expect(fetchCandidateHistory).toHaveBeenCalledWith("run_test", "item_alpha");

    // The verdict, the rating it moved, and the judge's own words.
    expect(container.querySelector(".history-mark")?.textContent).toBe("W");
    expect(screen.getByText("won")).toBeDefined();
    expect(screen.getByText(/1200/)).toBeDefined();
    expect(screen.getByText(/1216/)).toBeDefined();
    expect(screen.getByText("+16")).toBeDefined();
    expect(
      screen.getByText("Alpha explains the observed effect; Beta does not."),
    ).toBeDefined();
    expect(screen.getByText(/multi-turn debate/)).toBeDefined();
  });

  it("marks a loss and a draw differently from a win", async () => {
    fetchCandidateHistory.mockResolvedValue(
      history({
        matches: [
          entry({ id: "m1", result: "lost", delta: -16, rating_after: 1184 }),
          entry({
            id: "m2",
            result: "drew",
            delta: 0,
            rating_before: 1184,
            rating_after: 1184,
            opponent_title: "Gamma candidate",
          }),
          entry({
            id: "m3",
            result: "unresolved",
            delta: 0,
            opponent_title: "Delta candidate",
            rationale: "Neither side could be established from the evidence.",
          }),
        ],
      }),
    );
    const { container } = render(
      <CandidateHistory runId="run_test" itemId="item_alpha" />,
    );
    await waitFor(() => {
      expect(screen.getByText("Gamma candidate")).toBeDefined();
    });

    const marks = Array.from(container.querySelectorAll(".history-mark")).map(
      (mark) => mark.textContent,
    );
    expect(marks).toEqual(["L", "=", "–"]);
    expect(screen.getByText("-16")).toBeDefined();
    // Three zero-delta rows would be misleading; the draw and the unresolved
    // verdict both say "0" because that is what the ratings did.
    expect(screen.getAllByText("0").length).toBe(2);
  });

  it("counts the results it saw in the heading", async () => {
    fetchCandidateHistory.mockResolvedValue(
      history({
        matches: [
          entry({ id: "m1" }),
          entry({ id: "m2" }),
          entry({ id: "m3", result: "lost", delta: -16 }),
        ],
      }),
    );
    render(<CandidateHistory runId="run_test" itemId="item_alpha" />);
    await waitFor(() => {
      expect(
        screen.getByText(/3 matches · 2 won, 1 lost · newest first/),
      ).toBeDefined();
    });
  });

  it("says a candidate has not fought rather than showing an empty list", async () => {
    fetchCandidateHistory.mockResolvedValue(history({ matches: [] }));
    render(<CandidateHistory runId="run_test" itemId="item_alpha" />);
    await waitFor(() => {
      expect(screen.getByText("no recorded matches yet")).toBeDefined();
    });
    expect(screen.queryByText(/newest first/)).toBeNull();
  });

  it("says the read failed instead of pretending there were no matches", async () => {
    fetchCandidateHistory.mockRejectedValue(new Error("boom"));
    render(<CandidateHistory runId="run_test" itemId="item_alpha" />);
    await waitFor(() => {
      expect(screen.getByText("history could not be read")).toBeDefined();
    });
    expect(screen.queryByText("no recorded matches yet")).toBeNull();
  });

  it("asks once per candidate", async () => {
    fetchCandidateHistory.mockResolvedValue(history());
    const { rerender } = render(
      <CandidateHistory runId="run_test" itemId="item_alpha" />,
    );
    await waitFor(() => {
      expect(fetchCandidateHistory).toHaveBeenCalledTimes(1);
    });
    rerender(<CandidateHistory runId="run_test" itemId="item_alpha" />);
    expect(fetchCandidateHistory).toHaveBeenCalledTimes(1);
  });
});
