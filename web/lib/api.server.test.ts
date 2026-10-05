// @vitest-environment node
/**
 * Server-side behavior of the API client: Server Components fetch the
 * Rust API directly, so the browser's identity cookie must be forwarded
 * explicitly — there is no ambient cookie jar on the server.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchLibrary, fetchRuns, fetchSnapshot } from "./api";

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
