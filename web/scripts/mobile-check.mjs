#!/usr/bin/env node
/**
 * Mobile responsiveness gate for the Uruk web console.
 *
 * The check loads the real pages at the widths that matter on a phone and
 * fails on the defects that break a layout rather than on cosmetics:
 *
 *   - the document scrolls sideways (content wider than the viewport),
 *   - a visible element extends past the viewport outside a deliberate
 *     horizontal scroller,
 *   - the global navigation overlaps, wraps, or grows past 80px,
 *   - any visible control is smaller than WCAG 2.5.8's 24px minimum,
 *   - a button is smaller than the 40px touch target this UI promises,
 *   - `nowrap` text is clipped without an ellipsis.
 *
 * Usage:
 *   bun run check:mobile
 *   bun run check:mobile -- --base-url http://127.0.0.1:3000
 *   bun run check:mobile -- --base-url https://urukai.app \
 *       --run-id run_0412aa --cookie "$URUK_BROWSER_COOKIE"
 *   bun run check:mobile -- --run-id run_0412aa \
 *       --task-id task-generation-literature
 *
 * `--run-id` adds the live run, report, and citations pages. Those pages
 * belong to a browser identity, so pass its `uruk_browser` cookie value
 * when checking anything other than a fresh workspace.
 *
 * Screenshots and a JSON report land in `.mobile-check/` by default.
 */

import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { chromium } from "playwright";

const WIDTHS = [320, 360, 390, 414, 768];
const BASE_ROUTES = [
  ["home", "/"],
  ["runs", "/runs"],
  ["new-run", "/runs/new"],
  ["library", "/library"],
  ["library-search", "/library/search?q=evidence"],
];

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith("--")) continue;
    const key = token.slice(2);
    if (key === "help") {
      args.help = true;
      continue;
    }
    const value = argv[i + 1];
    if (value === undefined || value.startsWith("--")) {
      args[key] = true;
      continue;
    }
    args[key] = value;
    i += 1;
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));

if (args.help) {
  console.log(
    "Usage: bun run check:mobile [-- --base-url URL] [--run-id ID] [--cookie TOKEN] " +
      "[--task-id ID] [--color-scheme light|dark] [--user-agent UA] " +
      "[--widths 320,360,390,414,768] [--out DIR]",
  );
  process.exit(0);
}

const baseUrl = String(
  args["base-url"] ?? process.env.URUK_WEB_URL ?? "http://127.0.0.1:3000",
).replace(/\/+$/, "");
const widths = String(args.widths ?? WIDTHS.join(","))
  .split(",")
  .map((value) => Number.parseInt(value.trim(), 10))
  .filter((value) => Number.isInteger(value) && value >= 280);
const runId = typeof args["run-id"] === "string" ? args["run-id"] : "";
const taskId = typeof args["task-id"] === "string" ? args["task-id"] : "";
const cookie = typeof args.cookie === "string" ? args.cookie : "";
const colorScheme = args["color-scheme"] === "dark" ? "dark" : "light";
const customUserAgent =
  typeof args["user-agent"] === "string" ? args["user-agent"] : "";
const outDir = path.resolve(String(args.out ?? ".mobile-check"));

if (widths.length === 0) {
  console.error("No usable widths. Pass --widths 320,390,768.");
  process.exit(2);
}

const routes = [...BASE_ROUTES];
if (runId !== "") {
  routes.push(
    ["run", `/runs/${encodeURIComponent(runId)}`],
    ["report", `/runs/${encodeURIComponent(runId)}/report`],
    ["citations", `/runs/${encodeURIComponent(runId)}/citations`],
  );
  if (taskId !== "") {
    routes.push([
      "reasoning",
      `/runs/${encodeURIComponent(runId)}/tasks/${encodeURIComponent(taskId)}/reasoning`,
    ]);
  }
}

function slug(value) {
  return value
    .replace(/^\/+/, "")
    .replace(/[^a-z0-9]+/gi, "-")
    .replace(/^-+|-+$/g, "") || "home";
}

/** Everything measured in the page; no Playwright objects cross the bridge. */
function auditPage() {
  const viewportWidth = document.documentElement.clientWidth;
  const results = {
    viewportWidth,
    documentWidth: document.documentElement.scrollWidth,
    offenders: [],
    navHeight: 0,
    navItems: [],
    navGap: null,
    undersized: [],
    smallButtons: [],
    clipped: [],
    blocked:
      Boolean(
        document.querySelector(
          ".cf-footer-ip-reveal-btn, #challenge-form, .cf-browser-verification",
        ),
      ) ||
      /just a moment|attention required|verify you are human|checking your browser/i.test(
        `${document.title} ${document.body.innerText.slice(0, 500)}`,
      ),
  };

  const isVisible = (el) => {
    const style = getComputedStyle(el);
    const rect = el.getBoundingClientRect();
    return (
      style.display !== "none" &&
      style.visibility !== "hidden" &&
      Number(style.opacity) !== 0 &&
      rect.width > 0 &&
      rect.height > 0
    );
  };

  const inHorizontalScroller = (el) => {
    for (let parent = el.parentElement; parent; parent = parent.parentElement) {
      const style = getComputedStyle(parent);
      if (
        (style.overflowX === "auto" || style.overflowX === "scroll") &&
        parent.scrollWidth > parent.clientWidth + 1
      ) {
        return true;
      }
    }
    return false;
  };

  const inClippedSubtree = (el) => {
    for (let parent = el; parent; parent = parent.parentElement) {
      const style = getComputedStyle(parent);
      if (style.clip && style.clip !== "auto") return true;
    }
    return false;
  };

  for (const el of document.querySelectorAll("body *")) {
    if (!isVisible(el)) continue;
    if (
      el.classList.contains("skip-link") ||
      el.classList.contains("visually-hidden")
    ) {
      continue;
    }
    if (inClippedSubtree(el)) continue;
    const style = getComputedStyle(el);
    if (style.position === "fixed") continue;
    const rect = el.getBoundingClientRect();
    if (rect.right <= viewportWidth + 1 && rect.left >= -1) continue;
    if (inHorizontalScroller(el)) continue;
    results.offenders.push({
      tag: el.tagName.toLowerCase(),
      className: typeof el.className === "string" ? el.className.slice(0, 80) : "",
      left: Math.round(rect.left),
      right: Math.round(rect.right),
      width: Math.round(rect.width),
      text: (el.textContent ?? "").trim().slice(0, 60),
    });
  }

  const nav = document.querySelector(".site-nav");
  if (nav && isVisible(nav)) {
    results.navHeight = Math.round(nav.getBoundingClientRect().height);
    results.navItems = [
      ...nav.querySelectorAll(".wordmark, .nav-links > *"),
    ]
      .filter(isVisible)
      .map((el) => {
        const rect = el.getBoundingClientRect();
        return {
          label:
            (el.getAttribute("aria-label") ?? el.textContent ?? "").trim().slice(0, 40),
          left: Math.round(rect.left),
          right: Math.round(rect.right),
        };
      })
      .sort((a, b) => a.left - b.left);
    const gaps = [];
    for (let i = 1; i < results.navItems.length; i += 1) {
      gaps.push(results.navItems[i].left - results.navItems[i - 1].right);
    }
    results.navGap = gaps.length > 0 ? Math.min(...gaps) : null;
  }

  // Inline links inside prose are exempt from WCAG 2.5.8, and a checkbox
  // wrapped in its own <label> gets the whole label as its target. Measure
  // the controls this UI actually sizes: buttons, fields, disclosures, and
  // the navigation.
  const controls = [
    ...document.querySelectorAll(
      ".btn, button, select, textarea, summary, input:not([type='hidden']), " +
        ".nav-links a, .wordmark",
    ),
  ].filter((el) => {
    if (!isVisible(el)) return false;
    if (el.tagName === "INPUT" && el.closest("label")) return false;
    if (
      el.tagName === "INPUT" &&
      (el.getAttribute("type") === "radio" ||
        el.getAttribute("type") === "checkbox") &&
      el.getBoundingClientRect().width <= 1
    ) {
      return false;
    }
    return true;
  });

  for (const el of controls) {
    const rect = el.getBoundingClientRect();
    const label = (
      el.getAttribute("aria-label") ??
      el.textContent ??
      el.getAttribute("name") ??
      ""
    )
      .trim()
      .slice(0, 40);
    const item = {
      tag: el.tagName.toLowerCase(),
      className: typeof el.className === "string" ? el.className.slice(0, 60) : "",
      label,
      width: Math.round(rect.width),
      height: Math.round(rect.height),
    };
    if (rect.width < 24 || rect.height < 24) {
      results.undersized.push(item);
    } else if (item.className.includes("btn") && rect.height < 40) {
      results.smallButtons.push(item);
    }
  }

  for (const el of document.querySelectorAll("body *")) {
    if (!isVisible(el)) continue;
    if (
      el.classList.contains("visually-hidden") ||
      el.getAttribute("aria-hidden") === "true"
    ) {
      continue;
    }
    const style = getComputedStyle(el);
    if (inClippedSubtree(el)) continue;
    if (style.whiteSpace !== "nowrap") continue;
    if (el.scrollWidth <= el.clientWidth + 1) continue;
    if (style.textOverflow === "ellipsis") continue;
    results.clipped.push({
      tag: el.tagName.toLowerCase(),
      className: typeof el.className === "string" ? el.className.slice(0, 60) : "",
      scrollWidth: el.scrollWidth,
      clientWidth: el.clientWidth,
      text: (el.textContent ?? "").trim().slice(0, 60),
    });
  }

  return results;
}

function evaluate(result, width) {
  const failures = [];
  if (result.blocked) {
    failures.push(
      "page was replaced by a Cloudflare challenge; pass --user-agent or an authenticated --cookie",
    );
  }
  if (result.documentWidth > result.viewportWidth + 1) {
    failures.push(
      `document is ${result.documentWidth}px wide in a ${result.viewportWidth}px viewport`,
    );
  }
  if (result.offenders.length > 0) {
    failures.push(
      `${result.offenders.length} element(s) outside the viewport: ` +
        result.offenders
          .slice(0, 3)
          .map((item) => `${item.tag}.${item.className || "-"}`)
          .join(", "),
    );
  }
  if (result.navHeight > 80) {
    failures.push(`navigation is ${result.navHeight}px tall (max 80px)`);
  }
  if (result.navGap !== null && result.navGap < 4 && width <= 767) {
    failures.push(`navigation items are ${result.navGap}px apart (min 4px)`);
  }
  if (
    result.navItems.some(
      (item, index) =>
        index > 0 && item.left < result.navItems[index - 1].right - 1,
    )
  ) {
    failures.push("navigation items overlap");
  }
  if (result.undersized.length > 0) {
    failures.push(
      `${result.undersized.length} control(s) below 24px: ` +
        result.undersized
          .slice(0, 3)
          .map((item) => `${item.tag} ${item.width}x${item.height}`)
          .join(", "),
    );
  }
  if (width <= 767 && result.smallButtons.length > 0) {
    failures.push(
      `${result.smallButtons.length} button(s) below 40px: ` +
        result.smallButtons
          .slice(0, 3)
          .map((item) => `${item.label || item.className} ${item.height}px`)
          .join(", "),
    );
  }
  if (result.clipped.length > 0) {
    failures.push(
      `${result.clipped.length} nowrap text node(s) clipped without ellipsis: ` +
        result.clipped
          .slice(0, 3)
          .map((item) => `${item.tag}.${item.className || "-"}`)
          .join(", "),
    );
  }
  return failures;
}

await mkdir(outDir, { recursive: true });

const executablePath = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE;
const browser = await chromium.launch(
  executablePath ? { executablePath } : undefined,
);
const browserMajor = browser.version().split(".")[0];
const userAgent =
  customUserAgent ||
  `Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) ` +
    `Chrome/${browserMajor}.0.0.0 Safari/537.36`;
const report = [];
let failed = 0;

try {
  for (const width of widths) {
    const context = await browser.newContext({
      viewport: { width, height: 844 },
      deviceScaleFactor: 1,
      colorScheme,
      userAgent,
      locale: "en-US",
      timezoneId: "Europe/Zurich",
      // Desktop Chromium's `isMobile` emulation expands innerHeight to the
      // document height, which makes the page unscrollable and hides the
      // lower half from viewport checks. Width-driven media queries are the
      // thing under test, so keep a real scrolling viewport and add touch.
      isMobile: false,
      hasTouch: width < 768,
    });

    if (cookie !== "") {
      const origin = new URL(baseUrl);
      await context.addCookies([
        {
          name: "uruk_browser",
          value: cookie,
          domain: origin.hostname,
          path: "/",
          httpOnly: true,
          secure: origin.protocol === "https:",
          sameSite: "Lax",
        },
      ]);
    }

    const page = await context.newPage();
    for (const [name, route] of routes) {
      const url = `${baseUrl}${route}`;
      let navigationError = null;
      try {
        await page.goto(url, { waitUntil: "domcontentloaded", timeout: 45000 });
        await page.waitForTimeout(900);
      } catch (error) {
        navigationError = error instanceof Error ? error.message : String(error);
      }

      if (navigationError !== null) {
        failed += 1;
        report.push({ width, name, url, failures: [`navigation failed: ${navigationError}`] });
        console.log(`FAIL ${width}px ${name}: navigation failed: ${navigationError}`);
        continue;
      }

      const result = await page.evaluate(auditPage);
      const failures = evaluate(result, width);
      await page.screenshot({
        path: path.join(outDir, `${width}-${slug(name)}.png`),
        fullPage: true,
      });

      report.push({ width, name, url, ...result, failures });
      if (failures.length === 0) {
        console.log(`PASS ${width}px ${name}`);
      } else {
        failed += 1;
        console.log(`FAIL ${width}px ${name}: ${failures.join("; ")}`);
      }
    }
    await context.close();
  }
} finally {
  await browser.close();
}

await writeFile(
  path.join(outDir, "report.json"),
  `${JSON.stringify({ baseUrl, checkedAt: new Date().toISOString(), report }, null, 2)}\n`,
);

console.log(
  failed === 0
    ? `\nAll ${report.length} page checks passed (screenshots in ${outDir}).`
    : `\n${failed} of ${report.length} page checks failed. See ${path.join(outDir, "report.json")}.`,
);
process.exit(failed === 0 ? 0 : 1);
