/**
 * Typed API client. The browser talks to the same-origin `/api` path and
 * its anonymous identity cookie rides along automatically (HttpOnly —
 * this code never sees or handles the token). On the server (Server
 * Components) requests go straight to the Rust service at `URUK_API_URL`
 * (default `http://127.0.0.1:7913`), so the read functions take the
 * incoming request's cookie header (see `lib/server-identity.ts`) and
 * forward it; without it the API would answer for a different, empty
 * identity.
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

async function request(
  path: string,
  init?: RequestInit,
  cookie?: string,
): Promise<unknown> {
  let response: Response;
  try {
    const headers = new Headers(init?.headers);
    if (cookie !== undefined && cookie !== "") {
      headers.set("cookie", cookie);
    }
    response = await fetch(`${apiBase()}${path}`, {
      cache: "no-store",
      ...init,
      headers,
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

export async function fetchRuns(cookie?: string): Promise<RunOverview[]> {
  return parseRunOverviews(await request("/api/runs", undefined, cookie));
}

export async function fetchSnapshot(
  runId: string,
  cookie?: string,
): Promise<RunSnapshot> {
  const body = await request(
    `/api/runs/${encodeURIComponent(runId)}`,
    undefined,
    cookie,
  );
  const envelope = body as { snapshot?: unknown };
  return parseRunSnapshot(envelope.snapshot);
}

export async function fetchReport(
  runId: string,
  cookie?: string,
): Promise<ReportView> {
  return parseReport(
    await request(`/api/runs/${encodeURIComponent(runId)}/report`, undefined, cookie),
  );
}

export async function fetchRunSources(
  runId: string,
  cookie?: string,
): Promise<SourceView[]> {
  return parseSources(
    await request(`/api/runs/${encodeURIComponent(runId)}/sources`, undefined, cookie),
  );
}

export async function fetchLibrary(cookie?: string): Promise<SourceView[]> {
  return parseSources(await request("/api/library", undefined, cookie));
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
