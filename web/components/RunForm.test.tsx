import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ApiFailure } from "@/lib/api";
import { RunForm } from "./RunForm";

const pushMock = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: pushMock }),
}));

const startRunMock = vi.fn();
vi.mock("@/lib/api", async () => {
  const actual = await vi.importActual<typeof import("@/lib/api")>("@/lib/api");
  return {
    ...actual,
    startRun: (input: unknown) => startRunMock(input),
  };
});

beforeEach(() => {
  pushMock.mockReset();
  startRunMock.mockReset();
});

describe("RunForm", () => {
  it("refuses to submit an empty goal and says why", async () => {
    const user = userEvent.setup();
    render(<RunForm />);
    await user.click(screen.getByRole("button", { name: "Start research" }));
    expect(screen.getByRole("alert")).toHaveTextContent(
      "State the research goal first.",
    );
    expect(startRunMock).not.toHaveBeenCalled();
  });

  it("posts every web run with tournament ranking and navigates to it", async () => {
    startRunMock.mockResolvedValue("run_new1");
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(
      screen.getByLabelText("Research goal"),
      "why do the measurements disagree",
    );
    await user.click(screen.getByRole("button", { name: "Start research" }));

    expect(startRunMock).toHaveBeenCalledWith({
      goal: "why do the measurements disagree",
      mode: "campaign",
      ranking: "tournament",
    });
    expect(pushMock).toHaveBeenCalledWith("/runs/run_new1");
  });

  it("offers only the paper-style tournament method", () => {
    render(<RunForm />);

    expect(screen.queryByRole("radio", { name: /Simple/ })).not.toBeInTheDocument();
    expect(screen.getByText("Tournament ranking")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Multi-turn scientific debates, Elo ranking, and iterative refinement. Runtime varies by research goal.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Tournament · debates up to 5 turns · limits: 200 model calls, 60 min, 10 rounds",
      ),
    ).toBeInTheDocument();
  });

  it("shows the API's stable error body when the start is rejected", async () => {
    startRunMock.mockRejectedValue(
      new ApiFailure("the goal is blocked", "permission", 403),
    );
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    await user.click(screen.getByRole("button", { name: "Start research" }));

    expect(await screen.findByText("the goal is blocked")).toBeInTheDocument();
    expect(screen.getByText("kind: permission")).toBeInTheDocument();
    expect(pushMock).not.toHaveBeenCalled();
  });

  it("tells the user to start the API when it is unreachable", async () => {
    startRunMock.mockRejectedValue(
      new ApiFailure("fetch failed", "unreachable", null),
    );
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    await user.click(screen.getByRole("button", { name: "Start research" }));

    expect(
      await screen.findByText(/Start it with: uruk serve/),
    ).toBeInTheDocument();
  });
});
