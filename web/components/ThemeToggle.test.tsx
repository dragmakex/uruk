import { render, screen } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { ThemeToggle } from "./ThemeToggle";

beforeEach(() => {
  localStorage.clear();
  document.documentElement.dataset.theme = "light";
});

describe("ThemeToggle", () => {
  it("server-renders the neutral placeholder, since the server cannot know the choice", () => {
    expect(renderToString(<ThemeToggle />)).toContain("Theme");
  });

  it("offers dark mode when the resolved theme is light", () => {
    render(<ThemeToggle />);
    expect(
      screen.getByRole("button", { name: "Switch to dark" }),
    ).toBeInTheDocument();
  });

  it("offers light mode when the stored theme is dark", () => {
    localStorage.setItem("uruk-theme", "dark");
    document.documentElement.dataset.theme = "dark";
    render(<ThemeToggle />);
    expect(
      screen.getByRole("button", { name: "Switch to light" }),
    ).toBeInTheDocument();
  });

  it("switches light to dark: applies data-theme, persists, and renames itself", async () => {
    const user = userEvent.setup();
    render(<ThemeToggle />);

    await user.click(screen.getByRole("button", { name: "Switch to dark" }));

    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(localStorage.getItem("uruk-theme")).toBe("dark");
    expect(
      screen.getByRole("button", { name: "Switch to light" }),
    ).toBeInTheDocument();
  });

  it("switches dark back to light the same way", async () => {
    localStorage.setItem("uruk-theme", "dark");
    document.documentElement.dataset.theme = "dark";
    const user = userEvent.setup();
    render(<ThemeToggle />);

    await user.click(screen.getByRole("button", { name: "Switch to light" }));

    expect(document.documentElement.dataset.theme).toBe("light");
    expect(localStorage.getItem("uruk-theme")).toBe("light");
    expect(
      screen.getByRole("button", { name: "Switch to dark" }),
    ).toBeInTheDocument();
  });

  it("restores the persisted choice on a fresh mount, even if <html> was reset", async () => {
    const user = userEvent.setup();
    const first = render(<ThemeToggle />);
    await user.click(screen.getByRole("button", { name: "Switch to dark" }));
    first.unmount();

    // Dev Strict Mode remounts reset <html> to its JSX attributes; the
    // component must re-resolve dark from storage, not trust the DOM.
    document.documentElement.dataset.theme = "light";
    render(<ThemeToggle />);

    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(
      screen.getByRole("button", { name: "Switch to light" }),
    ).toBeInTheDocument();
  });
});
