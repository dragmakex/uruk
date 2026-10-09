import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PassagePage, SourceDetail } from "@/lib/types";
import SourcePage from "./page";

const { detailMock, browseMock, searchMock } = vi.hoisted(() => ({
  detailMock: vi.fn(),
  browseMock: vi.fn(),
  searchMock: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  ApiFailure: class ApiFailure extends Error {
    kind: string;
    constructor(message: string, kind: string) {
      super(message);
      this.kind = kind;
    }
  },
  fetchSourceDetail: detailMock,
  browseSourcePassages: browseMock,
  searchSourcePassages: searchMock,
}));

vi.mock("@/lib/server-identity", () => ({
  identityCookieHeader: vi.fn().mockResolvedValue(""),
}));

function detail(): SourceDetail {
  return {
    run_id: "run_1",
    source: {
      id: "src_1",
      run_id: "run_1",
      origin: { kind: "url", at: "https://example.org/p" },
      title: "A paper",
      authors: "Cho et al.",
      date: "2023",
      identifier: "doi:10.1/abc",
      retrieved_at: "2026-10-04T14:00:00Z",
      content_hash: "abc123def456",
      access: "full_text",
      access_limitations: null,
      text_artifact: "art_1",
    },
    passage_count: 40,
  };
}

function page(): PassagePage {
  return {
    total: 40,
    offset: 0,
    limit: 20,
    passages: [
      { seq: 0, byte_start: 0, byte_end: 120, text: "first passage text" },
      { seq: 1, byte_start: 120, byte_end: 300, text: "second passage text" },
    ],
  };
}

function props(params: Record<string, string> = {}) {
  return {
    params: Promise.resolve({ id: "run_1", sourceId: "src_1" }),
    searchParams: Promise.resolve(params),
  };
}

beforeEach(() => {
  detailMock.mockReset().mockResolvedValue(detail());
  browseMock.mockReset().mockResolvedValue(page());
  searchMock.mockReset().mockResolvedValue([]);
});

describe("SourcePage browsing", () => {
  it("shows the recorded source facts and the exact hash prefix", async () => {
    render(await SourcePage(props()));
    expect(
      screen.getByRole("heading", { name: "A paper" }),
    ).toBeInTheDocument();
    expect(screen.getByText("doi:10.1/abc")).toBeInTheDocument();
    expect(screen.getByText(/abc123def456/)).toBeInTheDocument();
  });

  it("lists passages in order with their byte spans", async () => {
    render(await SourcePage(props()));
    expect(searchMock).not.toHaveBeenCalled();
    expect(screen.getByText("first passage text")).toBeInTheDocument();
    expect(screen.getByText(/bytes 120–300/)).toBeInTheDocument();
  });

  it("pages forward from the current offset", async () => {
    browseMock.mockResolvedValue({ ...page(), total: 40, offset: 0, limit: 20 });
    render(await SourcePage(props()));
    expect(screen.getByRole("link", { name: /next/i })).toHaveAttribute(
      "href",
      "/runs/run_1/sources/src_1?offset=20",
    );
  });

  it("answers a foreign or unknown source with not-found, not an error page", async () => {
    const { ApiFailure } = await import("@/lib/api");
    detailMock.mockRejectedValue(
      new ApiFailure("no such source", "not_found", 404),
    );
    render(await SourcePage(props()));
    expect(screen.getByText(/source not found/i)).toBeInTheDocument();
  });
});

describe("SourcePage searching", () => {
  it("renders ranked snippets instead of the browse list when q is set", async () => {
    searchMock.mockResolvedValue([
      {
        source_id: "src_1",
        seq: 7,
        byte_start: 700,
        byte_end: 980,
        text: "…",
        snippet: "…the thermal gradient…",
        score: 2.5,
      },
    ]);
    render(await SourcePage(props({ q: "thermal" })));
    expect(searchMock).toHaveBeenCalledWith(
      "run_1",
      "src_1",
      "thermal",
      expect.anything(),
    );
    expect(browseMock).not.toHaveBeenCalled();
    expect(screen.getByText("…the thermal gradient…")).toBeInTheDocument();
  });

  it("keeps the active query in the search box", async () => {
    render(await SourcePage(props({ q: "thermal" })));
    expect(screen.getByLabelText("Search this source")).toHaveValue("thermal");
  });
});
