import { describe, expect, it } from "vitest";
import { snapshotFixture } from "./fixtures";
import { parseRunSnapshot } from "./parse";
import {
  initialStreamState,
  streamReducer,
  type StreamState,
} from "./sse";

const snapshot = parseRunSnapshot(snapshotFixture());

function apply(events: Parameters<typeof streamReducer>[1][]): StreamState {
  return events.reduce(streamReducer, initialStreamState);
}

describe("streamReducer", () => {
  it("goes live on open and records accepted snapshots", () => {
    const state = apply([
      { type: "open" },
      { type: "snapshot", snapshot, at: 1000 },
    ]);
    expect(state.status).toBe("live");
    expect(state.snapshot?.run.id).toBe("run_0412aa");
    expect(state.lastUpdateAt).toBe(1000);
  });

  it("marks the gap while EventSource reconnects, then recovers", () => {
    const state = apply([
      { type: "open" },
      { type: "snapshot", snapshot, at: 1 },
      { type: "connection-lost" },
    ]);
    expect(state.status).toBe("reconnecting");
    expect(state.snapshot).not.toBeNull();

    const recovered = streamReducer(state, { type: "open" });
    expect(recovered.status).toBe("live");
  });

  it("counts invalid frames without dropping the last good snapshot", () => {
    const state = apply([
      { type: "snapshot", snapshot, at: 1 },
      { type: "parse-error" },
      { type: "parse-error" },
    ]);
    expect(state.parseErrors).toBe(2);
    expect(state.snapshot).not.toBeNull();
  });

  it("stays ended once the server settles the stream", () => {
    const state = apply([
      { type: "snapshot", snapshot, at: 1 },
      { type: "ended" },
      { type: "open" },
      { type: "connection-lost" },
    ]);
    expect(state.status).toBe("ended");
  });

  it("reports a hard failure when the source closes for good", () => {
    const state = apply([{ type: "open" }, { type: "failed" }]);
    expect(state.status).toBe("failed");
  });
});
