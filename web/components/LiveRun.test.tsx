import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { snapshotFixture } from "@/lib/fixtures";
import { parseRunSnapshot } from "@/lib/parse";
import { LiveRun } from "./LiveRun";

/**
 * jsdom has no EventSource; this stand-in records instances so tests can
 * push named events through the real hook wiring.
 */
class FakeEventSource {
  static instances: FakeEventSource[] = [];
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 2;

  url: string;
  readyState = 0;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  listeners = new Map<string, Array<(event: MessageEvent<string>) => void>>();
  closed = false;

  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }

  addEventListener(
    name: string,
    handler: (event: MessageEvent<string>) => void,
  ) {
    const list = this.listeners.get(name) ?? [];
    list.push(handler);
    this.listeners.set(name, list);
  }

  emit(name: string, data: string) {
    for (const handler of this.listeners.get(name) ?? []) {
      handler({ data } as MessageEvent<string>);
    }
  }

  close() {
    this.closed = true;
    this.readyState = FakeEventSource.CLOSED;
  }
}

const initial = parseRunSnapshot(snapshotFixture());

beforeEach(() => {
  FakeEventSource.instances = [];
  vi.stubGlobal("EventSource", FakeEventSource);
  // The match log reads its own endpoint; this file is about the snapshot
  // stream, so it answers with an empty feed.
  vi.stubGlobal(
    "fetch",
    async (): Promise<Response> =>
      new Response(
        JSON.stringify({
          ok: true,
          candidates: {
            run_id: "run_0412aa",
            total: initial.items.length,
            offset: 0,
            limit: 5,
            through: null,
            items: initial.items,
          },
        }),
        {
          status: 200,
          headers: { "content-type": "application/json" },
        },
      ),
  );
});

afterEach(async () => {
  // Let the match-log read settle inside act before the tree unmounts, so
  // its arrival is not reported as an un-acted state update.
  await act(async () => {});
  vi.unstubAllGlobals();
});

describe("LiveRun", () => {
  it("groups scientist input, supervisor, workers, tools and shared memory", () => {
    const snapshot = parseRunSnapshot(snapshotFixture());
    snapshot.agents = [
      snapshot.agents[0]!,
      ...[
        "generation",
        "reflection",
        "ranking",
        "proximity",
        "evolution",
        "meta_review",
      ].map((role) => ({
        ...snapshot.agents[1]!,
        role,
        state: "idle" as const,
        current: null,
        counts: { ...snapshot.agents[1]!.counts, running: 0, pending: 0 },
      })),
    ];
    render(<LiveRun runId={snapshot.run.id} initial={snapshot} />);
    const system = screen.getByRole("region", { name: "Research system" });
    expect(
      within(system).getByRole("region", { name: "Scientist input" }),
    ).toBeInTheDocument();
    expect(
      within(system).getByRole("heading", { name: "Supervisor" }),
    ).toBeInTheDocument();
    const pool = within(system).getByRole("region", {
      name: "Async worker pool",
    });
    expect(within(pool).getAllByRole("heading")).toHaveLength(6);
    expect(
      within(pool).queryByRole("heading", { name: "Supervisor" }),
    ).not.toBeInTheDocument();
    expect(
      within(system).getByRole("region", { name: "Tools and retrieval" }),
    ).toBeInTheDocument();
    expect(
      within(system).getByRole("region", { name: "Shared context memory" }),
    ).toHaveTextContent("13");
    expect(
      screen.queryByText(/feeds Reflection|feeds Ranking/),
    ).not.toBeInTheDocument();
  });

  it("shows the fixed brief and permission facts without fictional integrations", () => {
    const fixture = snapshotFixture() as {
      goal: Record<string, unknown>;
      plan: Record<string, unknown>;
    };
    fixture.goal.rubric = {
      preferences: ["Prefer replication"],
      attributes: ["Testability"],
      constraints: ["No animal work"],
      acceptance: ["Name an endpoint"],
    };
    fixture.goal.permissions = {
      network: true,
      execute: false,
      allowed_tools: ["literature.discover"],
    };
    fixture.plan.stopping_conditions = ["budget_exhausted"];
    render(<LiveRun runId="run_0412aa" initial={parseRunSnapshot(fixture)} />);
    const brief = screen.getByRole("region", { name: "Scientist input" });
    expect(brief).toHaveTextContent(initial.goal.question);
    expect(brief).toHaveTextContent("Scientist → supervisor");
    expect(brief).toHaveTextContent("No animal work");
    expect(brief).toHaveTextContent("Testability");
    expect(brief).toHaveTextContent(/fixed for this run/i);
    const tools = screen.getByRole("region", { name: "Tools and retrieval" });
    expect(tools).toHaveTextContent("Permitted");
    expect(tools).toHaveTextContent("Not permitted");
    expect(tools).toHaveTextContent("literature.discover");
    expect(tools).not.toHaveTextContent(/connected|AlphaFold|Gemini/i);
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("navigates to memory records without fetching or opening them", async () => {
    const reads = vi.spyOn(globalThis, "fetch");
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    const memory = screen.getByRole("region", {
      name: "Shared context memory",
    });
    const candidates = within(memory).getByRole("link", {
      name: /candidates/i,
    });
    expect(candidates).toHaveAttribute("href", "#run-candidates");
    fireEvent.click(candidates);
    expect(document.getElementById("run-candidates")).not.toBeNull();
    expect(
      screen.getByRole("button", { name: "Open candidates" }),
    ).toHaveAttribute("aria-expanded", "false");
    expect(reads).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Open candidates" }));
    });
    expect(reads).toHaveBeenCalledTimes(1);
    reads.mockRestore();
  });

  it("keeps bounded-task workers and retrieval in their recorded groups", () => {
    const snapshot = parseRunSnapshot(snapshotFixture());
    snapshot.goal.mode = "task";
    snapshot.agents.push({ ...snapshot.agents[1]!, role: "search" });
    render(<LiveRun runId={snapshot.run.id} initial={snapshot} />);
    const pool = screen.getByRole("region", { name: "Async worker pool" });
    expect(within(pool).getAllByRole("heading")).toHaveLength(1);
    expect(
      within(pool).getByRole("heading", { name: "Generation" }),
    ).toBeInTheDocument();
    expect(
      within(pool).queryByRole("heading", { name: "Search" }),
    ).not.toBeInTheDocument();
    const tools = screen.getByRole("region", { name: "Tools and retrieval" });
    expect(
      within(tools).getByRole("heading", { name: "Search" }),
    ).toBeInTheDocument();
    expect(tools).toHaveTextContent("Supervised resource workers");
    expect(
      screen.queryByRole("heading", { name: "Evolution" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Bounded task")).toBeInTheDocument();
  });

  it.each([0, 1, 2])(
    "names every shared-memory count correctly for %s records",
    (count) => {
      const snapshot = parseRunSnapshot(snapshotFixture());
      for (const key of [
        "items",
        "reviews",
        "matches",
        "clusters",
        "decisions",
        "sources",
      ] as const)
        snapshot.stats[key] = count;
      render(<LiveRun runId={snapshot.run.id} initial={snapshot} />);
      const memory = screen.getByRole("region", {
        name: "Shared context memory",
      });
      for (const [singular, plural] of [
        ["Candidate", "Candidates"],
        ["Review", "Reviews"],
        ["Elo match", "Elo matches"],
        ["Cluster", "Clusters"],
        ["Decision", "Decisions"],
        ["Source", "Sources"],
      ] as const) {
        expect(
          within(memory).getByText(count === 1 ? singular : plural),
        ).toBeInTheDocument();
      }
      for (const [singular, plural] of [
        ["candidate", "candidates"],
        ["match", "matches"],
        ["decision", "decisions"],
      ] as const) {
        expect(
          within(memory).getByRole("link", {
            name: `${count} ${count === 1 ? singular : plural} in shared memory`,
          }),
        ).toBeInTheDocument();
      }
    },
  );

  it("puts the top-five result before the topology", () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    const overview = screen.getByRole("region", {
      name: "Top five candidates",
    });
    const topology = screen.getByRole("heading", { name: "Supervisor" });
    expect(
      overview.compareDocumentPosition(topology) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(overview).toHaveTextContent("1,412");
    expect(overview).toHaveTextContent("+16");
  });

  it("renders the header facts and topology from the initial snapshot", async () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);

    // Header: question, method, round; all real recorded facts.
    expect(
      screen.getByRole("heading", {
        level: 1,
        name: "Which mechanisms could explain acquired resistance?",
      }),
    ).toBeInTheDocument();
    expect(screen.getByText("tournament")).toBeInTheDocument();
    expect(screen.getAllByText("round 2 of 4").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();

    // Topology: supervisor and generation panels from the snapshot.
    expect(
      screen.getByRole("heading", { name: "Supervisor" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Generation" }),
    ).toBeInTheDocument();

    // Figure stats row shows the aggregate facts.
    expect(screen.getByText(/13 items, 34 matches/)).toBeInTheDocument();

    // Record tables are closed by default; opening candidates reveals the
    // ranked row with its recorded rating.
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Open candidates" }));
    });
    expect(screen.getByText(/1\. XBP1 splicing escape/)).toBeInTheDocument();
    expect(screen.getByText(/elo 1412, 9 matches/)).toBeInTheDocument();
  });

  it("applies streamed snapshots and settles on the end event", async () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    const source = FakeEventSource.instances[0];
    expect(source).toBeDefined();
    expect(source!.url).toBe("/api/runs/run_0412aa/events");

    const finished = snapshotFixture() as {
      run: Record<string, unknown>;
      stats: Record<string, unknown>;
    };
    finished.run.state = "completed";
    finished.run.stop_condition = "deliverable_satisfied";

    await act(async () => {
      source!.emit("snapshot", JSON.stringify(finished));
      source!.emit("end", "{}");
    });

    expect(
      screen.getByRole("heading", { name: "Completed" }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/deliverable satisfied/)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Stop" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Human report" }),
    ).toBeInTheDocument();
    expect(source!.closed).toBe(true);
  });

  it.each([
    ["budget-exhausted", "Budget exhausted"],
    ["cancelled", "Cancelled"],
    ["failed", "Failed"],
  ])("uses a concise heading for the %s status", (state, heading) => {
    const finished = snapshotFixture() as { run: Record<string, unknown> };
    finished.run.state = state;
    finished.run.stop_condition = "model_call_budget_reached";

    render(<LiveRun runId="run_0412aa" initial={parseRunSnapshot(finished)} />);

    expect(screen.getByRole("heading", { name: heading })).toBeInTheDocument();
    expect(
      screen.queryByText(/model call budget reached/),
    ).not.toBeInTheDocument();
  });

  it("sizes the finished-run actions identically, differing only in variant", async () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    const source = FakeEventSource.instances[0]!;
    const finished = snapshotFixture() as { run: Record<string, unknown> };
    finished.run.state = "completed";
    finished.run.stop_condition = "deliverable_satisfied";
    await act(async () => {
      source.emit("snapshot", JSON.stringify(finished));
      source.emit("end", "{}");
    });

    const report = screen.getByRole("link", { name: "Human report" });
    const citations = screen.getByRole("link", { name: "Agent report" });
    expect(report.textContent).toBe("Human report");
    expect(citations.textContent).toBe("Agent report");
    expect(report).toHaveAttribute("href", "/runs/run_0412aa/report");
    expect(citations).toHaveAttribute(
      "href",
      "/api/runs/run_0412aa/report.json",
    );
    expect(
      screen.queryByRole("link", { name: "Read the report" }),
    ).not.toBeInTheDocument();
    const sizeOf = (el: HTMLElement) =>
      [...el.classList].find((c) => /^btn--(xs|sm|md|cta)$/.test(c));
    expect(sizeOf(report)).toBe("btn--cta");
    expect(sizeOf(citations)).toBe("btn--cta");
    expect(report).toHaveClass("btn--primary");
    expect(citations).toHaveClass("btn--outline");
    expect(report).not.toHaveAttribute("style");
    expect(citations).not.toHaveAttribute("style");
    expect(report.parentElement).toBe(citations.parentElement);
    expect(report.parentElement).toHaveClass("btn-group--equal");
  });

  it("shows a reconnecting note when the stream drops", async () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    const source = FakeEventSource.instances[0]!;

    await act(async () => {
      source.readyState = FakeEventSource.CONNECTING;
      source.onerror?.();
    });

    expect(screen.getByText("connection lost, retrying")).toBeInTheDocument();
  });

  it("exposes the research question as the page's h1", () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    expect(
      screen.getByRole("heading", {
        level: 1,
        name: "Which mechanisms could explain acquired resistance?",
      }),
    ).toBeInTheDocument();
  });

  it("reports contract drift instead of connecting forever", async () => {
    render(<LiveRun runId="run_0412aa" initial={null} />);
    const source = FakeEventSource.instances[0]!;

    await act(async () => {
      source.onopen?.();
      source.emit("snapshot", JSON.stringify({ run: {} }));
    });

    expect(
      screen.getByRole("heading", { level: 1, name: "Invalid" }),
    ).toBeInTheDocument();
  });

  it("ignores invalid frames and keeps the last good snapshot", async () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    const source = FakeEventSource.instances[0]!;

    await act(async () => {
      source.emit("snapshot", "{not json");
      source.emit("snapshot", JSON.stringify({ run: {} }));
    });

    expect(
      screen.getByRole("heading", {
        level: 1,
        name: "Which mechanisms could explain acquired resistance?",
      }),
    ).toBeInTheDocument();
  });
});

describe("LiveRun queue and provider-wait states", () => {
  function queued(position: number) {
    const fixture = snapshotFixture() as {
      run: Record<string, unknown>;
      agents: unknown[];
    };
    fixture.run.state = "queued";
    fixture.run.queue_position = position;
    fixture.run.started_at = null;
    return parseRunSnapshot(fixture);
  }

  it("shows a queued run's position, keeps Stop, and never says failed", () => {
    render(<LiveRun runId="run_0412aa" initial={queued(3)} />);
    const banner = screen.getByRole("status", { name: /queued/i });
    expect(screen.getByRole("heading", { name: "Queued" })).toBeInTheDocument();
    expect(banner).toHaveTextContent(/position 3/);
    expect(banner).toHaveTextContent(/2 runs ahead/);
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expect(screen.queryByText(/failed/i)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: "Human report" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: "Agent report" }),
    ).not.toBeInTheDocument();
  });

  it("shows the provider wait on a running head and keeps Stop", async () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    const source = FakeEventSource.instances[0]!;
    const waiting = snapshotFixture() as { run: Record<string, unknown> };
    waiting.run.provider_wait = {
      since: "2026-10-04T14:03:00Z",
      note: "HTTP 503: overloaded",
    };
    await act(async () => {
      source.emit("snapshot", JSON.stringify(waiting));
    });
    const banner = screen.getByRole("status", { name: /provider/i });
    expect(
      screen.getByRole("heading", { name: "Waiting" }),
    ).toBeInTheDocument();
    expect(banner).toHaveTextContent("HTTP 503: overloaded");
    expect(banner).toHaveTextContent(/resumes automatically/i);
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expect(screen.queryByText(/failed/i)).not.toBeInTheDocument();
    expect(source.closed).toBe(false);
  });

  it("shows research time, not wall time, for a run that waited out an outage", () => {
    const finished = snapshotFixture() as { run: Record<string, unknown> };
    finished.run.state = "completed";
    finished.run.stop_condition = "deliverable_satisfied";
    // 14:00:05 → 14:04:17 is 252 s on the wall; 100 s of it was a settled
    // provider wait, so the run researched for 152 s.
    finished.run.started_at = "2026-10-04T14:00:05Z";
    finished.run.paused_secs = 100;
    finished.run.provider_wait = null;
    render(<LiveRun runId="run_0412aa" initial={parseRunSnapshot(finished)} />);
    expect(screen.getByLabelText("elapsed time")).toHaveTextContent("02:32");
  });

  it("drops the wait banner once the provider serves again", async () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);
    const source = FakeEventSource.instances[0]!;
    const waiting = snapshotFixture() as { run: Record<string, unknown> };
    waiting.run.provider_wait = {
      since: "2026-10-04T14:03:00Z",
      note: null,
    };
    await act(async () => {
      source.emit("snapshot", JSON.stringify(waiting));
    });
    expect(
      screen.getByRole("status", { name: /provider/i }),
    ).toBeInTheDocument();
    await act(async () => {
      source.emit("snapshot", JSON.stringify(snapshotFixture()));
    });
    expect(
      screen.queryByRole("status", { name: /provider/i }),
    ).not.toBeInTheDocument();
  });
});

describe("LiveRun while on the queue", () => {
  type RunState = "queued" | "running";

  /**
   * A run holding a queue place. Durable recovery can put a `running` run
   * back on the queue, so `queue_position` is what says "waiting".
   */
  function onQueue(
    position: number,
    providerWaiting: boolean,
    state: RunState = "queued",
  ) {
    const fixture = snapshotFixture() as { run: Record<string, unknown> };
    fixture.run.state = state;
    fixture.run.queue_position = position;
    fixture.run.started_at = state === "queued" ? null : "2026-10-04T14:00:05Z";
    fixture.run.provider_wait = null;
    fixture.run.queue_provider_waiting = providerWaiting;
    return fixture;
  }

  const OUTAGE_HEADING = "Waiting";
  const RECOVERY_HEADING = "Available";

  function expectNoVisualization() {
    expect(
      screen.queryByRole("heading", { name: "Supervisor" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Generation" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/Fig\. run/)).not.toBeInTheDocument();
    expect(screen.queryByText("Key")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("region", { name: "Ranked candidates" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Candidates" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/XBP1 splicing escape/)).not.toBeInTheDocument();
  }

  async function expectVisualization() {
    expect(
      screen.getByRole("heading", { name: "Supervisor" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Generation" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/Fig\. run/)).toBeInTheDocument();
    expect(screen.getByText("Key")).toBeInTheDocument();
    expect(
      screen.getByRole("region", { name: "Ranked candidates" }),
    ).toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Open candidates" }));
    });
    expect(screen.getByText(/1\. XBP1 splicing escape/)).toBeInTheDocument();
  }

  it("hides the empty visualization and keeps the position and Stop", () => {
    render(
      <LiveRun
        runId="run_0412aa"
        initial={parseRunSnapshot(onQueue(3, false))}
      />,
    );
    const banner = screen.getByRole("status", { name: /queued/i });
    expect(screen.getByRole("heading", { name: "Queued" })).toBeInTheDocument();
    expect(banner).toHaveTextContent(/position 3/);
    expect(banner).toHaveTextContent(/2 runs ahead/);
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expectNoVisualization();
    expect(
      screen.queryByRole("status", { name: /provider/i }),
    ).not.toBeInTheDocument();
  });

  it("treats a running run with a queue place as queued until its place is gone", async () => {
    render(
      <LiveRun
        runId="run_0412aa"
        initial={parseRunSnapshot(onQueue(1, false, "running"))}
      />,
    );
    expect(screen.getByRole("status", { name: /queued/i })).toHaveTextContent(
      /next to start/,
    );
    expectNoVisualization();
    const source = FakeEventSource.instances[0]!;
    await act(async () => {
      source.emit("snapshot", JSON.stringify(snapshotFixture()));
    });
    expect(
      screen.queryByRole("status", { name: /queued/i }),
    ).not.toBeInTheDocument();
    await expectVisualization();
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
  });

  it("tells a queued researcher that the active run is waiting for the provider", () => {
    render(
      <LiveRun
        runId="run_0412aa"
        initial={parseRunSnapshot(onQueue(2, true))}
      />,
    );
    const status = screen.getByRole("status", {
      name: "Model provider status",
    });
    expect(
      screen.getByRole("heading", { name: OUTAGE_HEADING }),
    ).toBeInTheDocument();
    expect(status).toHaveTextContent(
      OUTAGE_HEADING +
        "Your place in the queue is preserved and this run will start " +
        "automatically when its turn comes; no action is required. " +
        "Your run has not started.",
    );
    expect(screen.queryByText(/failed/i)).not.toBeInTheDocument();
    expect(
      screen.queryByText(/HTTP|503|overloaded|since/i),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: RECOVERY_HEADING }),
    ).not.toBeInTheDocument();
    // The queued banner still tells the truth about the place in line.
    expect(screen.getByRole("status", { name: /queued/i })).toHaveTextContent(
      /1 run ahead/,
    );
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expectNoVisualization();
  });

  it("shows the provider status when a stream snapshot reports the outage", async () => {
    render(
      <LiveRun
        runId="run_0412aa"
        initial={parseRunSnapshot(onQueue(2, false))}
      />,
    );
    expect(
      screen.queryByRole("status", { name: "Model provider status" }),
    ).not.toBeInTheDocument();
    const source = FakeEventSource.instances[0]!;
    await act(async () => {
      source.emit("snapshot", JSON.stringify(onQueue(2, true)));
    });
    expect(
      screen.getByRole("heading", { name: OUTAGE_HEADING }),
    ).toBeInTheDocument();
    expectNoVisualization();
  });

  it("announces the recovery politely while the run stays queued", async () => {
    render(
      <LiveRun
        runId="run_0412aa"
        initial={parseRunSnapshot(onQueue(2, true))}
      />,
    );
    const source = FakeEventSource.instances[0]!;
    await act(async () => {
      source.emit("snapshot", JSON.stringify(onQueue(2, false)));
    });
    expect(
      screen.queryByRole("heading", { name: OUTAGE_HEADING }),
    ).not.toBeInTheDocument();
    const status = screen.getByRole("status", {
      name: "Model provider status",
    });
    expect(
      screen.getByRole("heading", { name: RECOVERY_HEADING }),
    ).toBeInTheDocument();
    expect(status).toHaveTextContent(/queue is moving again/i);
    expect(status).toHaveTextContent(/start automatically/i);
    expect(screen.getByRole("status", { name: /queued/i })).toHaveTextContent(
      /position 2/,
    );
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expectNoVisualization();
  });

  it("never announces a recovery on an initial load that is not waiting", () => {
    render(
      <LiveRun
        runId="run_0412aa"
        initial={parseRunSnapshot(onQueue(2, false))}
      />,
    );
    expect(
      screen.queryByRole("status", { name: "Model provider status" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: RECOVERY_HEADING }),
    ).not.toBeInTheDocument();
  });

  it("drops the recovery notice when the run leaves the queue and starts", async () => {
    render(
      <LiveRun
        runId="run_0412aa"
        initial={parseRunSnapshot(onQueue(1, true))}
      />,
    );
    const source = FakeEventSource.instances[0]!;
    await act(async () => {
      source.emit("snapshot", JSON.stringify(onQueue(1, false)));
    });
    expect(
      screen.getByRole("heading", { name: RECOVERY_HEADING }),
    ).toBeInTheDocument();
    await act(async () => {
      source.emit("snapshot", JSON.stringify(snapshotFixture()));
    });
    expect(
      screen.queryByRole("status", { name: "Model provider status" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("status", { name: /queued/i }),
    ).not.toBeInTheDocument();
    await expectVisualization();
  });
});
