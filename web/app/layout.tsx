import type { Metadata } from "next";
import type { ReactNode } from "react";
import { SiteNav } from "@/components/SiteNav";
import "./globals.css";

export const metadata: Metadata = {
  title: {
    default: "URUK",
    template: "%s | URUK",
  },
  description:
    "Uruk is an autonomous research engine. This local console specifies runs, watches the agent tournament live, and reads the exported reports.",
};

/**
 * Applies the stored (or system) theme before first paint so neither mode
 * flashes. The toggle in the nav writes `uruk-theme` to localStorage.
 */
const themeInit = `(function () {
  var theme;
  try { theme = localStorage.getItem("uruk-theme"); } catch (e) { theme = null; }
  if (theme !== "light" && theme !== "dark") {
    theme = window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  document.documentElement.dataset.theme = theme;
})();`;

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" data-theme="light" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: themeInit }} />
      </head>
      <body>
        <a className="skip-link" href="#main">
          Skip to content
        </a>
        <SiteNav />
        <main id="main">{children}</main>
      </body>
    </html>
  );
}
