import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { tournamentFixture } from "@/lib/fixtures";
import { TournamentFeed } from "./TournamentFeed";

/** Serve one canned tournament response and record what was asked for. */
function stubTournament(body: unknown, status = 200): string[] {
  const seen: string[] = [];
  vi.stubGlobal(
    "fetch",
    async (url: RequestInfo | URL): Promise<Response> => {
      seen.push(String(url));
      return new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      });
    },
  );
  return seen;
}

function tournamentWithMatches(count: number) {
  const fixture = tournamentFixture() as {
    run_id: string;
    matches: Array<Record<string, unknown>>;
  };
  const first = fixture.matches[0]!;
  return {
    ...fixture,
    matches: Array.from({ length: count }, (_, index) => ({
      ...first,
      id: `match_${index + 1}`,
      rationale: `rationale ${index + 1}`,
      created_at: `2026-10-04T14:${String(index).padStart(2, "0")}:00Z`,
    })),
  };
}

async function openMatchLog() {
  const user = userEvent.setup();
  const toggle = await screen.findByRole("button", { name: "Open match log" });
  expect(toggle.textContent).toBe("Open");
  expect(toggle).toHaveAttribute("aria-expanded", "false");
  await user.click(toggle);
  return screen.findByRole("list", { name: "Match log" });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("TournamentFeed", () => {
  it("asks the run-scoped tournament route", async () => {
    const seen = stubTournament({ ok: true, tournament: tournamentFixture() });
    render(<TournamentFeed runId="run_0412aa" matchCount={2} />);
    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0]).toContain("/api/runs/run_0412aa/tournament");
  });

  it("shows each match with its verdict, judge, and rationale", async () => {
    stubTournament({ ok: true, tournament: tournamentFixture() });
    render(<TournamentFeed runId="run_0412aa" matchCount={2} />);

    const log = await openMatchLog();
    const cards = Array.from(log.children) as HTMLElement[];
    expect(cards).toHaveLength(2);

    // Newest first: the declined debate leads, the decided pairwise follows.
    expect(within(cards[0]!).getByText(/multi-turn debate/)).toBeDefined();
    expect(within(cards[0]!).getByText(/no basis to decide/)).toBeDefined();
    expect(
      within(cards[0]!).getByText(/nothing to separate them/),
    ).toBeDefined();
    expect(within(cards[1]!).getByText(/first side won/)).toBeDefined();
    expect(within(cards[1]!).getByText(/judge deepseek-v4\.1-flash/)).toBeDefined();
  });

  it("marks the winning side and its signed rating movement", async () => {
    stubTournament({ ok: true, tournament: tournamentFixture() });
    render(<TournamentFeed runId="run_0412aa" matchCount={2} />);

    const log = await openMatchLog();
    const decided = Array.from(log.children)[1] as HTMLElement;

    const sides = within(decided).getAllByRole("listitem");
    expect(within(sides[0]!).getByText("W")).toBeDefined();
    expect(within(sides[0]!).getByText("+16")).toBeDefined();
    expect(within(sides[1]!).getByText("L")).toBeDefined();
    expect(within(sides[1]!).getByText("-16")).toBeDefined();
  });

  it("declines to mark a winner when the judge found no basis", async () => {
    stubTournament({ ok: true, tournament: tournamentFixture() });
    render(<TournamentFeed runId="run_0412aa" matchCount={2} />);

    const log = await openMatchLog();
    const declined = Array.from(log.children)[0] as HTMLElement;
    const sides = within(declined).getAllByRole("listitem");

    expect(within(sides[0]!).queryByText("W")).toBeNull();
    expect(within(declined).getAllByText("=")).toHaveLength(2);
    expect(within(declined).getByText(/ratings unchanged/)).toBeDefined();
  });

  it("says the tournament has not started rather than showing an empty bracket", async () => {
    stubTournament({ ok: true, tournament: { run_id: "run_0412aa", matches: [] } });
    render(<TournamentFeed runId="run_0412aa" matchCount={0} />);
    expect(await screen.findByText(/No matches yet/)).toBeDefined();
  });

  it("admits a failed read instead of rendering a stale or invented log", async () => {
    stubTournament({ ok: false, error: "unavailable", kind: "internal" }, 500);
    render(<TournamentFeed runId="run_0412aa" matchCount={2} />);
    expect(await screen.findByText(/could not be read/)).toBeDefined();
  });

  it("reports a feed that fails validation as unreadable", async () => {
    stubTournament({ ok: true, tournament: { run_id: "run_0412aa", matches: [{}] } });
    render(<TournamentFeed runId="run_0412aa" matchCount={1} />);
    expect(await screen.findByText(/could not be read/)).toBeDefined();
  });

  it("shows only the five most recent matches on the first page", async () => {
    stubTournament({ ok: true, tournament: tournamentWithMatches(7) });
    render(<TournamentFeed runId="run_0412aa" matchCount={7} />);

    const log = await openMatchLog();
    expect(log.children).toHaveLength(5);
    expect(within(log).getByText("rationale 7")).toBeDefined();
    expect(within(log).getByText("rationale 3")).toBeDefined();
    expect(within(log).queryByText("rationale 2")).toBeNull();
    expect(screen.getByText("Page 1 of 2")).toBeDefined();
  });

  it("pages through older matches without extending the document forever", async () => {
    const user = userEvent.setup();
    stubTournament({ ok: true, tournament: tournamentWithMatches(7) });
    render(<TournamentFeed runId="run_0412aa" matchCount={7} />);
    await openMatchLog();

    expect(screen.getByRole("button", { name: "Previous Match log page" }).textContent).toBe("←");
    const next = screen.getByRole("button", { name: "Next Match log page" });
    expect(next.textContent).toBe("→");
    expect(screen.queryByText(/older matches|newer matches/i)).toBeNull();
    await user.click(next);

    const log = screen.getByRole("list", { name: "Match log" });
    expect(log.children).toHaveLength(2);
    expect(within(log).getByText("rationale 2")).toBeDefined();
    expect(within(log).getByText("rationale 1")).toBeDefined();
    expect(within(log).queryByText("rationale 7")).toBeNull();
    expect(screen.getByText("Page 2 of 2")).toBeDefined();
  });

  it("is closed by default and can open, close, and reopen", async () => {
    const user = userEvent.setup();
    stubTournament({ ok: true, tournament: tournamentWithMatches(6) });
    render(<TournamentFeed runId="run_0412aa" matchCount={6} />);

    const closedToggle = await screen.findByRole("button", { name: "Open match log" });
    expect(screen.queryByRole("list", { name: "Match log" })).toBeNull();
    expect(closedToggle.textContent).toBe("Open");
    await user.click(closedToggle);

    const toggle = screen.getByRole("button", { name: "Close match log" });
    expect(toggle.textContent).toBe("Close");
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    await user.click(toggle);
    expect(screen.queryByRole("list", { name: "Match log" })).toBeNull();
    expect(screen.getByRole("button", { name: "Open match log" }).textContent).toBe("Open");

    await user.click(screen.getByRole("button", { name: "Open match log" }));
    expect(await screen.findByRole("list", { name: "Match log" })).toBeDefined();
  });
});
