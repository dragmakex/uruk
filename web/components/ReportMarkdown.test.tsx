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

  it("blocks active URLs and passive remote-image requests", () => {
    const { container } = render(
      <ReportMarkdown
        markdown={
          "[bad](javascript:alert(document.domain)) ![tracker](https://attacker.invalid/pixel) [safe](https://example.com/)"
        }
      />,
    );

    expect(container.querySelector("img")).toBeNull();
    // With its javascript: href stripped the anchor loses the implicit
    // link role, so it is found by text instead.
    const bad = Array.from(container.querySelectorAll("a")).find(
      (a) => a.textContent === "bad",
    );
    expect(bad).toBeDefined();
    expect(bad).not.toHaveAttribute("href");
    expect(screen.getByRole("link", { name: "safe" })).toHaveAttribute(
      "href",
      "https://example.com/",
    );
    expect(screen.getByRole("link", { name: "safe" })).toHaveAttribute(
      "rel",
      "noreferrer noopener",
    );
  });
});
