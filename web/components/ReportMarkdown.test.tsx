import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ReportMarkdown } from "./ReportMarkdown";

describe("ReportMarkdown", () => {
  it("renders the report structure as real elements", () => {
    const { container } = render(
      <ReportMarkdown
        markdown={"# Question\n\n## 2. Findings\n\n- **Mode:** task\n"}
      />,
    );

    expect(
      screen.getByRole("heading", { level: 1, name: "Question" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { level: 2, name: "2. Findings" }),
    ).toBeInTheDocument();
    expect(container.querySelector("li")).toHaveTextContent("Mode: task");
  });

  it("never renders raw HTML from the generated text", () => {
    const { container } = render(
      <ReportMarkdown
        markdown={
        'before <script>window.pwned = true</script> <img src="x" onerror="x"> after'
        }
      />,
    );

    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("img")).toBeNull();
    expect(container).toHaveTextContent("before");
    expect(container).toHaveTextContent("after");
  });
});
