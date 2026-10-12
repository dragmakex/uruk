import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Button, ButtonGroup } from "./Button";

const WEB_ROOT = path.resolve(import.meta.dirname, "..");
const GLOBALS_CSS = readFileSync(path.join(WEB_ROOT, "app/globals.css"), "utf8")
  // Comments would otherwise be read as part of the following selector.
  .replace(/\/\*[\s\S]*?\*\//g, "");

/** Every `selector { body }` rule in globals.css whose selector matches. */
function ruleBodies(selector: RegExp): string[] {
  const bodies: string[] = [];
  for (const match of GLOBALS_CSS.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const sel = match[1]!.trim();
    if (selector.test(sel)) {
      bodies.push(match[2]!);
    }
  }
  return bodies;
}

/** CSS properties that change a control's box: a variant must set none. */
const GEOMETRY = [
  "padding",
  "height",
  "min-height",
  "max-height",
  "width",
  "min-width",
  "max-width",
  "font-size",
  "font-weight",
  "line-height",
  "border-width",
  "border:",
  "margin",
];

function productionTsx(dir: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...productionTsx(full));
    } else if (entry.name.endsWith(".tsx") && !entry.name.endsWith(".test.tsx")) {
      files.push(full);
    }
  }
  return files;
}

describe("Button", () => {
  it("renders a native button that defaults to type=button and forwards clicks", async () => {
    const onClick = vi.fn();
    const user = userEvent.setup();
    render(
      <Button variant="outline" size="sm" onClick={onClick}>
        Try again
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Try again" });
    expect(button).toHaveAttribute("type", "button");
    await user.click(button);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("keeps submit semantics and the disabled state of a native button", async () => {
    const onClick = vi.fn();
    const user = userEvent.setup();
    render(
      <Button type="submit" disabled onClick={onClick}>
        Start research
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Start research" });
    expect(button).toHaveAttribute("type", "submit");
    expect(button).toBeDisabled();
    await user.click(button);
    expect(onClick).not.toHaveBeenCalled();
  });

  it("renders an internal navigation link when given an app route", () => {
    render(
      <Button href="/runs/new" size="md">
        Start research
      </Button>,
    );
    expect(screen.getByRole("link", { name: "Start research" })).toHaveAttribute(
      "href",
      "/runs/new",
    );
  });

  it("renders API and download anchors with their href and download attribute", () => {
    render(
      <>
        <Button href="/api/runs/r1/report.pdf" variant="outline" size="sm">
          Download PDF
        </Button>
        <Button href="/api/runs/r1/report.md" download="report.md" variant="outline" size="sm">
          Download Markdown
        </Button>
      </>,
    );
    expect(screen.getByRole("link", { name: "Download PDF" })).toHaveAttribute(
      "href",
      "/api/runs/r1/report.pdf",
    );
    const md = screen.getByRole("link", { name: "Download Markdown" });
    expect(md).toHaveAttribute("href", "/api/runs/r1/report.md");
    expect(md).toHaveAttribute("download", "report.md");
  });

  it("forwards an explicit accessible name", () => {
    render(
      <Button variant="outline" size="xs" aria-label="Delete notes.pdf">
        Delete
      </Button>,
    );
    expect(screen.getByRole("button", { name: "Delete notes.pdf" })).toBeInTheDocument();
  });

  it("gives every variant of one size the same scale token and no inline style", () => {
    render(
      <>
        <Button variant="primary" size="cta" href="/runs/r1/report">
          Read the report
        </Button>
        <Button variant="outline" size="cta" href="/runs/r1/citations">
          Inspect citations
        </Button>
      </>,
    );
    const primary = screen.getByRole("link", { name: "Read the report" });
    const outline = screen.getByRole("link", { name: "Inspect citations" });
    expect(primary).toHaveClass("btn", "btn--primary", "btn--cta");
    expect(outline).toHaveClass("btn", "btn--outline", "btn--cta");
    expect(primary).not.toHaveAttribute("style");
    expect(outline).not.toHaveAttribute("style");
  });

  it("makes a group of equal CTAs exactly equal by layout, not by label width", () => {
    render(
      <ButtonGroup equal>
        <Button variant="primary" size="cta" href="/runs/r1/report">
          Read the report
        </Button>
        <Button variant="outline" size="cta" href="/runs/r1/citations">
          A much longer label than its sibling has
        </Button>
      </ButtonGroup>,
    );
    const primary = screen.getByRole("link", { name: "Read the report" });
    const outline = screen.getByRole("link", { name: /much longer label/ });
    expect(primary.parentElement).toBe(outline.parentElement);
    expect(primary.parentElement).toHaveClass("btn-group", "btn-group--equal");

    // Equal grid columns size both siblings to the same width whatever
    // their labels; the cta token fixes the height.
    const group = ruleBodies(/^\.btn-group--equal$/);
    expect(group.length).toBeGreaterThan(0);
    expect(group[0]).toMatch(/grid-auto-columns\s*:\s*1fr/);
    expect(group[0]).toMatch(/grid-auto-flow\s*:\s*column/);
    const cta = ruleBodies(/^\.btn--cta$/);
    expect(cta[0]).toMatch(/(^|[\s;])height\s*:/);
  });

  it("keeps geometry in size tokens: variant rules set no box properties", () => {
    const variantRules = ruleBodies(/^\.btn--(primary|outline)(\s*[:,].*)?$/);
    expect(variantRules.length).toBeGreaterThan(0);
    for (const body of variantRules) {
      for (const property of GEOMETRY) {
        expect(body, `variant rule must not set ${property}: ${body}`).not.toMatch(
          new RegExp(`(^|[\\s;])${property.replace(/[-:]/g, "\\$&")}\\s*:?`),
        );
      }
    }
    const cta = ruleBodies(/^\.btn--cta$/);
    expect(cta.length).toBeGreaterThan(0);
    expect(cta[0]).toMatch(/(^|[\s;])height\s*:/);
    expect(cta[0]).toMatch(/(^|[\s;])min-width\s*:/);
  });

  it("is the only button implementation in production TSX", () => {
    const offenders: string[] = [];
    for (const dir of ["app", "components"]) {
      for (const file of productionTsx(path.join(WEB_ROOT, dir))) {
        if (file.endsWith(path.join("components", "Button.tsx"))) {
          continue;
        }
        const source = readFileSync(file, "utf8");
        if (/<button[\s>]/.test(source) || /className=\{?["'`]btn[\s"'`-]/.test(source)) {
          offenders.push(path.relative(WEB_ROOT, file));
        }
      }
    }
    expect(offenders).toEqual([]);
  });
});
