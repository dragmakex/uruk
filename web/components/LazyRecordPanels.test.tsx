import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { tournamentFixture, snapshotFixture } from "@/lib/fixtures";
import { TournamentFeed } from "./TournamentFeed";
import { DecisionLedger } from "./DecisionLedger";
import { RankingList } from "./RankingList";

const panels = [
  {
    label: "match log",
    pager: "Match log",
    summary: "7 matches recorded",
    route: "tournament",
    envelope: "tournament",
    field: "matches",
    render: (revision: number) => (
      <TournamentFeed runId="run_test" matchCount={revision} />
    ),
  },
  {
    label: "decisions",
    pager: "Decisions",
    summary: "7 decisions recorded",
    route: "decisions",
    envelope: "ledger",
    field: "decisions",
    render: (revision: number) => (
      <DecisionLedger runId="run_test" decisionCount={revision} />
    ),
  },
  {
    label: "candidates",
    pager: "Candidates",
    summary: "7 candidates recorded",
    route: "candidates",
    envelope: "candidates",
    field: "items",
    render: (revision: number) => (
      <RankingList runId="run_test" itemCount={7} revision={String(revision)} />
    ),
  },
];

function records(field: string) {
  const match = (tournamentFixture() as { matches: object[] }).matches[0]!;
  const item = (snapshotFixture() as { items: object[] }).items[0]!;
  return Array.from({ length: 7 }, (_, index) =>
    field === "matches"
      ? { ...match, id: `m${7 - index}`, rationale: `record ${7 - index}` }
      : field === "decisions"
        ? {
            id: `d${index + 1}`,
            kind: "scheduling",
            actor: "supervisor",
            reason: `record ${index + 1}`,
            referenced: [],
            created_at: "2026-01-01T00:00:00Z",
          }
        : { ...item, id: `i${index + 1}`, title: `record ${index + 1}` },
  );
}

function stub(panel: (typeof panels)[number]) {
  const rows = records(panel.field);
  const fetch = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
    const parsed = new URL(String(url), "http://localhost");
    expect(parsed.pathname).toBe(`/api/runs/run_test/${panel.route}`);
    expect(init?.cache).toBe("no-store");
    const offset = Number(parsed.searchParams.get("offset") ?? 0);
    const limit = Number(parsed.searchParams.get("limit") ?? 5);
    return new Response(
      JSON.stringify({
        ok: true,
        [panel.envelope]: {
          run_id: "run_test",
          total: 7,
          offset,
          limit,
          through: panel.field === "items" ? null : 7,
          [panel.field]: rows.slice(offset, offset + limit),
        },
      }),
    );
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

afterEach(() => vi.unstubAllGlobals());

for (const panel of panels)
  describe(`${panel.pager} lazy reads`, () => {
    it("does not request anything until Open, including after live counts change", async () => {
      const fetch = stub(panel);
      const { rerender } = render(panel.render(7));
      await act(async () => {});
      expect(fetch).not.toHaveBeenCalled();
      expect(screen.getByText(panel.summary)).toBeVisible();
      const toggle = screen.getByRole("button", {
        name: `Open ${panel.label}`,
      });
      expect(toggle.textContent).toBe("Open");
      expect(toggle).toHaveAttribute("aria-expanded", "false");
      expect(
        document.getElementById(toggle.getAttribute("aria-controls")!),
      ).not.toBeNull();
      rerender(panel.render(8));
      await act(async () => {});
      expect(fetch).not.toHaveBeenCalled();
      fireEvent.click(toggle);
      await screen.findByText(
        panel.field === "matches"
          ? "record 7"
          : panel.field === "items"
            ? "1. record 1"
            : "record 1",
      );
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(String(fetch.mock.calls[0]![0])).toContain("offset=0&limit=5");
      expect(String(fetch.mock.calls[0]![0])).not.toContain("through=");
    });

    it("fetches the next slice only on Next and preserves the history watermark", async () => {
      const fetch = stub(panel);
      render(panel.render(7));
      fireEvent.click(
        screen.getByRole("button", { name: `Open ${panel.label}` }),
      );
      await screen.findByText("Page 1 of 2");
      expect(fetch).toHaveBeenCalledTimes(1);
      const next = screen.getByRole("button", {
        name: `Next ${panel.pager} page`,
      });
      expect(next.textContent).toBe("→");
      fireEvent.click(next);
      await screen.findByText("Page 2 of 2");
      expect(fetch).toHaveBeenCalledTimes(2);
      expect(String(fetch.mock.calls[1]![0])).toContain("offset=5&limit=5");
      if (panel.field !== "items")
        expect(String(fetch.mock.calls[1]![0])).toContain("through=7");
      expect(
        screen.getByRole("button", { name: `Next ${panel.pager} page` }),
      ).toBeDisabled();
      expect(
        screen.getByRole("button", { name: `Previous ${panel.pager} page` })
          .textContent,
      ).toBe("←");
      expect(
        screen.queryByText(
          panel.field === "matches"
            ? "record 7"
            : panel.field === "items"
              ? "1. record 1"
              : "record 1",
        ),
      ).toBeNull();
      fireEvent.click(
        screen.getByRole("button", { name: `Previous ${panel.pager} page` }),
      );
      await screen.findByText("Page 1 of 2");
      expect(fetch).toHaveBeenCalledTimes(3);
    });

    it("keeps a loaded page on close/reopen and never fetches on live updates", async () => {
      const fetch = stub(panel);
      const { rerender } = render(panel.render(7));
      fireEvent.click(
        screen.getByRole("button", { name: `Open ${panel.label}` }),
      );
      await screen.findByText("Page 1 of 2");
      fireEvent.click(
        screen.getByRole("button", { name: `Close ${panel.label}` }),
      );
      fireEvent.click(
        screen.getByRole("button", { name: `Open ${panel.label}` }),
      );
      expect(fetch).toHaveBeenCalledTimes(1);
      rerender(panel.render(8));
      await act(async () => {});
      expect(fetch).toHaveBeenCalledTimes(1);
      fireEvent.click(
        screen.getByRole("button", { name: `Refresh ${panel.label}` }),
      );
      await waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
      expect(String(fetch.mock.calls[1]![0])).not.toContain("through=");
    });

    it("reports a failed page, retains the disclosure and retries only on request", async () => {
      const fetch = stub(panel);
      fetch.mockRejectedValueOnce(new Error("offline"));
      render(panel.render(7));
      fireEvent.click(
        screen.getByRole("button", { name: `Open ${panel.label}` }),
      );
      await screen.findByText(/could not be read/i);
      expect(screen.queryByText(/No .*yet/)).toBeNull();
      expect(fetch).toHaveBeenCalledTimes(1);
      fireEvent.click(
        screen.getByRole("button", { name: `Retry ${panel.label}` }),
      );
      await screen.findByText("Page 1 of 2");
      expect(fetch).toHaveBeenCalledTimes(2);
    });

    it("preserves the history's stale warning while paging an older pinned read", async () => {
      const fetch = stub(panel);
      const { rerender } = render(panel.render(7));
      fireEvent.click(
        screen.getByRole("button", { name: `Open ${panel.label}` }),
      );
      await screen.findByText("Page 1 of 2");
      rerender(panel.render(8));
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(screen.getByRole("status")).toHaveTextContent(
        /record has changed/,
      );
      fireEvent.click(
        screen.getByRole("button", { name: `Next ${panel.pager} page` }),
      );
      await screen.findByText("Page 2 of 2");
      if (panel.field !== "items") {
        expect(screen.getByRole("status")).toHaveTextContent(
          /record has changed/,
        );
        expect(String(fetch.mock.calls[1]![0])).toContain("through=7");
      }
    });

    it("aborts an unfinished page when closed and ignores its late response", async () => {
      let resolve!: (response: Response) => void;
      const fetch = stub(panel);
      fetch.mockImplementationOnce(
        () =>
          new Promise<Response>((done) => {
            resolve = done;
          }),
      );
      render(panel.render(7));
      fireEvent.click(
        screen.getByRole("button", { name: `Open ${panel.label}` }),
      );
      await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
      expect(screen.getByRole("status")).toHaveTextContent(/Reading/i);
      const signal = fetch.mock.calls[0]![1]?.signal;
      fireEvent.click(
        screen.getByRole("button", { name: `Close ${panel.label}` }),
      );
      expect(signal?.aborted).toBe(true);
      await act(async () => {
        resolve(new Response(JSON.stringify({ unexpected: true })));
      });
      expect(screen.queryByText(/could not be read/i)).toBeNull();
      fireEvent.click(
        screen.getByRole("button", { name: `Open ${panel.label}` }),
      );
      await screen.findByText("Page 1 of 2");
      expect(fetch).toHaveBeenCalledTimes(2);
    });
  });
