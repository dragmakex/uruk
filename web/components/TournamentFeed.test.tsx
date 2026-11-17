import { render, screen, waitFor, within } from "@testing-library/react";
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

    const log = await screen.findByRole("list", { name: "Match log" });
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

    const log = await screen.findByRole("list", { name: "Match log" });
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

    const log = await screen.findByRole("list", { name: "Match log" });
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
});
