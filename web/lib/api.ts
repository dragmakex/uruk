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
  parseRunPreview,
  parseRunSnapshot,
  parseSources,
  parseStartedRun,
  parseUpload,
  parseUploads,
} from "./parse";
import type {
  ReportView,
  RunOverview,
  RunPreview,
  RunSnapshot,
  SourceView,
  StartRunInput,
  UploadView,
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

/**
 * Validate and plan a start without creating anything: the same payload
 * as [`startRun`] sent with `dry_run: true`.
 */
export async function previewRun(input: StartRunInput): Promise<RunPreview> {
  const body = await request("/api/runs", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...input, dry_run: true }),
  });
  return parseRunPreview(body);
}

/**
 * Store a file as an owner-bound upload. The bytes travel as the raw
 * body; the file name rides in the query string and is display text and
 * an extractor hint on the server, never a path.
 */
export async function uploadFile(file: File): Promise<UploadView> {
  const body = await request(
    `/api/uploads?name=${encodeURIComponent(file.name)}`,
    {
      method: "POST",
      headers: { "content-type": "application/octet-stream" },
      body: file,
    },
  );
  return parseUpload(body);
}

export async function fetchUploads(): Promise<UploadView[]> {
  return parseUploads(await request("/api/uploads"));
}

/** Remove an upload this browser owns. */
export async function deleteUpload(uploadId: string): Promise<void> {
  await request(`/api/uploads/${encodeURIComponent(uploadId)}`, {
    method: "DELETE",
  });
}

/**
 * Pause a run. Despite the route name this is a durable, resumable pause,
 * never the CLI's terminal cancel: queued work waits, and paused time does
 * not count against the wall-clock budget.
 */
export async function stopRun(runId: string): Promise<void> {
  await request(`/api/runs/${encodeURIComponent(runId)}/stop`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  });
}

/** Resume a paused run; a no-op on a run that is already running. */
export async function resumeRun(runId: string): Promise<void> {
  await request(`/api/runs/${encodeURIComponent(runId)}/resume`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  });
}

/**
 * Start a fresh run from this run's original start configuration (new
 * identity, no copied results; uploads are re-ingested). Returns the new
 * run's id. Refused while the original is still actively working.
 */
export async function restartRun(runId: string): Promise<string> {
  const body = await request(`/api/runs/${encodeURIComponent(runId)}/restart`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  });
  return parseStartedRun(body);
}
