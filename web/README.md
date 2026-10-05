# uruk-web

The Next.js frontend for Uruk's local web console. Architecture, API
contract, security posture, and reverse-proxy setup live in
[`../docs/WEB.md`](../docs/WEB.md); this file covers the frontend itself.

**No authentication exists anywhere in this stack. Local use only; see
docs/WEB.md before binding anything to a non-loopback address.**

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
- Reports render as the exact exported markdown bytes in a preformatted
  block rather than a lossy HTML re-interpretation.

## Responsive topology

One semantic DOM tree at every width:

- ≥1440px: Supervisor left, three-column worker grid, measured SVG wires
  (dashed while the downstream agent is thinking).
- 1024-1439px: condensed spacing, wires retained.
- 768-1023px: Supervisor full width, two-column grid, wires hidden; each
  card shows its textual `feeds …` pipeline context instead.
- <768px: single-column pipeline order, compact sticky run header,
  collapsible key.

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
and the live run view driven by a scripted EventSource. There is no
Playwright suite in-repo; cross-viewport rendering was verified manually
with Playwright screenshots during development.
