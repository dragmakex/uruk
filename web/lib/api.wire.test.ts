// @vitest-environment node
/**
 * Wire shapes of the write-side API client: uploads travel as raw bytes
 * with the file name in the query string (never as a path), deletes hit
 * the id-scoped route, and a preview is the same start payload with
 * `dry_run: true`.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { deleteUpload, previewRun, startRun, uploadFile } from "./api";

interface SeenRequest {
  url: string;
  method: string;
  body: unknown;
}

function stubFetch(status: number, body: unknown): SeenRequest[] {
  const seen: SeenRequest[] = [];
  vi.stubGlobal(
    "fetch",
    async (url: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      seen.push({
        url: String(url),
        method: init?.method ?? "GET",
        body: init?.body,
      });
      return new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      });
    },
  );
  return seen;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("uploadFile", () => {
  it("posts the raw bytes with the encoded file name in the query", async () => {
    const seen = stubFetch(201, {
      ok: true,
      upload: {
        id: "upl_1",
        file_name: "lab notes.txt",
        size_bytes: 10,
        content_hash: "h",
        created_at: "2026-10-08T10:00:00Z",
      },
    });

    const file = new File(["alpha beta"], "lab notes.txt", { type: "text/plain" });
    const upload = await uploadFile(file);

    expect(seen[0]?.url).toBe(
      "http://127.0.0.1:7913/api/uploads?name=lab%20notes.txt",
    );
    expect(seen[0]?.method).toBe("POST");
    expect(await new Response(seen[0]?.body as BodyInit).text()).toBe("alpha beta");
    expect(upload.id).toBe("upl_1");
    expect(upload.file_name).toBe("lab notes.txt");
  });

  it("surfaces the stable error body when the file type is refused", async () => {
    stubFetch(400, {
      ok: false,
      error: "this file type is not accepted",
      kind: "validation",
    });
    const file = new File(["MZ"], "payload.exe");
    await expect(uploadFile(file)).rejects.toMatchObject({
      kind: "validation",
      message: "this file type is not accepted",
    });
  });
});

describe("deleteUpload", () => {
  it("issues a DELETE against the id-scoped route", async () => {
    const seen = stubFetch(200, { ok: true, upload_id: "upl_1" });
    await deleteUpload("upl_1");
    expect(seen[0]?.url).toBe("http://127.0.0.1:7913/api/uploads/upl_1");
    expect(seen[0]?.method).toBe("DELETE");
  });
});

describe("previewRun", () => {
  it("posts the start payload with dry_run set and parses the preview", async () => {
    const seen = stubFetch(200, {
      ok: true,
      dry_run: true,
      plan: { roles: ["generation"], methods: ["m"], rationale: "r" },
      inputs: [],
      permissions: { network: false, execute: false, allowed_tools: [] },
      budget: {
        max_model_calls: 200,
        max_seconds: 3600,
        max_iterations: 10,
        max_debate_turns: 1,
        max_acquisitions: 8,
      },
    });

    const preview = await previewRun({
      goal: "q",
      mode: "task",
      ranking: "simple",
    });

    expect(seen[0]?.url).toBe("http://127.0.0.1:7913/api/runs");
    expect(seen[0]?.method).toBe("POST");
    expect(JSON.parse(String(seen[0]?.body))).toMatchObject({
      goal: "q",
      dry_run: true,
    });
    expect(preview.plan.roles).toEqual(["generation"]);
  });
});

describe("startRun", () => {
  it("never sets dry_run on a real start", async () => {
    const seen = stubFetch(201, { ok: true, run_id: "run_1" });
    await startRun({ goal: "q", mode: "campaign", ranking: "simple" });
    expect(JSON.parse(String(seen[0]?.body))).not.toHaveProperty("dry_run");
  });
});
