import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import RunsPage from "./page";

const { fetchRunsMock } = vi.hoisted(() => ({
  fetchRunsMock: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  ApiFailure: class ApiFailure extends Error {
    kind = "test";
  },
  fetchRuns: fetchRunsMock,
}));

vi.mock("@/lib/server-identity", () => ({
  identityCookieHeader: vi.fn().mockResolvedValue(""),
}));

beforeEach(() => {
  fetchRunsMock.mockReset();
  fetchRunsMock.mockResolvedValue([]);
});

describe("RunsPage", () => {
  it("shows only the lower empty-state Start research action", async () => {
    render(await RunsPage());

    expect(screen.getAllByRole("link", { name: "Start research" })).toHaveLength(1);
    const action = screen.getByRole("link", { name: "Start research" });
    expect(action).toHaveAttribute("href", "/runs/new");
    expect(action.closest(".empty-state")).not.toBeNull();
  });
});

describe("RunsPage queue facts", () => {
  it("shows queue position and provider waits on the run cards", async () => {
    fetchRunsMock.mockResolvedValue([
      {
        id: "run_queued",
        state: "queued",
        stop_condition: null,
        iterations: 0,
        question: "a queued question",
        mode: "task",
        created_at: "2026-10-04T14:00:00Z",
        updated_at: "2026-10-04T14:00:00Z",
        queue_position: 2,
        paused_since: null,
      },
      {
        id: "run_waiting",
        state: "running",
        stop_condition: null,
        iterations: 0,
        question: "a waiting question",
        mode: "task",
        created_at: "2026-10-04T14:00:00Z",
        updated_at: "2026-10-04T14:00:00Z",
        queue_position: null,
        paused_since: "2026-10-04T14:05:00Z",
      },
    ]);
    render(await RunsPage());

    expect(screen.getByText(/queued, position 2/)).toBeInTheDocument();
    expect(screen.getByText(/waiting for provider/)).toBeInTheDocument();
    expect(screen.queryByText(/failed/)).not.toBeInTheDocument();
  });
});
