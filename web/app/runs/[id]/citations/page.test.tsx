import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CitationView } from "@/lib/types";
import CitationsPage from "./page";

const { citationsMock } = vi.hoisted(() => ({
  citationsMock: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  ApiFailure: class ApiFailure extends Error {
    kind = "test";
  },
  fetchCitations: citationsMock,
}));

vi.mock("@/lib/server-identity", () => ({
  identityCookieHeader: vi.fn().mockResolvedValue(""),
}));

function citations(): CitationView[] {
  return [
    {
      origin: { kind: "deliverable_claim", basis: "source_reported" },
      claim: "the alloy melts at 900K",
      source_id: "src_1",
      source_title: "A paper",
      locator: "chars 10..90",
      resolution: {
        kind: "span",
        start: 10,
        end: 90,
        text: "the exact cited bytes",
        truncated: false,
      },
    },
    {
      origin: { kind: "review_observation", review_id: "rev_1" },
      claim: "figure 3 contradicts this",
      source_id: "src_1",
      source_title: "A paper",
      locator: "p. 4",
      resolution: { kind: "source_locator" },
    },
    {
      origin: { kind: "deliverable_claim", basis: "model_asserted" },
      claim: "ghost claim",
      source_id: "src_404",
      source_title: null,
      locator: null,
      resolution: {
        kind: "unresolved",
        reason: "the cited source src_404 is not recorded by this run",
      },
    },
  ];
}

function props() {
  return { params: Promise.resolve({ id: "run_1" }) };
}

beforeEach(() => {
  citationsMock.mockReset().mockResolvedValue(citations());
});

describe("CitationsPage", () => {
  it("quotes a verified span's exact text", async () => {
    render(await CitationsPage(props()));
    expect(screen.getByText("the exact cited bytes")).toBeInTheDocument();
    expect(screen.getByText("the alloy melts at 900K")).toBeInTheDocument();
  });

  it("links a coarse locator to its source rather than quoting bytes", async () => {
    render(await CitationsPage(props()));
    const links = screen.getAllByRole("link", { name: "A paper" });
    expect(links[0]).toHaveAttribute("href", "/runs/run_1/sources/src_1");
    expect(screen.getByText("p. 4")).toBeInTheDocument();
  });

  it("states an unresolved citation's reason verbatim", async () => {
    render(await CitationsPage(props()));
    expect(
      screen.getByText("the cited source src_404 is not recorded by this run"),
    ).toBeInTheDocument();
  });

  it("says when a run has no citations yet", async () => {
    citationsMock.mockResolvedValue([]);
    render(await CitationsPage(props()));
    expect(screen.getByText(/no citations/i)).toBeInTheDocument();
  });
});
