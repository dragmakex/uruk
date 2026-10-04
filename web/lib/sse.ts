"use client";

/**
 * Live run state over the browser's native EventSource.
 *
 * Reconnection is EventSource's built-in behavior; this hook layers on:
 * connection status (so the UI can show a stale indicator), validated
 * snapshot parsing (invalid frames are counted and dropped, never
 * rendered), and a deliberate close when the server sends the terminal
 * `end` event so the browser does not reconnect to a finished run.
 */

import { useEffect, useMemo, useReducer } from "react";
import { parseRunSnapshot } from "./parse";
import type { RunSnapshot } from "./types";

export type StreamStatus =
  | "connecting"
  | "live"
  | "reconnecting"
  | "ended"
  | "failed";

export interface StreamState {
  snapshot: RunSnapshot | null;
  status: StreamStatus;
  /** Frames that failed validation; non-zero means a contract drift. */
  parseErrors: number;
  /** Instant of the last accepted snapshot, for staleness display. */
  lastUpdateAt: number | null;
}

export type StreamEvent =
  | { type: "open" }
  | { type: "snapshot"; snapshot: RunSnapshot; at: number }
  | { type: "parse-error" }
  | { type: "ended" }
  | { type: "connection-lost" }
  | { type: "failed" };

export const initialStreamState: StreamState = {
  snapshot: null,
  status: "connecting",
  parseErrors: 0,
  lastUpdateAt: null,
};

/** Pure transition function, unit-tested without a browser. */
export function streamReducer(state: StreamState, event: StreamEvent): StreamState {
  switch (event.type) {
    case "open":
      // An open after `ended` must not resurrect a finished stream.
      if (state.status === "ended") return state;
      return { ...state, status: "live" };
    case "snapshot":
      return {
        ...state,
        snapshot: event.snapshot,
        status: state.status === "ended" ? "ended" : "live",
        lastUpdateAt: event.at,
      };
    case "parse-error":
      return { ...state, parseErrors: state.parseErrors + 1 };
    case "ended":
      return { ...state, status: "ended" };
    case "connection-lost":
      if (state.status === "ended") return state;
      return { ...state, status: "reconnecting" };
    case "failed":
      if (state.status === "ended") return state;
      return { ...state, status: "failed" };
  }
}

/** Subscribe to `/api/runs/{id}/events` for the lifetime of the component. */
export function useRunStream(
  runId: string,
  initial: RunSnapshot | null,
): StreamState {
  const seeded = useMemo<StreamState>(
    () => ({ ...initialStreamState, snapshot: initial }),
    [initial],
  );
  const [state, dispatch] = useReducer(streamReducer, seeded);

  useEffect(() => {
    const source = new EventSource(
      `/api/runs/${encodeURIComponent(runId)}/events`,
    );
    let closed = false;

    source.onopen = () => dispatch({ type: "open" });

    source.addEventListener("snapshot", (event: MessageEvent<string>) => {
      try {
        const snapshot = parseRunSnapshot(JSON.parse(event.data));
        dispatch({ type: "snapshot", snapshot, at: Date.now() });
      } catch {
        dispatch({ type: "parse-error" });
      }
    });

    source.addEventListener("end", () => {
      // The run is terminal: close deliberately so the browser does not
      // reconnect to a stream that will never change again.
      closed = true;
      source.close();
      dispatch({ type: "ended" });
    });

    source.onerror = () => {
      if (closed) return;
      if (source.readyState === EventSource.CLOSED) {
        dispatch({ type: "failed" });
      } else {
        // EventSource is auto-reconnecting; surface the gap.
        dispatch({ type: "connection-lost" });
      }
    };

    return () => {
      closed = true;
      source.close();
    };
  }, [runId]);

  return state;
}
