import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LibraryPassageHit } from "@/lib/types";
import LibrarySearchPage from "./page";

const { searchMock } = vi.hoisted(() => ({
  searchMock: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  ApiFailure: class ApiFailure extends Error {
    kind = "test";
  },
  searchLibraryPassages: searchMock,
}));

vi.mock("@/lib/server-identity", () => ({
  identityCookieHeader: vi.fn().mockResolvedValue(""),
}));

function hit(overrides: Partial<LibraryPassageHit> = {}): LibraryPassageHit {
  return {
    run_id: "run_1",
    source_id: "src_1",
    source_title: "A paper",
    seq: 3,
    byte_start: 120,
    byte_end: 480,
    text: "the full passage text",
    snippet: "…thermal drift of the sensor…",
    score: 1.5,
    ...overrides,
  };
}

beforeEach(() => {
  searchMock.mockReset();
  searchMock.mockResolvedValue([]);
});

describe("LibrarySearchPage", () => {
  it("does not search until a query is given", async () => {
    render(await LibrarySearchPage({ searchParams: Promise.resolve({}) }));
    expect(searchMock).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Search passages")).toHaveValue("");
  });

  it("renders ranked hits naming their source and run", async () => {
    searchMock.mockResolvedValue([hit()]);
    render(
      await LibrarySearchPage({
        searchParams: Promise.resolve({ q: "thermal" }),
      }),
    );
    expect(searchMock).toHaveBeenCalledWith("thermal", expect.anything());
    expect(
      screen.getByText("…thermal drift of the sensor…"),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "A paper" })).toHaveAttribute(
      "href",
      "/runs/run_1/sources/src_1",
    );
  });

  it("says plainly when nothing matches", async () => {
    render(
      await LibrarySearchPage({
        searchParams: Promise.resolve({ q: "nonesuch" }),
      }),
    );
    expect(screen.getByText(/no passages match/i)).toBeInTheDocument();
  });
});
