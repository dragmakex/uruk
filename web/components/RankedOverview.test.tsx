import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { snapshotFixture } from "@/lib/fixtures";
import { parseRunSnapshot } from "@/lib/parse";
import { RankedOverview } from "./RankedOverview";

describe("RankedOverview", () => {
  it("keeps API order, caps at five, and excludes archived and non-rankable records", () => {
    const snapshot = parseRunSnapshot(snapshotFixture());
    const first = snapshot.items[0]!;
    snapshot.items = [
      { ...first, id: "archived", disposition: "archived" },
      { ...first, id: "report", kind: "synthesis" },
      ...Array.from({ length: 6 }, (_, i) => ({ ...first, id: `item-${i}`, title: `Candidate ${i}`, rating: 1500 - i })),
    ];
    render(<RankedOverview snapshot={snapshot} />);
    const rows = within(screen.getByRole("list")).getAllByRole("listitem");
    expect(rows).toHaveLength(5);
    expect(rows[0]).toHaveTextContent("Candidate 0");
    expect(screen.queryByText("Candidate 5")).not.toBeInTheDocument();
    expect(screen.queryByText("supported / evidence-backed")).not.toBeInTheDocument();
  });

  it("keeps a real zero distinct from an unknown baseline and marks stale ratings", () => {
    const snapshot = parseRunSnapshot(snapshotFixture());
    snapshot.ranking_baseline!.ratings.item_h7 = 1412.2;
    snapshot.items[0]!.stale_rating = true;
    const { rerender } = render(<RankedOverview snapshot={snapshot} />);
    expect(screen.getByText("Δ 0 (stale)")).toBeInTheDocument();
    rerender(<RankedOverview snapshot={{ ...snapshot, ranking_baseline: null }} />);
    expect(screen.getByText("Δ unavailable (stale)")).toBeInTheDocument();
  });

  it("shows an honest empty state", () => {
    const snapshot = parseRunSnapshot(snapshotFixture());
    render(<RankedOverview snapshot={{ ...snapshot, items: [] }} />);
    expect(screen.getByText(/No candidates yet/)).toBeInTheDocument();
    expect(screen.queryByRole("list")).not.toBeInTheDocument();
  });
});
