import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import LandingPage from "./page";

describe("LandingPage", () => {
  it("links into the console from the action group", () => {
    render(<LandingPage />);
    expect(screen.getByRole("heading", { name: "URUK" })).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Start research" }),
    ).toHaveAttribute("href", "/runs/new");
    expect(screen.getByRole("link", { name: "View runs" })).toHaveAttribute(
      "href",
      "/runs",
    );
  });

  it("does not explain the browser-cookie ownership model", () => {
    render(<LandingPage />);
    expect(
      screen.queryByText(/anonymous browser workspace/i),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/cookie/i)).not.toBeInTheDocument();
  });
});
