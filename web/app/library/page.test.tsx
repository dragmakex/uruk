import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SourceView } from "@/lib/types";
import LibraryPage from "./page";

const { fetchLibraryMock } = vi.hoisted(() => ({
  fetchLibraryMock: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  ApiFailure: class ApiFailure extends Error {
    kind = "test";
  },
  fetchLibrary: fetchLibraryMock,
}));

vi.mock("@/lib/server-identity", () => ({
  identityCookieHeader: vi.fn().mockResolvedValue(""),
}));

function source(overrides: Partial<SourceView> = {}): SourceView {
  return {
    id: "src_1",
    run_id: "run_1",
    origin: { kind: "url", at: "https://example.org/p" },
    title: "A paper",
    authors: null,
    date: null,
    identifier: null,
    retrieved_at: "2026-10-04T14:00:00Z",
    content_hash: "abc123def456",
    access: "full_text",
    access_limitations: null,
    text_artifact: "art_1",
    ...overrides,
  };
}

beforeEach(() => {
  fetchLibraryMock.mockReset();
  fetchLibraryMock.mockResolvedValue([]);
});

describe("LibraryPage", () => {
  it("passes the search-params filters through to the API", async () => {
    render(
      await LibraryPage({
        searchParams: Promise.resolve({
          access: "full_text",
          origin: "url",
          q: "heat",
        }),
      }),
    );
    expect(fetchLibraryMock).toHaveBeenCalledWith(expect.anything(), {
      access: "full_text",
      origin: "url",
      q: "heat",
    });
  });

  it("renders a GET filter form preserving the active values", async () => {
    render(
      await LibraryPage({
        searchParams: Promise.resolve({ access: "full_text", q: "heat" }),
      }),
    );
    const access = screen.getByLabelText("Access");
    expect(access).toHaveValue("full_text");
    expect(screen.getByLabelText("Origin")).toHaveValue("");
    expect(screen.getByLabelText("Metadata")).toHaveValue("heat");
  });

  it("links each source to its detail page under its run", async () => {
    fetchLibraryMock.mockResolvedValue([source()]);
    render(await LibraryPage({ searchParams: Promise.resolve({}) }));
    expect(screen.getByRole("link", { name: "A paper" })).toHaveAttribute(
      "href",
      "/runs/run_1/sources/src_1",
    );
  });

  it("links to the owner-wide passage search", async () => {
    render(await LibraryPage({ searchParams: Promise.resolve({}) }));
    expect(
      screen.getByRole("link", { name: "Search passages" }),
    ).toHaveAttribute("href", "/library/search");
  });

  it("distinguishes a filtered empty result from an empty library", async () => {
    render(
      await LibraryPage({ searchParams: Promise.resolve({ q: "nothing" }) }),
    );
    expect(screen.getByText(/no sources match/i)).toBeInTheDocument();
    expect(
      screen.queryByText(/No sources in this browser/i),
    ).not.toBeInTheDocument();
  });
});
