import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ReportPage from "./page";

const { fetchReportMock } = vi.hoisted(() => ({
  fetchReportMock: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  ApiFailure: class ApiFailure extends Error {
    kind = "test";
  },
  fetchReport: fetchReportMock,
}));

vi.mock("@/lib/server-identity", () => ({
  identityCookieHeader: vi.fn().mockResolvedValue(""),
}));

beforeEach(() => {
  fetchReportMock.mockReset();
  fetchReportMock.mockResolvedValue({
    run_id: "run_abc",
    markdown: "# Why\n\n## 2. Findings\n\nNo findings were produced.\n",
    manifest: null,
  });
});

describe("ReportPage", () => {
  it("offers the canonical downloads next to the rendered report", async () => {
    render(await ReportPage({ params: Promise.resolve({ id: "run_abc" }) }));

    expect(screen.getByRole("link", { name: "Download PDF" })).toHaveAttribute(
      "href",
      "/api/runs/run_abc/report.pdf",
    );
    expect(
      screen.getByRole("link", { name: "Download Markdown" }),
    ).toHaveAttribute("href", "/api/runs/run_abc/report.md");
    expect(screen.getByRole("link", { name: "Human report" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("link", { name: "Agent report" })).toHaveAttribute("href", "/api/runs/run_abc/report.json");
    expect(
      screen.getByRole("heading", { level: 2, name: "2. Findings" }),
    ).toBeInTheDocument();
  });

  it("puts run bookkeeping after the report in a closed disclosure", async () => {
    fetchReportMock.mockResolvedValue({
      run_id: "run_abc",
      markdown: "# Ideas\n\n## 1. Findings\n\nHow the leading ideas work.\n",
      manifest: { usage: { model_calls: 25, tokens: 1234 }, counts: { items: 8 } },
    });
    const { container } = render(await ReportPage({ params: Promise.resolve({ id: "run_abc" }) }));
    const article = container.querySelector("article");
    const summary = screen.getByText("Run details", { selector: "summary" });
    const disclosure = summary.closest("details");
    expect(disclosure).not.toHaveAttribute("open");
    expect(article!.compareDocumentPosition(disclosure!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    expect(disclosure).toHaveTextContent("model calls25");
    expect(disclosure).toHaveTextContent("tokens1,234");
  });

  it("renders the budget status as plain UI copy", async () => {
    fetchReportMock.mockResolvedValue({
      run_id: "run_4b7b",
      markdown: "# Report\n",
      manifest: { state: "budget-exhausted", partial: true },
    });

    render(await ReportPage({ params: Promise.resolve({ id: "run_4b7b" }) }));

    expect(screen.getByText("Budget exhausted")).toBeInTheDocument();
    expect(screen.queryByText("budget-exhausted")).not.toBeInTheDocument();
  });
});
