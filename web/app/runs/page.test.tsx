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
