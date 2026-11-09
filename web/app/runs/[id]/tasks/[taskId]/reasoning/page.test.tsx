import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import TaskReasoningPage from "./page";

const { fetchTaskReasoningMock } = vi.hoisted(() => ({
  fetchTaskReasoningMock: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  ApiFailure: class ApiFailure extends Error {
    kind = "test";
  },
  fetchTaskReasoning: fetchTaskReasoningMock,
}));

vi.mock("@/lib/server-identity", () => ({
  identityCookieHeader: vi.fn().mockResolvedValue(""),
}));

beforeEach(() => {
  fetchTaskReasoningMock.mockReset();
  fetchTaskReasoningMock.mockResolvedValue({
    task: {
      id: "task_generation",
      role: "generation",
      strategy: "literature",
      state: "completed",
    },
    reasoning: [
      {
        started_at: "2026-11-06T10:00:00Z",
        finished_at: "2026-11-06T10:00:02Z",
        model: "deepseek-test",
        content: "Inspect the evidence first.\nThen compare candidates.",
      },
    ],
  });
});

describe("TaskReasoningPage", () => {
  it("renders the provider trace verbatim and states its provenance", async () => {
    render(
      await TaskReasoningPage({
        params: Promise.resolve({ id: "run_abc", taskId: "task_generation" }),
      }),
    );

    expect(screen.getByText(/verbatim reasoning traces/)).toBeInTheDocument();
    expect(screen.getByText("deepseek-test · 2026-11-06T10:00:00Z")).toBeInTheDocument();
    expect(screen.getByText(/Inspect the evidence first/)).toHaveTextContent(
      "Inspect the evidence first. Then compare candidates.",
    );
  });

  it("explains when the provider returned no explicit reasoning", async () => {
    fetchTaskReasoningMock.mockResolvedValueOnce({
      task: {
        id: "task_generation",
        role: "generation",
        strategy: "literature",
        state: "running",
      },
      reasoning: [],
    });

    render(
      await TaskReasoningPage({
        params: Promise.resolve({ id: "run_abc", taskId: "task_generation" }),
      }),
    );

    expect(
      screen.getByRole("heading", { name: "No provider reasoning recorded" }),
    ).toBeInTheDocument();
  });
});
