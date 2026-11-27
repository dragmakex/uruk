# uruk-web

The Next.js frontend for Urukai's local web console. Architecture, API
contract, security posture, and reverse-proxy setup live in
[`../docs/WEB.md`](../docs/WEB.md); this file covers the frontend itself.

**Identity is an anonymous browser cookie set and checked by the Rust
API; this frontend never sees the token (it is HttpOnly). Server
Components forward the incoming cookie header to the API via
`lib/server-identity.ts`. Local use first; see docs/WEB.md before
binding anything to a non-loopback address.**

## Commands

```sh
bun install
bun run dev        # dev server on :3000, /api proxied to 127.0.0.1:7913
bun run test       # vitest (jsdom)
bun run lint       # eslint
bun run typecheck  # tsc --noEmit (strict)
bun run build      # production build
bun run start      # serve the production build
bun run check      # lint + typecheck + test + build
bunx playwright install chromium   # once, for the responsive gate
bun run check:mobile -- --base-url http://127.0.0.1:3000
                   # headless responsive audit (320-768px) + screenshots
```

Tooling is Bun (`packageManager` pins `bun@1.3.14`; the lockfile is
`bun.lock`). `unrs-resolver` is in `trustedDependencies` because its
postinstall links the prebuilt native binding used by
eslint-config-next's import resolver. The test suite is Vitest with a
jsdom environment, so run it as `bun run test` (the `bun test` built-in
runner has no jsdom environment and is not used here).

## Layout

- `app/` App Router pages. Server Components by default; client
  components exist only for the SSE live view, the theme toggle, and the
  run form. Pages that read the API are `force-dynamic`; the landing and
  run-specification shells are static.
- `components/` presentation and the three client leaves (`LiveRun`,
  `RunForm`, `ThemeToggle`, plus the measured SVG wire layer in
  `Topology`).
- `lib/` the typed API client (`api.ts`), hand-written runtime validators
  (`parse.ts`) so no network JSON is trusted unparsed, the EventSource
  lifecycle hook and its pure reducer (`sse.ts`), formatting helpers, and
  static role metadata.

## Design system

`app/globals.css` reproduces the reference pages: light and dark duals,
dotted technical canvas, exact square geometry (a global
`border-radius: 0`), one-pixel navy borders, filled headers for thinking
agents, dashed boxes for idle ones, and monospace microcopy. Tokens are
CSS custom properties switched on `<html data-theme>`; an inline script
applies the stored or system theme before first paint.

Honesty notes:

- The reference's typefaces are not identified or licensed, so the system
  sans and mono stacks stand in. No external font or CDN is loaded.
- The reference form shows an "est. 4 min" estimate; this UI shows the
  real budget that is actually sent (`budget 200 model calls, 60 min, 10
  rounds`) because the engine records budgets, not duration estimates.
- Agent cards render recorded task facts (current strategy, queue depths,
  last failure) and run-wide counts. No activity prose is invented.
- The report page renders the markdown for reading, but the canonical
  record stays available unmodified: Download Markdown serves the exact
  exported `REPORT.md` bytes and Download PDF serves its deterministic
  PDF twin, both straight from the engine. Raw HTML inside the
  model-generated markdown is never rendered. **Human report** opens this
  readable report: the leading five ideas by Elo, how each works, practical
  steps, and an annotated reading guide with direct links and access limits.
  Model counts and other run bookkeeping sit in a closed **Run details**
  disclosure after the explanation. **Agent report** downloads the
  self-contained `AGENT_REPORT.json` audit snapshot from the same export.
  Older runs without the new artifact return `not_found` until explicitly
  re-exported; clicking the link never spends budget or starts research.

## Responsive topology

One semantic DOM tree at every width:

- ≥1440px: Supervisor left, three-column worker grid, measured SVG wires
  (dashed while the downstream agent is thinking).
- 1024-1439px: condensed spacing, wires retained.
- 768-1023px: Supervisor full width, two-column grid, wires hidden; each
  card shows its textual `feeds …` pipeline context instead.
- <768px: single-column pipeline order, compact sticky run header,
  collapsible key. The run header is measured at runtime so the sticky
  record controls stay below it when the question wraps to two lines.

The layout is verified down to 320px. Both data tables (the library and
the run-form agent legend) become stacked records rather than forcing a
horizontal scroll, and interactive controls keep a 44px touch target.

## Accessibility

Semantic landmarks and headings, a real radio group in the run form,
visible focus outlines, text state labels everywhere (never color-only),
`aria-hidden` on the decorative wire layer, a polite `aria-live` region
that announces run-state transitions only, and transitions gated behind
`prefers-reduced-motion`.

## Testing

Vitest with jsdom and Testing Library. Covered behavior: every runtime
parser (valid, missing, and type-drifted payloads), formatting rules, the
SSE reducer's full lifecycle (live, reconnecting, parse errors, ended,
failed), run-form validation and error bodies, agent-card fact rendering,
and the live run view driven by a scripted EventSource.

`bun run check:mobile` is the layout gate. It drives the real pages with
Playwright at 320, 360, 390, 414, and 768px and fails on sideways page
scroll, an element outside the viewport, a cramped or wrapping header,
an undersized control, or clipped `nowrap` text. Screenshots and the
JSON report land in `.mobile-check/`. Run it against a local server
before shipping UI changes; add `--run-id <id>` (and `--cookie <value>`
when the run belongs to an existing browser identity) to cover the live
run, report, and citations pages, plus `--task-id <id>` for a reasoning
trace page. Add `--color-scheme dark` to run the same gate against the
dark tokens. The gate sends a normal browser user agent by default and
fails with an explicit message if Cloudflare replaces a page with a
challenge, rather than reporting it as a layout bug; pass
`--user-agent` to override it.
