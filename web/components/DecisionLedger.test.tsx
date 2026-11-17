import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DecisionLedger as Ledger } from "@/lib/types";

const fetchDecisions = vi.fn();

vi.mock("@/lib/api", () => ({
  fetchDecisions: (...args: unknown[]) => fetchDecisions(...args),
}));

const { DecisionLedger } = await import("./DecisionLedger");

function decision(overrides: Partial<Ledger["decisions"][number]> = {}) {
  return {
    id: "decision_1",
    kind: "scheduling",
    actor: "supervisor",
    reason: "Generation has the thinnest coverage of the mechanism.",
    referenced: ["task_generation_01"],
    created_at: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}

function ledger(overrides: Partial<Ledger> = {}): Ledger {
  return {
    run_id: "run_test",
    decisions: [decision()],
    ...overrides,
  };
}

async function openDecisions() {
  const toggle = await screen.findByRole("button", { name: "Open decisions" });
  expect(toggle.textContent).toBe("Open");
  fireEvent.click(toggle);
}

afterEach(() => {
  cleanup();
  fetchDecisions.mockReset();
});

describe("DecisionLedger", () => {
  it("shows who decided, what kind, and the reason they gave", async () => {
    fetchDecisions.mockResolvedValue(ledger());
    const { container } = render(
      <DecisionLedger runId="run_test" decisionCount={1} />,
    );

    expect(screen.getByText("reading decisions…")).toBeDefined();
    await openDecisions();
    await waitFor(() => {
      expect(screen.getByText("supervisor")).toBeDefined();
    });

    expect(container.querySelector(".decision-index")?.textContent).toBe("D01");
    expect(screen.getByText("work scheduled")).toBeDefined();
    expect(
      screen.getByText("Generation has the thinnest coverage of the mechanism."),
    ).toBeDefined();
    expect(screen.getByText(/refers to task_generation_01/)).toBeDefined();
    expect(fetchDecisions).toHaveBeenCalledWith("run_test");
  });

  it("keeps an actor's own name when the actor is an agent", async () => {
    fetchDecisions.mockResolvedValue(
      ledger({
        decisions: [
          decision({
            kind: "meta_feedback",
            actor: "reflection",
            reason: "The rubric weights mechanism over plausibility.",
            referenced: [],
          }),
        ],
      }),
    );
    const { container } = render(
      <DecisionLedger runId="run_test" decisionCount={1} />,
    );
    await openDecisions();
    expect(screen.getByText("reflection")).toBeDefined();
    expect(screen.getByText("metacognitive feedback")).toBeDefined();
    // Nothing referenced, so nothing is claimed.
    expect(container.querySelector(".decision-refs")).toBeNull();
  });

  it("shows a kind this build does not know as itself rather than dropping it", async () => {
    fetchDecisions.mockResolvedValue(
      ledger({
        decisions: [decision({ kind: "evidence_replan", actor: "system" })],
      }),
    );
    const { container } = render(
      <DecisionLedger runId="run_test" decisionCount={1} />,
    );
    await openDecisions();
    expect(screen.getByText("system")).toBeDefined();
    // Unknown kinds get the underscores read as spaces, not a guess.
    expect(screen.getByText("evidence replan")).toBeDefined();
    expect(container.querySelector(".decision-row")).not.toBeNull();
  });

  it("keeps the record's order rather than re-sorting it", async () => {
    fetchDecisions.mockResolvedValue(
      ledger({
        decisions: [
          decision({ id: "d1", reason: "first choice" }),
          decision({ id: "d2", reason: "second choice" }),
        ],
      }),
    );
    const { container } = render(
      <DecisionLedger runId="run_test" decisionCount={2} />,
    );
    await openDecisions();
    expect(screen.getByText("second choice")).toBeDefined();
    const indices = Array.from(container.querySelectorAll(".decision-index")).map(
      (el) => el.textContent,
    );
    expect(indices).toEqual(["D01", "D02"]);
  });

  it("says a run has decided nothing yet, in terms of what it would record", async () => {
    fetchDecisions.mockResolvedValue(ledger({ decisions: [] }));
    render(<DecisionLedger runId="run_test" decisionCount={0} />);
    await waitFor(() => {
      expect(screen.getByText(/No decisions on record yet/)).toBeDefined();
    });
  });

  it("says the read failed instead of showing an empty ledger", async () => {
    fetchDecisions.mockRejectedValue(new Error("boom"));
    render(<DecisionLedger runId="run_test" decisionCount={0} />);
    await waitFor(() => {
      expect(screen.getByText("decisions could not be read")).toBeDefined();
    });
    expect(screen.queryByText(/No decisions on record yet/)).toBeNull();
  });

  it("re-reads the ledger when the snapshot's decision count moves", async () => {
    fetchDecisions.mockResolvedValue(ledger());
    const { rerender } = render(
      <DecisionLedger runId="run_test" decisionCount={1} />,
    );
    await waitFor(() => {
      expect(fetchDecisions).toHaveBeenCalledTimes(1);
    });
    await openDecisions();

    // A snapshot that adds no decision must not re-read anything.
    rerender(<DecisionLedger runId="run_test" decisionCount={1} />);
    expect(fetchDecisions).toHaveBeenCalledTimes(1);

    fetchDecisions.mockResolvedValue(
      ledger({ decisions: [decision(), decision({ id: "d2", reason: "later" })] }),
    );
    rerender(<DecisionLedger runId="run_test" decisionCount={2} />);
    await waitFor(() => {
      expect(fetchDecisions).toHaveBeenCalledTimes(2);
    });
    expect(screen.getByText("later")).toBeDefined();
  });

  it("is closed by default and pages five decisions at a time", async () => {
    fetchDecisions.mockResolvedValue(
      ledger({
        decisions: Array.from({ length: 7 }, (_, index) =>
          decision({ id: `d${index + 1}`, reason: `choice ${index + 1}` }),
        ),
      }),
    );
    render(<DecisionLedger runId="run_test" decisionCount={7} />);

    const toggle = await screen.findByRole("button", { name: "Open decisions" });
    expect(screen.queryByRole("list", { name: "Decisions, oldest first" })).toBeNull();
    fireEvent.click(toggle);
    expect(screen.getByRole("button", { name: "Close decisions" }).textContent).toBe("Close");
    expect(screen.getByRole("list", { name: "Decisions, oldest first" }).children).toHaveLength(5);
    expect(screen.getByText("choice 1")).toBeDefined();
    expect(screen.queryByText("choice 6")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Later decisions" }));
    expect(screen.getByText("choice 6")).toBeDefined();
    expect(screen.getByText("D06")).toBeDefined();
    expect(screen.getByText("Page 2 of 2")).toBeDefined();
  });
});
