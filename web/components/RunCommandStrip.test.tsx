import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { snapshotFixture } from "@/lib/fixtures";
import { parseRunSnapshot } from "@/lib/parse";
import { RunCommandStrip } from "./RunCommandStrip";

function snapshot() { return parseRunSnapshot(snapshotFixture()); }

describe("RunCommandStrip", () => {
  it("shows recorded usage, counts, leader and unknown cost without estimating", () => {
    render(<RunCommandStrip snapshot={snapshot()} />);
    expect(screen.getByText("12 / 200 calls")).toBeInTheDocument();
    expect(screen.getByText("13 items")).toBeInTheDocument();
    expect(screen.getByText("34 matches")).toBeInTheDocument();
    expect(screen.getByText("Leader Elo 1,412")).toBeInTheDocument();
    expect(screen.getByText("Cost unknown")).toBeInTheDocument();
    const bar = screen.getByRole("progressbar", { name: "Model-call budget" });
    expect(bar).toHaveAttribute("aria-valuenow", "12");
    expect(bar).toHaveAttribute("aria-valuemax", "200");
    expect(bar).toHaveAttribute("data-exhausted", "false");
  });

  it("uses a dashed exhausted state, clamps the bar but keeps honest counters", () => {
    const value = snapshot();
    value.run.state = "budget-exhausted";
    value.usage.model_calls = 205;
    value.usage.cost_usd = 0.12;
    value.usage.cost_partially_unknown = true;
    render(<RunCommandStrip snapshot={value} />);
    expect(screen.getByText("205 / 200 calls")).toBeInTheDocument();
    expect(screen.getByText("$0.1200 + unknown")).toBeInTheDocument();
    expect(screen.getByRole("progressbar")).toHaveAttribute("data-exhausted", "true");
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "200");
    expect(screen.getByText("Budget exhausted")).toBeInTheDocument();
  });

  it("distinguishes a queue place and provider wait from active computation", () => {
    const value = snapshot();
    value.stats.leader = null;
    value.run.queue_position = 2;
    const { rerender } = render(<RunCommandStrip snapshot={value} />);
    expect(screen.getByText("Queued #2")).toBeInTheDocument();
    expect(screen.getByText("Leader unrated")).toBeInTheDocument();
    value.run.queue_position = null;
    value.run.provider_wait = { since: value.run.updated_at, note: null };
    rerender(<RunCommandStrip snapshot={value} />);
    expect(screen.getByText("Waiting for provider")).toBeInTheDocument();
  });
});
