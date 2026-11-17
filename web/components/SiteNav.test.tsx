import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SiteNav } from "./SiteNav";

describe("SiteNav", () => {
  it("keeps every primary destination in the global header", () => {
    render(<SiteNav />);

    const home = screen.getByRole("link", { name: "URUK home" });
    expect(home.querySelector("img")).toHaveAttribute("src", "/favicon.svg");
    expect(home.querySelector("img")).toHaveAttribute("alt", "");
    expect(screen.getByRole("link", { name: "Runs" })).toHaveAttribute("href", "/runs");
    expect(screen.getByRole("link", { name: "Library" })).toHaveAttribute("href", "/library");
    expect(screen.getByRole("link", { name: "Start research" })).toHaveAttribute(
      "href",
      "/runs/new",
    );
  });
});
