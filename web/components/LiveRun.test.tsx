import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { snapshotFixture } from "@/lib/fixtures";
import { parseRunSnapshot } from "@/lib/parse";
import { resumeRun, restartRun, stopRun } from "@/lib/api";
import { LiveRun } from "./LiveRun";

const pushMock = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: pushMock }),
}));

vi.mock("@/lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/api")>()),
  stopRun: vi.fn(async () => {}),
  resumeRun: vi.fn(async () => {}),
  restartRun: vi.fn(async () => "run_fresh"),
}));

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
  vi.clearAllMocks();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

function pausedSnapshot() {
  const paused = snapshotFixture() as { run: Record<string, unknown> };
  paused.run.state = "paused";
  paused.run.paused_ms = 30_000;
  return parseRunSnapshot(paused);
}

describe("LiveRun", () => {
  it("renders the header facts and topology from the initial snapshot", () => {
    render(<LiveRun runId="run_0412aa" initial={initial} />);

    // Header: question, method, round; all real recorded facts.
    expect(
      screen.getByText("Which mechanisms could explain acquired resistance?"),
    ).toBeInTheDocument();
    expect(screen.getByText("tournament")).toBeInTheDocument();
    expect(screen.getAllByText("round 2 of 4").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();

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
    expect(screen.queryByRole("button", { name: "Pause" })).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Read the report" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Restart" }),
    ).toBeInTheDocument();
    expect(source!.closed).toBe(true);
  });

  it("shows a paused run with resume and restart instead of pause", () => {
    render(<LiveRun runId="run_0412aa" initial={pausedSnapshot()} />);

    expect(screen.getByRole("heading", { name: "Run paused" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Resume" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Restart" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pause" })).not.toBeInTheDocument();
  });

  it("pause asks for confirmation and calls the stop endpoint", async () => {
    vi.stubGlobal("confirm", vi.fn(() => true));
    render(<LiveRun runId="run_0412aa" initial={initial} />);

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    });

    expect(vi.mocked(stopRun)).toHaveBeenCalledWith("run_0412aa");
  });

  it("resume calls the resume endpoint without a confirmation gate", async () => {
    render(<LiveRun runId="run_0412aa" initial={pausedSnapshot()} />);

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Resume" }));
    });

    expect(vi.mocked(resumeRun)).toHaveBeenCalledWith("run_0412aa");
  });

  it("restart confirms, then navigates to the fresh run", async () => {
    vi.stubGlobal("confirm", vi.fn(() => true));
    render(<LiveRun runId="run_0412aa" initial={pausedSnapshot()} />);

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Restart" }));
    });

    expect(vi.mocked(restartRun)).toHaveBeenCalledWith("run_0412aa");
    expect(pushMock).toHaveBeenCalledWith("/runs/run_fresh");
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
