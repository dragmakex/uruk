/**
 * Typed API client. The browser talks to the same-origin `/api` path; on
 * the server (Server Components) requests go straight to the Rust service
 * at `URUK_API_URL` (default `http://127.0.0.1:7913`).
 *
 * Every response body passes through a `lib/parse.ts` validator. Errors
 * become [`ApiFailure`] carrying the stable `{error, kind}` body, plus the
 * synthetic kind `"unreachable"` when the backend is down.
 */

import {
  parseApiError,
  parseReport,
  parseRunOverviews,
  parseRunSnapshot,
  parseSources,
  parseStartedRun,
} from "./parse";
import type {
  ReportView,
  RunOverview,
  RunSnapshot,
  SourceView,
  StartRunInput,
} from "./types";

export class ApiFailure extends Error {
  readonly kind: string;
  readonly status: number | null;

  constructor(message: string, kind: string, status: number | null) {
    super(message);
    this.name = "ApiFailure";
    this.kind = kind;
    this.status = status;
  }
}

function apiBase(): string {
  if (typeof window !== "undefined") return "";
  return process.env.URUK_API_URL ?? "http://127.0.0.1:7913";
}

async function request(path: string, init?: RequestInit): Promise<unknown> {
  let response: Response;
  try {
    response = await fetch(`${apiBase()}${path}`, {
      cache: "no-store",
      ...init,
    });
  } catch {
    throw new ApiFailure(
      "the uruk API is unreachable; is `uruk serve` running?",
      "unreachable",
      null,
    );
  }

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = null;
  }

  if (!response.ok) {
    const err = parseApiError(body);
    throw new ApiFailure(err.error, err.kind, response.status);
  }
  return body;
}

export async function fetchRuns(): Promise<RunOverview[]> {
  return parseRunOverviews(await request("/api/runs"));
}

export async function fetchSnapshot(runId: string): Promise<RunSnapshot> {
  const body = await request(`/api/runs/${encodeURIComponent(runId)}`);
  const envelope = body as { snapshot?: unknown };
  return parseRunSnapshot(envelope.snapshot);
}

export async function fetchReport(runId: string): Promise<ReportView> {
  return parseReport(await request(`/api/runs/${encodeURIComponent(runId)}/report`));
}

export async function fetchRunSources(runId: string): Promise<SourceView[]> {
  return parseSources(await request(`/api/runs/${encodeURIComponent(runId)}/sources`));
}

export async function fetchLibrary(): Promise<SourceView[]> {
  return parseSources(await request("/api/library"));
}

export async function startRun(input: StartRunInput): Promise<string> {
  const body = await request("/api/runs", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input),
  });
  return parseStartedRun(body);
}

export async function stopRun(runId: string): Promise<void> {
  await request(`/api/runs/${encodeURIComponent(runId)}/stop`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  });
}
