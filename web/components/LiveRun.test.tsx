import { act, render, screen } from "@testing-library/react";
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

  addEventListener(name: string, handler: (event: MessageEvent<string>) => void) {
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
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("LiveRun", () => {
  it("renders the header facts and topology from the initial snapshot", () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);

    // Header: question, method, round; all real recorded facts.
    expect(
      screen.getByText("Which mechanisms could explain acquired resistance?"),
    ).toBeInTheDocument();
    expect(screen.getByText("tournament")).toBeInTheDocument();
    expect(screen.getAllByText("round 2 of 4").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();

    // Topology: supervisor and generation panels from the snapshot.
    expect(screen.getByRole("heading", { name: "Supervisor" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Generation" })).toBeInTheDocument();

    // Figure stats row shows the aggregate facts.
    expect(screen.getByText(/13 items, 34 matches/)).toBeInTheDocument();

    // Ranked candidate with its recorded rating.
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
      screen.getByText(/Run completed \(deliverable satisfied\)/),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Stop" })).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Read the report" }),
    ).toBeInTheDocument();
    expect(source!.closed).toBe(true);
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

    const report = screen.getByRole("link", { name: "Read the report" });
    const citations = screen.getByRole("link", { name: "Inspect citations" });
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
      screen.getByRole("heading", { level: 1, name: /snapshot could not be read/i }),
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
      screen.getByText("Which mechanisms could explain acquired resistance?"),
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
    expect(banner).toHaveTextContent(/position 3/);
    expect(banner).toHaveTextContent(/2 runs ahead/);
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expect(screen.queryByText(/failed/i)).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Read the report" })).not.toBeInTheDocument();
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
    expect(screen.getByRole("status", { name: /provider/i })).toBeInTheDocument();
    await act(async () => {
      source.emit("snapshot", JSON.stringify(snapshotFixture()));
    });
    expect(screen.queryByRole("status", { name: /provider/i })).not.toBeInTheDocument();
  });
});
