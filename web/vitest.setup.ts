import "@testing-library/jest-dom/vitest";

// jsdom does not implement matchMedia; the theme code reads it for the
// system preference. Report "no match" so tests default to light, the same
// fallback the prepaint script uses. Guarded: node-environment test files
// (e.g. lib/api.server.test.ts) have no window at all.
if (typeof window !== "undefined" && typeof window.matchMedia !== "function") {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    value: (query: string): MediaQueryList =>
      ({
        matches: false,
        media: query,
        onchange: null,
        addEventListener: () => {},
        removeEventListener: () => {},
        addListener: () => {},
        removeListener: () => {},
        dispatchEvent: () => false,
      }) as MediaQueryList,
  });
}
