import { describe, expect, it } from "vitest";
import { snapshotFixture, tournamentFixture } from "./fixtures";
import {
  ParseError,
  parseCandidatePage,
  parseDecisionLedger,
  parseTournamentFeed,
} from "./parse";

const matches = (tournamentFixture() as { matches: unknown[] }).matches;
const items = (snapshotFixture() as { items: unknown[] }).items;
const decision = {
  id: "d1",
  kind: "scheduling",
  actor: "supervisor",
  reason: "Recorded choice",
  referenced: [],
  created_at: "2026-01-01T00:00:00Z",
};

for (const contract of [
  {
    name: "matches",
    field: "matches",
    parse: parseTournamentFeed,
    rows: matches,
  },
  {
    name: "decisions",
    field: "decisions",
    parse: parseDecisionLedger,
    rows: [decision],
  },
  {
    name: "candidates",
    field: "items",
    parse: parseCandidatePage,
    rows: items,
  },
])
  describe(`${contract.name} page contract`, () => {
    function page() {
      return {
        run_id: "run_test",
        total: contract.rows.length,
        offset: 0,
        limit: 5,
        through: contract.field === "items" ? null : 7,
        [contract.field]: contract.rows,
      };
    }

    it("retains the exact server bounds alongside only the requested records", () => {
      expect(contract.parse(page())).toMatchObject({
        total: contract.rows.length,
        offset: 0,
        limit: 5,
      });
    });

    it("rejects a legacy unbounded payload without page metadata", () => {
      expect(() =>
        contract.parse({ run_id: "run_test", [contract.field]: contract.rows }),
      ).toThrow(ParseError);
    });

    it("rejects fractional, unsafe, negative, or overlarge page metadata", () => {
      for (const invalid of [
        { total: -1 },
        { offset: 0.5 },
        { total: Number.MAX_SAFE_INTEGER + 1 },
        { limit: 0 },
        { limit: 101 },
        { through: -1 },
        { through: 1.5 },
      ]) {
        expect(() => contract.parse({ ...page(), ...invalid })).toThrow(
          ParseError,
        );
      }
    });

    it("rejects a short or excessive body instead of pretending it is a real page", () => {
      expect(() => contract.parse({ ...page(), total: 10 })).toThrow(
        ParseError,
      );
      expect(() =>
        contract.parse({
          ...page(),
          limit: 1,
          [contract.field]: Array(6).fill(contract.rows[0]),
        }),
      ).toThrow(ParseError);
    });

    it("validates the actual row rather than merely trusting pagination fields", () => {
      expect(() =>
        contract.parse({ ...page(), total: 1, [contract.field]: [{}] }),
      ).toThrow(ParseError);
    });
  });
