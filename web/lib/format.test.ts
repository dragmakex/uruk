import { describe, expect, it } from "vitest";
import {
  elapsedBetween,
  formatClock,
  formatElo,
  rankingMethod,
  roundLabel,
  shortRunId,
  stateLabel,
} from "./format";

describe("formatClock", () => {
  it("renders mm:ss under an hour", () => {
    expect(formatClock(0)).toBe("00:00");
    expect(formatClock(257)).toBe("04:17");
    expect(formatClock(3599)).toBe("59:59");
  });

  it("renders h:mm:ss from one hour", () => {
    expect(formatClock(3600)).toBe("1:00:00");
    expect(formatClock(3661)).toBe("1:01:01");
  });

  it("clamps negative inputs to zero", () => {
    expect(formatClock(-5)).toBe("00:00");
  });
});

describe("elapsedBetween", () => {
  it("returns whole seconds between two RFC-3339 instants", () => {
    expect(
      elapsedBetween("2026-10-04T14:00:00Z", "2026-10-04T14:04:17Z"),
    ).toBe(257);
  });

  it("returns null when either timestamp is unparsable", () => {
    expect(elapsedBetween("junk", "2026-10-04T14:04:17Z")).toBeNull();
  });
});

describe("shortRunId", () => {
  it("keeps the last four characters of the raw id", () => {
    expect(shortRunId("run_20cda5713eeb4401a5267e0c73836b33")).toBe("6b33");
  });

  it("falls back to the whole id when it is short", () => {
    expect(shortRunId("r1")).toBe("r1");
  });
});

describe("rankingMethod", () => {
  it("is tournament when debates may run multiple turns", () => {
    expect(rankingMethod(5)).toBe("tournament");
  });

  it("is simple when debates are capped at one turn", () => {
    expect(rankingMethod(1)).toBe("simple");
  });
});

describe("roundLabel", () => {
  it("shows the live round for a running run", () => {
    expect(roundLabel("running", 1, 4)).toBe("round 2 of 4");
  });

  it("never exceeds the budgeted rounds", () => {
    expect(roundLabel("running", 4, 4)).toBe("round 4 of 4");
  });

  it("shows completed rounds for a finished run", () => {
    expect(roundLabel("completed", 3, 4)).toBe("3 of 4 rounds");
  });
});

describe("stateLabel", () => {
  it("humanizes the kebab-case run states", () => {
    expect(stateLabel("running")).toBe("running");
    expect(stateLabel("waiting-for-human")).toBe("waiting for you");
    expect(stateLabel("budget-exhausted")).toBe("budget exhausted");
  });
});

describe("formatElo", () => {
  it("rounds to a whole number as the figure does", () => {
    expect(formatElo(1412.23)).toBe("1412");
  });
});
