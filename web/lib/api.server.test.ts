// @vitest-environment node
/**
 * Server-side behavior of the API client: Server Components fetch the
 * Rust API directly, so the browser's identity cookie must be forwarded
 * explicitly — there is no ambient cookie jar on the server.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  browseSourcePassages,
  fetchCitations,
  fetchLibrary,
  fetchRuns,
  fetchSnapshot,
  fetchSourceDetail,
  searchLibraryPassages,
  searchSourcePassages,
} from "./api";

interface SeenRequest {
  url: string;
  cookie: string | null;
}

function stubFetch(body: unknown): SeenRequest[] {
  const seen: SeenRequest[] = [];
  vi.stubGlobal(
    "fetch",
    async (url: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      seen.push({
        url: String(url),
        cookie: new Headers(init?.headers).get("cookie"),
      });
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    },
  );
  return seen;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("server-side requests", () => {
  it("forwards the given cookie header to the Rust API", async () => {
    const seen = stubFetch({ ok: true, runs: [] });
    await fetchRuns("uruk_browser=aaaa");
    expect(seen[0]?.cookie).toBe("uruk_browser=aaaa");
  });

  it("targets the Rust API origin directly when not in a browser", async () => {
    const seen = stubFetch({ ok: true, runs: [] });
    await fetchRuns("uruk_browser=aaaa");
    expect(seen[0]?.url).toBe("http://127.0.0.1:7913/api/runs");
  });

  it("sends no cookie header when no cookie is given", async () => {
    const seen = stubFetch({ ok: true, runs: [] });
    await fetchRuns();
    expect(seen[0]?.cookie).toBeNull();
  });

  it("forwards the cookie on run-scoped and library requests too", async () => {
    const sources = stubFetch({ ok: true, sources: [] });
    await fetchLibrary("uruk_browser=bbbb");
    expect(sources[0]?.cookie).toBe("uruk_browser=bbbb");

    vi.unstubAllGlobals();
    const snapshot = stubFetch({ ok: true, snapshot: null });
    await expect(fetchSnapshot("run_x", "uruk_browser=bbbb")).rejects.toThrow();
    expect(snapshot[0]?.cookie).toBe("uruk_browser=bbbb");
  });
});

describe("library explorer requests", () => {
  it("sends only the filters that are set, percent-encoded", async () => {
    const seen = stubFetch({ ok: true, sources: [] });
    await fetchLibrary("uruk_browser=cc", {
      access: "full_text",
      q: "heat & light",
    });
    expect(seen[0]?.url).toBe(
      "http://127.0.0.1:7913/api/library?access=full_text&q=heat+%26+light",
    );
  });

  it("requests the bare library when no filters are given", async () => {
    const seen = stubFetch({ ok: true, sources: [] });
    await fetchLibrary("uruk_browser=cc");
    expect(seen[0]?.url).toBe("http://127.0.0.1:7913/api/library");
  });

  it("searches owner-wide passages with the query encoded", async () => {
    const seen = stubFetch({ ok: true, query: "x", passages: [] });
    await searchLibraryPassages("thermal drift", "uruk_browser=cc");
    expect(seen[0]?.url).toBe(
      "http://127.0.0.1:7913/api/library/passages?q=thermal+drift",
    );
    expect(seen[0]?.cookie).toBe("uruk_browser=cc");
  });

  it("fetches a source detail under its run, path-encoded", async () => {
    const seen = stubFetch({
      ok: true,
      run_id: "run/1",
      source: null,
      passage_count: 0,
    });
    await expect(
      fetchSourceDetail("run/1", "src 1", "uruk_browser=cc"),
    ).rejects.toThrow();
    expect(seen[0]?.url).toBe(
      "http://127.0.0.1:7913/api/runs/run%2F1/sources/src%201",
    );
  });

  it("browses passages with offset and limit", async () => {
    const seen = stubFetch({
      ok: true,
      total: 0,
      offset: 40,
      limit: 20,
      passages: [],
    });
    await browseSourcePassages("run_1", "src_1", { offset: 40, limit: 20 }, "c=1");
    expect(seen[0]?.url).toBe(
      "http://127.0.0.1:7913/api/runs/run_1/sources/src_1/passages?offset=40&limit=20",
    );
  });

  it("searches one source's passages with q", async () => {
    const seen = stubFetch({ ok: true, query: "x", passages: [] });
    await searchSourcePassages("run_1", "src_1", "heat", "c=1");
    expect(seen[0]?.url).toBe(
      "http://127.0.0.1:7913/api/runs/run_1/sources/src_1/passages?q=heat",
    );
  });

  it("fetches a run's citations with the cookie forwarded", async () => {
    const seen = stubFetch({ ok: true, run_id: "run_1", citations: [] });
    await fetchCitations("run_1", "uruk_browser=dd");
    expect(seen[0]?.url).toBe("http://127.0.0.1:7913/api/runs/run_1/citations");
    expect(seen[0]?.cookie).toBe("uruk_browser=dd");
  });
});
