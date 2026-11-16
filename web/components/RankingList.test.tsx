import { cleanup, render, screen } from "@testing-library/react";
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

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("RankingList", () => {
  it("says nothing about movement before it has seen a second board", () => {
    const { container } = render(<RankingList items={first} />);
    expect(container.querySelector(".rank-move")).toBeNull();
  });

  it("marks the places each row gained or lost when the board reorders", () => {
    const { container, rerender } = render(<RankingList items={first} />);
    rerender(<RankingList items={reopened} />);

    expect(screen.getAllByText("▲1").length).toBeGreaterThan(0);
    expect(screen.getAllByText("▼1").length).toBeGreaterThan(0);
    expect(container.querySelector(".rank-move.is-up")).not.toBeNull();
    expect(container.querySelector(".rank-move.is-down")).not.toBeNull();
  });

  it("keeps the count after the reorder, so a late reader still sees it", () => {
    const { rerender } = render(<RankingList items={first} />);
    rerender(<RankingList items={reopened} />);
    // A snapshot that changed nothing else must not wipe the movement.
    rerender(<RankingList items={[...reopened]} />);
    expect(screen.getAllByText("▲1").length).toBeGreaterThan(0);
  });

  it("calls a newcomer new rather than pretending it climbed", () => {
    const { container, rerender } = render(<RankingList items={first} />);
    rerender(<RankingList items={reopened} />);
    const mark = container.querySelector(".rank-move.is-new");
    expect(mark).not.toBeNull();
    expect(mark?.textContent).toContain("not on the board at the last update");
  });

  it("keeps the place a row holds out of the way", () => {
    const { container, rerender } = render(<RankingList items={first} />);
    // a and b swap; c is untouched at third.
    rerender(<RankingList items={[first[1]!, first[0]!, first[2]!]} />);
    const rows = Array.from(container.querySelectorAll(".item-row"));
    const held = rows.find((row) => row.textContent?.includes("Intron retention"));
    expect(held?.querySelector(".rank-move")).toBeNull();
  });

  it("slides rows that moved, and only when the browser can animate", () => {
    // jsdom reports a zero box for every element, so stand in a stack of
    // rows whose tops follow their position in the list.
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      function (this: HTMLElement) {
        const siblings = this.parentElement
          ? Array.from(this.parentElement.children).indexOf(this)
          : 0;
        return rowBox(siblings * 40);
      },
    );
    const animate = vi.fn();
    Element.prototype.animate = animate as unknown as Element["animate"];

    const { rerender } = render(<RankingList items={first} />);
    animate.mockClear();
    rerender(<RankingList items={reopened} />);

    expect(animate).toHaveBeenCalled();
    const [frames] = animate.mock.calls[0] as [Array<{ transform: string }>];
    expect(frames[0]?.transform).toMatch(/^translateY\(-?\d+px\)$/);
    delete (Element.prototype as { animate?: unknown }).animate;
  });

  it("still shows the rating and unrated status as before", () => {
    render(<RankingList items={reopened} />);
    expect(screen.getByText(/elo 1490/)).toBeDefined();
    expect(screen.getByText("unrated")).toBeDefined();
  });
});
