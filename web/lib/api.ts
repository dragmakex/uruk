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
  parseCitations,
  parseLibraryPassageHits,
  parsePassagePage,
  parseReport,
  parseRunOverviews,
  parseRunPreview,
  parseRunSnapshot,
  parseSourceDetail,
  parseSourcePassageHits,
  parseSources,
  parseStartedRun,
  parseUpload,
  parseUploads,
} from "./parse";
import type {
  CitationView,
  LibraryFilter,
  LibraryPassageHit,
  PassagePage,
  ReportView,
  RunOverview,
  RunPreview,
  RunSnapshot,
  SourceDetail,
  SourcePassageHit,
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

/** Build `?k=v…` from the entries that are set and non-empty. */
function queryString(params: Record<string, string | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== "") search.set(key, value);
  }
  const qs = search.toString();
  return qs === "" ? "" : `?${qs}`;
}

export async function fetchLibrary(
  cookie?: string,
  filter?: LibraryFilter,
): Promise<SourceView[]> {
  const qs = queryString({
    access: filter?.access,
    origin: filter?.origin,
    q: filter?.q,
  });
  return parseSources(await request(`/api/library${qs}`, undefined, cookie));
}

export async function searchLibraryPassages(
  q: string,
  cookie?: string,
): Promise<LibraryPassageHit[]> {
  return parseLibraryPassageHits(
    await request(`/api/library/passages${queryString({ q })}`, undefined, cookie),
  );
}

function sourcePath(runId: string, sourceId: string): string {
  return `/api/runs/${encodeURIComponent(runId)}/sources/${encodeURIComponent(sourceId)}`;
}

export async function fetchSourceDetail(
  runId: string,
  sourceId: string,
  cookie?: string,
): Promise<SourceDetail> {
  return parseSourceDetail(
    await request(sourcePath(runId, sourceId), undefined, cookie),
  );
}

export async function browseSourcePassages(
  runId: string,
  sourceId: string,
  page: { offset?: number; limit?: number },
  cookie?: string,
): Promise<PassagePage> {
  const qs = queryString({
    offset: page.offset !== undefined ? String(page.offset) : undefined,
    limit: page.limit !== undefined ? String(page.limit) : undefined,
  });
  return parsePassagePage(
    await request(`${sourcePath(runId, sourceId)}/passages${qs}`, undefined, cookie),
  );
}

export async function searchSourcePassages(
  runId: string,
  sourceId: string,
  q: string,
  cookie?: string,
): Promise<SourcePassageHit[]> {
  return parseSourcePassageHits(
    await request(
      `${sourcePath(runId, sourceId)}/passages${queryString({ q })}`,
      undefined,
      cookie,
    ),
  );
}

export async function fetchCitations(
  runId: string,
  cookie?: string,
): Promise<CitationView[]> {
  return parseCitations(
    await request(
      `/api/runs/${encodeURIComponent(runId)}/citations`,
      undefined,
      cookie,
    ),
  );
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

export async function stopRun(runId: string): Promise<void> {
  await request(`/api/runs/${encodeURIComponent(runId)}/stop`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  });
}
