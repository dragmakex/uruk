import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ItemCard } from "@/lib/types";
import { RankingList } from "./RankingList";

function item(id: string, title: string, rating: number | null): ItemCard {
  return {
    id,
    title,
    kind: "hypothesis",
    disposition: "active",
    assessment: "plausible",
    rating,
    matches_played: rating === null ? 0 : 3,
    stale_rating: false,
    review_count: 0,
  };
}

const first = [
  item("item_a", "XBP1 splicing escape", 1463.1),
  item("item_b", "Splicing factor titration", 1441.8),
  item("item_c", "Intron retention buildup", 1402.4),
];

const reopened = [
  item("item_b", "Splicing factor titration", 1490.2),
  item("item_a", "XBP1 splicing escape", 1439.5),
  item("item_c", "Intron retention buildup", 1402.4),
  item("item_d", "Ribosome stalling", null),
];

// The board itself never reads the API; a row that is opened does. The
// stub stands in for that one read so the disclosure is covered here too.
vi.mock("@/lib/api", () => ({
  fetchCandidateHistory: () =>
    Promise.resolve({
      run_id: "run_test",
      item_id: "item_a",
      title: "XBP1 splicing escape",
      matches: [
        {
          id: "m1",
          opponent_id: "item_b",
          opponent_title: "Splicing factor titration",
          result: "lost",
          method: "pairwise",
          judge_model: "judge-test",
          rationale: "Titration accounts for the same data with fewer steps.",
          rating_before: 1463.1,
          rating_after: 1447.1,
          delta: -16,
          created_at: "2026-01-01T00:00:00Z",
          produced_by: null,
        },
      ],
    }),
}));

function rowBox(top: number): DOMRect {
  return {
    top,
    left: 0,
    width: 600,
    height: 40,
    right: 600,
    bottom: top + 40,
    x: 0,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

function openCandidates() {
  const toggle = screen.getByRole("button", { name: "Open candidates" });
  expect(toggle.textContent).toBe("Open");
  fireEvent.click(toggle);
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("RankingList", () => {
  it("says nothing about movement before it has seen a second board", () => {
    const { container } = render(<RankingList runId="run_test" items={first} />);
    openCandidates();
    expect(container.querySelector(".rank-move")).toBeNull();
  });

  it("marks the places each row gained or lost when the board reorders", () => {
    const { container, rerender } = render(<RankingList runId="run_test" items={first} />);
    openCandidates();
    rerender(<RankingList runId="run_test" items={reopened} />);

    expect(screen.getAllByText("▲1").length).toBeGreaterThan(0);
    expect(screen.getAllByText("▼1").length).toBeGreaterThan(0);
    expect(container.querySelector(".rank-move.is-up")).not.toBeNull();
    expect(container.querySelector(".rank-move.is-down")).not.toBeNull();
  });

  it("keeps the count after the reorder, so a late reader still sees it", () => {
    const { rerender } = render(<RankingList runId="run_test" items={first} />);
    openCandidates();
    rerender(<RankingList runId="run_test" items={reopened} />);
    // A snapshot that changed nothing else must not wipe the movement.
    rerender(<RankingList runId="run_test" items={[...reopened]} />);
    expect(screen.getAllByText("▲1").length).toBeGreaterThan(0);
  });

  it("calls a newcomer new rather than pretending it climbed", () => {
    const { container, rerender } = render(<RankingList runId="run_test" items={first} />);
    openCandidates();
    rerender(<RankingList runId="run_test" items={reopened} />);
    const mark = container.querySelector(".rank-move.is-new");
    expect(mark).not.toBeNull();
    expect(mark?.textContent).toContain("not on the board at the last update");
  });

  it("keeps the place a row holds out of the way", () => {
    const { container, rerender } = render(<RankingList runId="run_test" items={first} />);
    openCandidates();
    // a and b swap; c is untouched at third.
    rerender(<RankingList runId="run_test" items={[first[1]!, first[0]!, first[2]!]} />);
    const rows = Array.from(container.querySelectorAll(".item-row"));
    const held = rows.find((row) => row.textContent?.includes("Intron retention"));
    expect(held?.querySelector(".rank-move")).toBeNull();
  });

  it("slides rows that moved, and only when the browser can animate", () => {
    // jsdom reports a zero box for every element, so stand in a stack of
    // rows whose tops follow their position in the list.
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      function (this: HTMLElement) {
        // Rows live inside their own disclosure, so their place is their
        // order in the board, not their order among siblings.
        const board = Array.from(document.querySelectorAll(".item-row"));
        return rowBox(board.indexOf(this) * 40);
      },
    );
    const animate = vi.fn();
    Element.prototype.animate = animate as unknown as Element["animate"];

    const { rerender } = render(<RankingList runId="run_test" items={first} />);
    openCandidates();
    animate.mockClear();
    rerender(<RankingList runId="run_test" items={reopened} />);

    expect(animate).toHaveBeenCalled();
    const [frames] = animate.mock.calls[0] as [Array<{ transform: string }>];
    expect(frames[0]?.transform).toMatch(/^translateY\(-?\d+px\)$/);
    delete (Element.prototype as { animate?: unknown }).animate;
  });

  it("still shows the rating and unrated status as before", () => {
    render(<RankingList runId="run_test" items={reopened} />);
    openCandidates();
    expect(screen.getByText(/elo 1490/)).toBeDefined();
    expect(screen.getByText("unrated")).toBeDefined();
  });

  it("opens a row onto that candidate's own matches, and only then", async () => {
    const { container } = render(<RankingList runId="run_test" items={first} />);
    openCandidates();
    // A closed board reads nothing: no request, no empty panel.
    expect(container.querySelector(".history")).toBeNull();

    const summary = container.querySelector("summary.item-row");
    expect(summary).not.toBeNull();
    fireEvent.click(summary!);

    await waitFor(() => {
      expect(container.querySelector(".history")).not.toBeNull();
    });
    const panel = container.querySelector(".history")?.textContent ?? "";
    expect(panel).toContain("Splicing factor titration");
    expect(panel).toContain("Titration accounts for the same data");
    expect(panel).toContain("1 match · 1 lost · newest first");
  });

  it("is closed by default and pages five candidates at a time", () => {
    const many = Array.from({ length: 7 }, (_, index) =>
      item(`item_${index + 1}`, `Candidate ${index + 1}`, 1500 - index),
    );
    render(<RankingList runId="run_test" items={many} />);

    expect(screen.queryByRole("list")).toBeNull();
    openCandidates();
    expect(screen.getByRole("button", { name: "Close candidates" }).textContent).toBe("Close");
    expect(screen.getByRole("list").children).toHaveLength(5);
    expect(screen.getByText("1. Candidate 1")).toBeDefined();
    expect(screen.queryByText("6. Candidate 6")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Lower-ranked" }));
    expect(screen.getByRole("list").children).toHaveLength(2);
    expect(screen.getByText("6. Candidate 6")).toBeDefined();
    expect(screen.getByText("Page 2 of 2")).toBeDefined();
  });
});
