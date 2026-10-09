import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ApiFailure } from "@/lib/api";
import type { RunPreview, UploadView } from "@/lib/types";
import { RunForm } from "./RunForm";

const pushMock = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: pushMock }),
}));

const startRunMock = vi.fn();
const previewRunMock = vi.fn();
const uploadFileMock = vi.fn();
const deleteUploadMock = vi.fn();
const fetchUploadsMock = vi.fn();
vi.mock("@/lib/api", async () => {
  const actual = await vi.importActual<typeof import("@/lib/api")>("@/lib/api");
  return {
    ...actual,
    startRun: (input: unknown) => startRunMock(input),
    previewRun: (input: unknown) => previewRunMock(input),
    uploadFile: (file: unknown) => uploadFileMock(file),
    deleteUpload: (id: unknown) => deleteUploadMock(id),
    fetchUploads: () => fetchUploadsMock(),
  };
});

beforeEach(() => {
  pushMock.mockReset();
  startRunMock.mockReset();
  previewRunMock.mockReset();
  uploadFileMock.mockReset();
  deleteUploadMock.mockReset();
  fetchUploadsMock.mockReset();
  fetchUploadsMock.mockResolvedValue([]);
});

/** The payload an untouched form submits: visible defaults, no grants. */
const DEFAULT_PAYLOAD = {
  mode: "campaign",
  ranking: "simple",
  max_model_calls: 200,
  max_seconds: 3600,
  max_iterations: 10,
  max_acquisitions: 8,
};

const UPLOAD: UploadView = {
  id: "upl_1",
  file_name: "notes.txt",
  size_bytes: 9,
  content_hash: "abc123",
  created_at: "2026-10-08T10:00:00Z",
};

const PREVIEW: RunPreview = {
  plan: {
    roles: ["generation", "reflection", "ranking"],
    methods: ["grounded hypothesis generation"],
    rationale: "campaign mode runs the full loop",
  },
  inputs: [],
  permissions: { network: false, execute: false, allowed_tools: [] },
  budget: {
    max_model_calls: 200,
    max_seconds: 3600,
    max_iterations: 10,
    max_debate_turns: 1,
    max_acquisitions: 8,
  },
};

describe("RunForm", () => {
  it("restores retained uploads after a reload", async () => {
    fetchUploadsMock.mockResolvedValue([UPLOAD]);
    render(<RunForm />);
    expect(await screen.findByText("notes.txt")).toBeInTheDocument();
    expect(screen.getByLabelText("Include notes.txt")).toBeChecked();
  });

  it("refuses to submit an empty goal and says why", async () => {
    const user = userEvent.setup();
    render(<RunForm />);
    await user.click(screen.getByRole("button", { name: "Start research" }));
    expect(screen.getByRole("alert")).toHaveTextContent(
      "State the research goal first.",
    );
    expect(screen.getByLabelText("Research goal")).toHaveAccessibleDescription(
      "State the research goal first.",
    );
    expect(startRunMock).not.toHaveBeenCalled();
  });

  it("posts the visible defaults and navigates to the run", async () => {
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
      ...DEFAULT_PAYLOAD,
    });
    expect(pushMock).toHaveBeenCalledWith("/runs/run_new1");
  });

  it("labels the paper's comparison methods Direct and Multi-turn", () => {
    render(<RunForm />);

    expect(screen.getByRole("radio", { name: /^Direct/ })).toBeChecked();
    expect(screen.getByRole("radio", { name: /^Multi-turn/ })).not.toBeChecked();
  });

  it("keeps only the two actions in the form footer", () => {
    render(<RunForm />);

    const footer = screen
      .getByRole("button", { name: "Start research" })
      .closest(".panel-foot");
    expect(footer?.children).toHaveLength(2);
    expect(
      screen.getByRole("button", { name: "Preview plan" }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/limits: 200 model calls/)).not.toBeInTheDocument();
  });

  it("selects multi-turn debate and submits its default turn cap", async () => {
    startRunMock.mockResolvedValue("run_new2");
    const user = userEvent.setup();
    render(<RunForm />);

    await user.click(screen.getByRole("radio", { name: /^Multi-turn/ }));

    expect(screen.getByRole("radio", { name: /^Multi-turn/ })).toBeChecked();

    await user.type(screen.getByLabelText("Research goal"), "compare both ideas");
    await user.click(screen.getByRole("button", { name: "Start research" }));
    expect(startRunMock).toHaveBeenCalledWith({
      goal: "compare both ideas",
      ...DEFAULT_PAYLOAD,
      ranking: "tournament",
      max_debate_turns: 5,
    });
  });

  it("offers the debate turn cap only for multi-turn debate", async () => {
    const user = userEvent.setup();
    render(<RunForm />);

    expect(screen.queryByLabelText("Debate turns")).not.toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: /^Multi-turn/ }));
    expect(screen.getByLabelText("Debate turns")).toHaveValue(5);
    await user.click(screen.getByRole("radio", { name: /^Direct/ }));
    expect(screen.queryByLabelText("Debate turns")).not.toBeInTheDocument();
  });

  it("submits the full configuration when every section is filled", async () => {
    startRunMock.mockResolvedValue("run_full");
    uploadFileMock.mockResolvedValue(UPLOAD);
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(
      screen.getByLabelText("Research goal"),
      "compare sorbent regeneration strategies",
    );
    await user.click(screen.getByRole("radio", { name: /^Task/ }));
    await user.click(screen.getByRole("radio", { name: /^Multi-turn/ }));

    const turns = screen.getByLabelText("Debate turns");
    await user.clear(turns);
    await user.type(turns, "7");

    await user.type(screen.getByLabelText("Domain profile"), "materials-science");
    await user.type(
      screen.getByLabelText("Deliverables"),
      "ranked hypotheses\nevidence table",
    );
    await user.type(screen.getByLabelText("Preferences"), "grounded claims");
    await user.type(screen.getByLabelText("Attributes"), "novelty");
    await user.type(
      screen.getByLabelText("Constraints"),
      "open-access sources only",
    );

    const calls = screen.getByLabelText("Model calls");
    await user.clear(calls);
    await user.type(calls, "500");
    const seconds = screen.getByLabelText("Time limit (seconds)");
    await user.clear(seconds);
    await user.type(seconds, "7200");
    const iterations = screen.getByLabelText("Iterations");
    await user.clear(iterations);
    await user.type(iterations, "12");
    const acquisitions = screen.getByLabelText("Source acquisitions");
    await user.clear(acquisitions);
    await user.type(acquisitions, "4");

    await user.type(
      screen.getByLabelText("Source URLs"),
      "https://example.org/a.pdf\nhttps://example.org/b.csv",
    );

    const file = new File(["alpha beta"], "notes.txt", { type: "text/plain" });
    await user.upload(screen.getByLabelText("Attach a file"), file);
    expect(uploadFileMock).toHaveBeenCalledWith(file);
    expect(await screen.findByText("notes.txt")).toBeInTheDocument();

    await user.click(screen.getByLabelText("Allow network retrieval"));
    await user.click(screen.getByLabelText("Literature search"));
    await user.click(screen.getByLabelText("OpenAlex"));
    await user.click(screen.getByLabelText("Crossref"));

    await user.click(screen.getByRole("button", { name: "Start research" }));

    expect(startRunMock).toHaveBeenCalledWith({
      goal: "compare sorbent regeneration strategies",
      mode: "task",
      ranking: "tournament",
      profile: "materials-science",
      deliverables: ["ranked hypotheses", "evidence table"],
      preferences: ["grounded claims"],
      attributes: ["novelty"],
      constraints: ["open-access sources only"],
      max_model_calls: 500,
      max_seconds: 7200,
      max_iterations: 12,
      max_debate_turns: 7,
      max_acquisitions: 4,
      input_urls: ["https://example.org/a.pdf", "https://example.org/b.csv"],
      upload_ids: ["upl_1"],
      allow_network: true,
      search: true,
      search_connectors: ["arxiv"],
    });
    expect(pushMock).toHaveBeenCalledWith("/runs/run_full");
  });

  it("rejects a non-http source line before submitting", async () => {
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    await user.type(screen.getByLabelText("Source URLs"), "ftp://host/file");
    await user.click(screen.getByRole("button", { name: "Start research" }));

    expect(screen.getByRole("alert")).toHaveTextContent(
      /Source URLs accept http\(s\) URLs only/,
    );
    expect(startRunMock).not.toHaveBeenCalled();
  });

  it("rejects an out-of-range budget before submitting", async () => {
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    const calls = screen.getByLabelText("Model calls");
    await user.clear(calls);
    await user.type(calls, "0");
    await user.click(screen.getByRole("button", { name: "Start research" }));

    expect(screen.getByRole("alert")).toHaveTextContent(
      /Model calls must be a whole number between 1 and 100000/,
    );
    expect(
      screen.getByLabelText("Research goal"),
    ).not.toHaveAccessibleDescription();
    expect(startRunMock).not.toHaveBeenCalled();
  });

  it("unlocks literature search only after the network grant", async () => {
    const user = userEvent.setup();
    render(<RunForm />);

    const search = screen.getByLabelText("Literature search");
    expect(search).toBeDisabled();
    expect(screen.queryByLabelText("arXiv")).not.toBeInTheDocument();

    await user.click(screen.getByLabelText("Allow network retrieval"));
    expect(search).toBeEnabled();

    await user.click(search);
    expect(screen.getByLabelText("arXiv")).toBeChecked();
    expect(screen.getByLabelText("OpenAlex")).toBeChecked();
    expect(screen.getByLabelText("Crossref")).toBeChecked();
  });

  it("drops the search grant when the network grant is revoked", async () => {
    startRunMock.mockResolvedValue("run_net");
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    await user.click(screen.getByLabelText("Allow network retrieval"));
    await user.click(screen.getByLabelText("Literature search"));
    await user.click(screen.getByLabelText("Allow network retrieval"));

    expect(screen.getByLabelText("Literature search")).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Start research" }));
    expect(startRunMock).toHaveBeenCalledWith({
      goal: "a goal",
      ...DEFAULT_PAYLOAD,
    });
  });

  it("omits the connector list when every connector stays selected", async () => {
    startRunMock.mockResolvedValue("run_all");
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    await user.click(screen.getByLabelText("Allow network retrieval"));
    await user.click(screen.getByLabelText("Literature search"));
    await user.click(screen.getByRole("button", { name: "Start research" }));

    expect(startRunMock).toHaveBeenCalledWith({
      goal: "a goal",
      ...DEFAULT_PAYLOAD,
      allow_network: true,
      search: true,
    });
  });

  it("refuses to search with no connector selected", async () => {
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    await user.click(screen.getByLabelText("Allow network retrieval"));
    await user.click(screen.getByLabelText("Literature search"));
    await user.click(screen.getByLabelText("OpenAlex"));
    await user.click(screen.getByLabelText("Crossref"));
    await user.click(screen.getByLabelText("arXiv"));
    await user.click(screen.getByRole("button", { name: "Start research" }));

    expect(screen.getByRole("alert")).toHaveTextContent(
      /Pick at least one connector or turn literature search off/,
    );
    expect(startRunMock).not.toHaveBeenCalled();
  });

  it("can exclude an attached file without deleting it", async () => {
    startRunMock.mockResolvedValue("run_excl");
    uploadFileMock.mockResolvedValue(UPLOAD);
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    const file = new File(["alpha beta"], "notes.txt", { type: "text/plain" });
    await user.upload(screen.getByLabelText("Attach a file"), file);

    const include = await screen.findByLabelText("Include notes.txt");
    expect(include).toBeChecked();
    await user.click(include);

    await user.click(screen.getByRole("button", { name: "Start research" }));
    expect(startRunMock).toHaveBeenCalledWith({
      goal: "a goal",
      ...DEFAULT_PAYLOAD,
    });
    expect(deleteUploadMock).not.toHaveBeenCalled();
  });

  it("deletes an attached file from the server and the list", async () => {
    uploadFileMock.mockResolvedValue(UPLOAD);
    deleteUploadMock.mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(<RunForm />);

    const file = new File(["alpha beta"], "notes.txt", { type: "text/plain" });
    await user.upload(screen.getByLabelText("Attach a file"), file);
    await screen.findByText("notes.txt");

    await user.click(screen.getByLabelText("Delete notes.txt"));

    expect(deleteUploadMock).toHaveBeenCalledWith("upl_1");
    expect(screen.queryByText("notes.txt")).not.toBeInTheDocument();
  });

  it("shows the API's refusal when an upload is rejected", async () => {
    uploadFileMock.mockRejectedValue(
      new ApiFailure("this file type is not accepted", "validation", 400),
    );
    // The input's `accept` filter would swallow the .exe before the API
    // could refuse it; bypass it to exercise the server-refusal path.
    const user = userEvent.setup({ applyAccept: false });
    render(<RunForm />);

    const file = new File(["MZ"], "payload.exe");
    await user.upload(screen.getByLabelText("Attach a file"), file);

    expect(
      await screen.findByText(/this file type is not accepted/),
    ).toBeInTheDocument();
  });

  it("aggregates every failure from a multi-file upload", async () => {
    uploadFileMock
      .mockRejectedValueOnce(new ApiFailure("too large", "validation", 400))
      .mockRejectedValueOnce(new ApiFailure("wrong type", "validation", 400));
    const user = userEvent.setup({ applyAccept: false });
    render(<RunForm />);

    await user.upload(screen.getByLabelText("Attach a file"), [
      new File(["a"], "one.bin"),
      new File(["b"], "two.bin"),
    ]);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "one.bin: too large; two.bin: wrong type",
    );
  });

  it("locks run actions while an upload is in progress", async () => {
    let finishUpload: ((upload: UploadView) => void) | undefined;
    uploadFileMock.mockReturnValue(
      new Promise<UploadView>((resolve) => {
        finishUpload = resolve;
      }),
    );
    const user = userEvent.setup();
    render(<RunForm />);

    await user.upload(
      screen.getByLabelText("Attach a file"),
      new File(["alpha"], "notes.txt", { type: "text/plain" }),
    );
    expect(screen.getByLabelText("Attach a file")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Preview plan" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Start research" })).toBeDisabled();

    finishUpload?.(UPLOAD);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Start research" })).toBeEnabled(),
    );
  });

  it("previews the plan without starting a run", async () => {
    previewRunMock.mockResolvedValue(PREVIEW);
    const user = userEvent.setup();
    render(<RunForm />);

    await user.type(screen.getByLabelText("Research goal"), "a goal");
    await user.click(screen.getByRole("button", { name: "Preview plan" }));

    expect(previewRunMock).toHaveBeenCalledWith(
      expect.objectContaining({ goal: "a goal" }),
    );
    expect(
      await screen.findByText("generation, reflection, ranking"),
    ).toBeInTheDocument();
    expect(screen.getByText("campaign mode runs the full loop")).toBeInTheDocument();
    expect(startRunMock).not.toHaveBeenCalled();
    expect(pushMock).not.toHaveBeenCalled();
  });

  it("validates before previewing too", async () => {
    const user = userEvent.setup();
    render(<RunForm />);

    await user.click(screen.getByRole("button", { name: "Preview plan" }));
    expect(screen.getByRole("alert")).toHaveTextContent(
      "State the research goal first.",
    );
    expect(previewRunMock).not.toHaveBeenCalled();
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
