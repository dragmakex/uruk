import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { snapshotFixture } from "@/lib/fixtures";
import { parseRunSnapshot } from "@/lib/parse";
import type { AgentPanel, Stats } from "@/lib/types";
import { activityText, AgentCard, footerStat } from "./AgentCard";

const snapshot = parseRunSnapshot(snapshotFixture());
const stats: Stats = snapshot.stats;

function panel(overrides: Partial<AgentPanel>): AgentPanel {
  return {
    role: "reflection",
    state: "idle",
    counts: {
      pending: 0,
      blocked: 0,
      running: 0,
      waiting_for_human: 0,
      completed: 0,
      failed: 0,
      cancelled: 0,
      uncertain: 0,
    },
    current: null,
    last_finished: null,
    assignments: null,
    ...overrides,
  };
}

describe("activityText", () => {
  it("describes the running task from recorded facts", () => {
    const generation = snapshot.agents[1];
    expect(generation).toBeDefined();
    expect(activityText(generation!)).toBe("Running literature.");
  });

  it("admits when there is no work instead of inventing activity", () => {
    expect(activityText(panel({}))).toBe("No work yet this run.");
  });

  it("reports queued work and human gates", () => {
    expect(
      activityText(panel({ counts: { ...panel({}).counts, pending: 3 } })),
    ).toBe("3 tasks queued.");
    expect(
      activityText(
        panel({ counts: { ...panel({}).counts, waiting_for_human: 1 } }),
      ),
    ).toBe("1 task waiting for your approval.");
  });

  it("falls back to the last finished task when idle", () => {
    expect(
      activityText(
        panel({
          last_finished: {
            strategy: "initial",
            state: "completed",
            attempts: 1,
            started_at: null,
            finished_at: "2026-10-04T14:00:00Z",
            error: null,
          },
        }),
      ),
    ).toBe("Last task: initial, completed.");
  });
});

describe("footerStat", () => {
  it("shows run-wide counts per role", () => {
    expect(footerStat(panel({ role: "generation" }), stats)).toBe("13 items");
    expect(footerStat(panel({ role: "reflection" }), stats)).toBe("4 reviews");
    expect(footerStat(panel({ role: "ranking" }), stats)).toBe("34 matches");
    expect(footerStat(panel({ role: "proximity" }), stats)).toBe("4 clusters");
    expect(footerStat(panel({ role: "supervisor" }), stats)).toBe(
      "3 of 4 workers busy",
    );
  });

  it("counts meta-review summaries from its completed tasks", () => {
    const meta = panel({
      role: "meta_review",
      counts: { ...panel({}).counts, completed: 1 },
    });
    expect(footerStat(meta, stats)).toBe("1 summary");
  });
});

describe("AgentCard", () => {
  it("renders the role, a text state label, and the footer stat", () => {
    const generation = snapshot.agents[1]!;
    render(<AgentCard panel={generation} stats={stats} />);
    expect(screen.getByRole("heading", { name: "Generation" })).toBeInTheDocument();
    expect(screen.getByText("thinking")).toBeInTheDocument();
    expect(screen.getByText("13 items")).toBeInTheDocument();
  });

  it("renders the supervisor assignment table", () => {
    const supervisor = snapshot.agents[0]!;
    render(<AgentCard panel={supervisor} stats={stats} />);
    expect(screen.getByText("Assignments")).toBeInTheDocument();
    expect(screen.getByText("3 open, 1 running")).toBeInTheDocument();
  });

  it("marks idle panels with the dashed-box class", () => {
    const { container } = render(<AgentCard panel={panel({})} stats={stats} />);
    expect(container.querySelector(".agent")).toHaveClass("is-idle");
  });

  it("surfaces a failed task's recorded error", () => {
    const failing = panel({
      counts: { ...panel({}).counts, failed: 2 },
      last_finished: {
        strategy: "deep_verification",
        state: "failed",
        attempts: 3,
        started_at: null,
        finished_at: "2026-10-04T14:00:00Z",
        error: "provider timeout (attempts exhausted: 3)",
      },
    });
    render(<AgentCard panel={failing} stats={stats} />);
    expect(
      screen.getByText(/2 failed: provider timeout/),
    ).toBeInTheDocument();
  });
});
