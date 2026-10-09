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
    expect(
      screen.getByRole("heading", { level: 2, name: "2. Findings" }),
    ).toBeInTheDocument();
  });
});
