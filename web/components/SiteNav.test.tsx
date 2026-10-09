import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SiteNav } from "./SiteNav";

describe("SiteNav", () => {
  it("uses the favicon fortress asset beside the URUK wordmark", () => {
    render(<SiteNav />);

    const home = screen.getByRole("link", { name: "URUK" });
    expect(home.querySelector("img")).toHaveAttribute("src", "/favicon.svg");
    expect(home.querySelector("img")).toHaveAttribute("alt", "");
  });
});
