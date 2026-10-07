"use client";

import { useLayoutEffect, useState } from "react";

type Theme = "light" | "dark";

const STORAGE_KEY = "uruk-theme";

/**
 * Resolves the theme the same way as the prepaint script in app/layout.tsx:
 * the stored choice when valid, otherwise the system preference. Client-only.
 */
function resolveTheme(): Theme {
  let stored: string | null = null;
  try {
    stored = localStorage.getItem(STORAGE_KEY);
  } catch {
    stored = null;
  }
  if (stored === "light" || stored === "dark") {
    return stored;
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

/**
 * Light/dark switch. The server renders a neutral "Theme" placeholder (it
 * cannot know the visitor's choice); the first client render matches it, so
 * hydration is clean, and the layout effect renames the button before paint.
 *
 * The layout effect also re-applies the resolved theme to `<html data-theme>`:
 * in development, Strict Mode remounts reset `<html>` to its JSX attributes,
 * which would silently drop what the prepaint script applied (see the Next.js
 * guide "Preventing Flash", section "Re-applying attributes in development").
 */
export function ThemeToggle() {
  const [theme, setTheme] = useState<Theme | null>(null);

  useLayoutEffect(() => {
    const resolved = resolveTheme();
    document.documentElement.dataset.theme = resolved;
    // Two-pass hydration: the server rendered a placeholder because it cannot
    // know the stored theme, so adopting the client value needs exactly one
    // setState after hydration, before paint. A lazy initializer would make
    // the first client render diverge from the server HTML instead.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- one-shot two-pass hydration update
    setTheme(resolved);
  }, []);

  function toggle() {
    const next: Theme = (theme ?? resolveTheme()) === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Private-mode storage failures only lose persistence, not the toggle.
    }
    setTheme(next);
  }

  return (
    <button
      type="button"
      className="btn-outline btn"
      onClick={toggle}
      style={{ padding: "6px 14px", fontFamily: "var(--font-mono)", fontSize: 12 }}
    >
      {theme === null ? "Theme" : theme === "dark" ? "Switch to light" : "Switch to dark"}
    </button>
  );
}
