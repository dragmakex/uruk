"use client";

import { useSyncExternalStore } from "react";

type Theme = "light" | "dark";

/**
 * The applied theme lives on `<html data-theme>`, written before paint by
 * the root layout's inline script. That attribute is the external store:
 * reads come from the DOM, changes go through a MutationObserver, so server
 * and first client render agree and no effect re-renders on mount.
 */
function subscribe(onChange: () => void): () => void {
  const observer = new MutationObserver(onChange);
  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme"],
  });
  return () => observer.disconnect();
}

function readTheme(): Theme | "unknown" {
  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

function serverTheme(): Theme | "unknown" {
  // The server cannot know the visitor's stored choice.
  return "unknown";
}

/** Light/dark switch matching the dual reference pages. */
export function ThemeToggle() {
  const theme = useSyncExternalStore(subscribe, readTheme, serverTheme);

  function toggle() {
    const next: Theme = readTheme() === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    try {
      localStorage.setItem("uruk-theme", next);
    } catch {
      // Private-mode storage failures only lose persistence, not the toggle.
    }
  }

  return (
    <button
      type="button"
      className="btn-outline btn"
      onClick={toggle}
      aria-label={
        theme === "unknown"
          ? "Switch color theme"
          : `Switch to the ${theme === "dark" ? "light" : "dark"} theme`
      }
      style={{ padding: "6px 14px", fontFamily: "var(--font-mono)", fontSize: 12 }}
    >
      {theme === "unknown" ? "Theme" : theme === "dark" ? "Light" : "Dark"}
    </button>
  );
}
