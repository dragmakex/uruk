import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { parseRunSnapshot } from "@/lib/parse";
import { snapshotFixture } from "@/lib/fixtures";
import { EvidenceBadge } from "./EvidenceBadge";

const candidate = () => parseRunSnapshot(snapshotFixture()).items[0]!;
describe("EvidenceBadge", () => {
  it("names evidence and scope limits on hover, focus and click", () => {
    const item = candidate();
    item.assessment = "supported";
    item.assessment_basis = { rationale: "Measured on one instrument only.", review_id: "rev_1", decision_id: null,
      evidence: [{ id: "ev_1", label: "Independent drift study", kind: "retrieved_literature", relation: "supports", claim: "Drift tracks temperature", limits: "One instrument" }] };
    render(<EvidenceBadge item={item} />);
    const chip = screen.getByRole("button", { name: /Evidence for/ });
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
    fireEvent.focus(chip);
    expect(screen.getByRole("tooltip")).toHaveTextContent("Independent drift study");
    expect(screen.getByRole("tooltip")).toHaveTextContent("Limits: One instrument");
    expect(chip).toHaveAttribute("aria-describedby", screen.getByRole("tooltip").id);
    fireEvent.keyDown(chip, { key: "Escape" });
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
    fireEvent.mouseEnter(chip);
    expect(screen.getByRole("tooltip")).toHaveTextContent("rev_1");
    fireEvent.blur(chip);
    fireEvent.click(chip);
    expect(screen.getByRole("tooltip")).toHaveTextContent("ev_1");
  });
  it.each(["plausible", "untested", "refuted", "contested", "inconclusive"])("preserves %s without promoting a rating into support", (tier) => {
    const item = { ...candidate(), assessment: tier, rating: 9999 };
    render(<EvidenceBadge item={item} />);
    expect(screen.getByRole("button")).toHaveTextContent(tier);
    expect(screen.queryByText(/evidence-backed/)).not.toBeInTheDocument();
  });
});
