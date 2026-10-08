"use client";

import { useRouter } from "next/navigation";
import { useId, useState } from "react";
import {
  ApiFailure,
  deleteUpload,
  previewRun,
  startRun,
  uploadFile,
} from "@/lib/api";
import type { RunPreview, StartRunInput, UploadView } from "@/lib/types";

const MAX_GOAL_CHARS = 8000;

/** Budget bounds, mirrored from `src/web/start.rs` so refusals are instant. */
const BUDGETS = {
  modelCalls: { label: "Model calls", min: 1, max: 100000, fallback: 200 },
  seconds: { label: "Time limit (seconds)", min: 1, max: 604800, fallback: 3600 },
  iterations: { label: "Iterations", min: 1, max: 1000, fallback: 10 },
  acquisitions: { label: "Source acquisitions", min: 1, max: 100, fallback: 8 },
  debateTurns: { label: "Debate turns", min: 2, max: 10, fallback: 5 },
} as const;

/** The federated literature connectors, in the API's canonical order. */
const CONNECTORS = [
  { value: "openalex", label: "OpenAlex" },
  { value: "crossref", label: "Crossref" },
  { value: "arxiv", label: "arXiv" },
] as const;

/** File types the server's extractors accept (`src/web/uploads.rs`). */
const UPLOAD_ACCEPT = ".txt,.md,.csv,.tsv,.json,.jsonl,.pdf,.html,.htm,.rs,.py,.r";

interface AttachedUpload {
  upload: UploadView;
  included: boolean;
}

/** Split a one-entry-per-line textarea into trimmed, non-blank entries. */
function lines(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

/** Parse a budget field or explain its bounds. */
function parseBudget(
  raw: string,
  spec: { label: string; min: number; max: number },
): number | string {
  const value = Number(raw.trim());
  if (!Number.isInteger(value) || value < spec.min || value > spec.max) {
    return `${spec.label} must be a whole number between ${spec.min} and ${spec.max}.`;
  }
  return value;
}

function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} kB`;
  return `${bytes} B`;
}

/**
 * The run specification form: the full safe configuration `POST
 * /api/runs` accepts — goal, mode, ranking, rubric guidance, budgets,
 * source URLs, owner-bound file uploads, and the explicit network and
 * literature-search grants. Nothing here can name a server path or grant
 * execution; those capabilities do not exist on the web surface.
 *
 * Both comparison methods feed the Elo tournament. The API retains its
 * legacy values (`simple` and `tournament`), while the interface uses the
 * paper-based names Direct and Multi-turn.
 */
export function RunForm() {
  const router = useRouter();
  const ids = {
    goal: useId(),
    profile: useId(),
    deliverables: useId(),
    preferences: useId(),
    attributes: useId(),
    constraints: useId(),
    modelCalls: useId(),
    seconds: useId(),
    iterations: useId(),
    acquisitions: useId(),
    debateTurns: useId(),
    urls: useId(),
    attach: useId(),
    network: useId(),
    search: useId(),
  };

  const [goal, setGoal] = useState("");
  const [mode, setMode] = useState<"task" | "campaign">("campaign");
  const [ranking, setRanking] = useState<"simple" | "tournament">("simple");
  const [debateTurns, setDebateTurns] = useState(String(BUDGETS.debateTurns.fallback));
  const [profile, setProfile] = useState("");
  const [deliverables, setDeliverables] = useState("");
  const [preferences, setPreferences] = useState("");
  const [attributes, setAttributes] = useState("");
  const [constraints, setConstraints] = useState("");
  const [modelCalls, setModelCalls] = useState(String(BUDGETS.modelCalls.fallback));
  const [seconds, setSeconds] = useState(String(BUDGETS.seconds.fallback));
  const [iterations, setIterations] = useState(String(BUDGETS.iterations.fallback));
  const [acquisitions, setAcquisitions] = useState(
    String(BUDGETS.acquisitions.fallback),
  );
  const [urls, setUrls] = useState("");
  const [uploads, setUploads] = useState<AttachedUpload[]>([]);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [allowNetwork, setAllowNetwork] = useState(false);
  const [search, setSearch] = useState(false);
  const [connectors, setConnectors] = useState<string[]>(
    CONNECTORS.map((c) => c.value),
  );
  const [busy, setBusy] = useState<"start" | "preview" | null>(null);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const [preview, setPreview] = useState<RunPreview | null>(null);
  const [apiError, setApiError] = useState<{ error: string; kind: string } | null>(
    null,
  );

  /**
   * Validate the form into a start payload, or report the first problem.
   * Only defaults the server would apply anyway are omitted, so what the
   * user sees is what is sent.
   */
  function buildInput(): StartRunInput | null {
    function refuse(message: string): null {
      setFieldError(message);
      return null;
    }

    const question = goal.trim();
    if (question.length === 0) return refuse("State the research goal first.");
    if (question.length > MAX_GOAL_CHARS) {
      return refuse(`The goal is limited to ${MAX_GOAL_CHARS} characters.`);
    }

    const budget = {
      modelCalls: parseBudget(modelCalls, BUDGETS.modelCalls),
      seconds: parseBudget(seconds, BUDGETS.seconds),
      iterations: parseBudget(iterations, BUDGETS.iterations),
      acquisitions: parseBudget(acquisitions, BUDGETS.acquisitions),
    };
    for (const parsed of Object.values(budget)) {
      if (typeof parsed === "string") return refuse(parsed);
    }

    const input: StartRunInput = {
      goal: question,
      mode,
      ranking,
      max_model_calls: budget.modelCalls as number,
      max_seconds: budget.seconds as number,
      max_iterations: budget.iterations as number,
      max_acquisitions: budget.acquisitions as number,
    };

    if (ranking === "tournament") {
      const turns = parseBudget(debateTurns, BUDGETS.debateTurns);
      if (typeof turns === "string") return refuse(turns);
      input.max_debate_turns = turns;
    }

    const trimmedProfile = profile.trim();
    if (trimmedProfile.length > 0) input.profile = trimmedProfile;

    for (const [key, text] of [
      ["deliverables", deliverables],
      ["preferences", preferences],
      ["attributes", attributes],
      ["constraints", constraints],
    ] as const) {
      const entries = lines(text);
      if (entries.length > 0) input[key] = entries;
    }

    const inputUrls = lines(urls);
    if (inputUrls.some((u) => !/^https?:\/\//.test(u))) {
      return refuse(
        "Source URLs accept http(s) URLs only. Attach files instead of naming paths.",
      );
    }
    if (inputUrls.length > 0) input.input_urls = inputUrls;

    const uploadIds = uploads.filter((u) => u.included).map((u) => u.upload.id);
    if (uploadIds.length > 0) input.upload_ids = uploadIds;

    if (allowNetwork) input.allow_network = true;
    if (allowNetwork && search) {
      input.search = true;
      if (connectors.length === 0) {
        return refuse("Pick at least one connector or turn literature search off.");
      }
      if (connectors.length < CONNECTORS.length) {
        input.search_connectors = CONNECTORS.map((c) => c.value).filter((c) =>
          connectors.includes(c),
        );
      }
    }

    setFieldError(null);
    return input;
  }

  async function onSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setApiError(null);
    setPreview(null);
    const input = buildInput();
    if (input === null) return;
    setBusy("start");
    try {
      const runId = await startRun(input);
      router.push(`/runs/${encodeURIComponent(runId)}`);
    } catch (e) {
      if (e instanceof ApiFailure) {
        setApiError({ error: e.message, kind: e.kind });
      } else {
        setApiError({ error: "unexpected client error", kind: "client" });
      }
      setBusy(null);
    }
  }

  async function onPreview() {
    setApiError(null);
    setPreview(null);
    const input = buildInput();
    if (input === null) return;
    setBusy("preview");
    try {
      setPreview(await previewRun(input));
    } catch (e) {
      if (e instanceof ApiFailure) {
        setApiError({ error: e.message, kind: e.kind });
      } else {
        setApiError({ error: "unexpected client error", kind: "client" });
      }
    } finally {
      setBusy(null);
    }
  }

  async function onAttach(event: React.ChangeEvent<HTMLInputElement>) {
    const files = Array.from(event.target.files ?? []);
    event.target.value = "";
    for (const file of files) {
      try {
        const upload = await uploadFile(file);
        setUploads((prev) => [...prev, { upload, included: true }]);
        setUploadError(null);
      } catch (e) {
        setUploadError(
          e instanceof ApiFailure ? e.message : "the upload failed unexpectedly",
        );
      }
    }
  }

  async function onDeleteUpload(upload: UploadView) {
    try {
      await deleteUpload(upload.id);
      setUploads((prev) => prev.filter((u) => u.upload.id !== upload.id));
      setUploadError(null);
    } catch (e) {
      setUploadError(
        e instanceof ApiFailure ? e.message : "the delete failed unexpectedly",
      );
    }
  }

  function toggleInclude(id: string) {
    setUploads((prev) =>
      prev.map((u) =>
        u.upload.id === id ? { ...u, included: !u.included } : u,
      ),
    );
  }

  function toggleConnector(value: string) {
    setConnectors((prev) =>
      prev.includes(value) ? prev.filter((c) => c !== value) : [...prev, value],
    );
  }

  function setNetwork(granted: boolean) {
    setAllowNetwork(granted);
    if (!granted) setSearch(false);
  }

  const budgetFields = [
    {
      id: ids.modelCalls,
      spec: BUDGETS.modelCalls,
      value: modelCalls,
      set: setModelCalls,
    },
    { id: ids.seconds, spec: BUDGETS.seconds, value: seconds, set: setSeconds },
    {
      id: ids.iterations,
      spec: BUDGETS.iterations,
      value: iterations,
      set: setIterations,
    },
    {
      id: ids.acquisitions,
      spec: BUDGETS.acquisitions,
      value: acquisitions,
      set: setAcquisitions,
    },
  ];

  const guidanceFields = [
    {
      id: ids.deliverables,
      label: "Deliverables",
      value: deliverables,
      set: setDeliverables,
      hint: "What the run must produce. One per line.",
    },
    {
      id: ids.preferences,
      label: "Preferences",
      value: preferences,
      set: setPreferences,
      hint: "Soft rubric: what better answers look like.",
    },
    {
      id: ids.attributes,
      label: "Attributes",
      value: attributes,
      set: setAttributes,
      hint: "Dimensions reviews must score.",
    },
    {
      id: ids.constraints,
      label: "Constraints",
      value: constraints,
      set: setConstraints,
      hint: "Hard rules; violations fail review.",
    },
  ];

  return (
    <form className="panel" onSubmit={onSubmit} noValidate>
      <div className="panel-head">
        <h1 className="panel-title">Run specification</h1>
        <span className="micro">new</span>
      </div>

      <div className="panel-section">
        <label className="section-label" htmlFor={ids.goal}>
          Research goal
        </label>
        <textarea
          id={ids.goal}
          className="goal-input"
          name="goal"
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
          placeholder="Question, system, constraints, what would count as an answer."
          maxLength={MAX_GOAL_CHARS + 1}
          aria-invalid={fieldError !== null && goal.trim().length === 0}
        />
      </div>

      <div className="panel-section">
        <fieldset className="bare-fieldset">
          <legend className="section-label">Mode</legend>
          <div className="choice-grid" role="presentation">
            <label className={`choice${mode === "campaign" ? " is-selected" : ""}`}>
              <input
                type="radio"
                name="mode"
                value="campaign"
                checked={mode === "campaign"}
                onChange={() => setMode("campaign")}
              />
              <span className="choice-mark" aria-hidden="true" />
              <span className="choice-name">Campaign</span>
              <span className="choice-desc">
                Iterative discovery loop: generate, review, rank, refine until the
                budget or the goal is met.
              </span>
            </label>
            <label className={`choice${mode === "task" ? " is-selected" : ""}`}>
              <input
                type="radio"
                name="mode"
                value="task"
                checked={mode === "task"}
                onChange={() => setMode("task")}
              />
              <span className="choice-mark" aria-hidden="true" />
              <span className="choice-name">Task</span>
              <span className="choice-desc">
                One pass with only the necessary roles. For bounded questions.
              </span>
            </label>
          </div>
        </fieldset>
      </div>

      <div className="panel-section">
        <fieldset className="bare-fieldset">
          <legend className="section-label">Ranking</legend>
          <div className="choice-grid" role="presentation">
            <label className={`choice${ranking === "simple" ? " is-selected" : ""}`}>
              <input
                type="radio"
                name="ranking"
                value="simple"
                checked={ranking === "simple"}
                onChange={() => setRanking("simple")}
              />
              <span className="choice-mark" aria-hidden="true" />
              <span className="choice-name">Direct</span>
              <span className="choice-desc">
                Single-turn pairwise comparison with an Elo update. Lower compute;
                runtime varies by research goal.
              </span>
            </label>
            <label
              className={`choice${ranking === "tournament" ? " is-selected" : ""}`}
            >
              <input
                type="radio"
                name="ranking"
                value="tournament"
                checked={ranking === "tournament"}
                onChange={() => setRanking("tournament")}
              />
              <span className="choice-mark" aria-hidden="true" />
              <span className="choice-name">Multi-turn</span>
              <span className="choice-desc">
                Pairwise scientific debate up to the turn cap, followed by an Elo
                update. More compute-intensive; runtime varies by research goal.
              </span>
            </label>
          </div>
          {ranking === "tournament" && (
            <div className="num-field debate-turns">
              <label className="micro" htmlFor={ids.debateTurns}>
                Debate turns
              </label>
              <input
                id={ids.debateTurns}
                className="num-input"
                type="number"
                min={BUDGETS.debateTurns.min}
                max={BUDGETS.debateTurns.max}
                value={debateTurns}
                onChange={(e) => setDebateTurns(e.target.value)}
              />
            </div>
          )}
        </fieldset>
      </div>

      <div className="panel-section">
        <span className="section-label">Guidance</span>
        <div className="field-stack">
          <div className="num-field">
            <label className="micro" htmlFor={ids.profile}>
              Domain profile
            </label>
            <input
              id={ids.profile}
              className="text-input"
              type="text"
              value={profile}
              onChange={(e) => setProfile(e.target.value)}
              placeholder="e.g. materials-science"
            />
          </div>
          {guidanceFields.map((field) => (
            <div className="num-field" key={field.id}>
              <label className="micro" htmlFor={field.id}>
                {field.label}
              </label>
              <textarea
                id={field.id}
                className="list-input"
                value={field.value}
                onChange={(e) => field.set(e.target.value)}
                placeholder={field.hint}
                rows={2}
              />
            </div>
          ))}
        </div>
      </div>

      <div className="panel-section">
        <span className="section-label">Budget</span>
        <div className="num-grid">
          {budgetFields.map((field) => (
            <div className="num-field" key={field.id}>
              <label className="micro" htmlFor={field.id}>
                {field.spec.label}
              </label>
              <input
                id={field.id}
                className="num-input"
                type="number"
                min={field.spec.min}
                max={field.spec.max}
                value={field.value}
                onChange={(e) => field.set(e.target.value)}
              />
            </div>
          ))}
        </div>
      </div>

      <div className="panel-section">
        <span className="section-label">Sources</span>
        <div className="field-stack">
          <div className="num-field">
            <label className="micro" htmlFor={ids.urls}>
              Source URLs
            </label>
            <textarea
              id={ids.urls}
              className="list-input"
              value={urls}
              onChange={(e) => setUrls(e.target.value)}
              placeholder="https:// links to read, one per line. Needs the network grant."
              rows={2}
            />
          </div>
          <div className="num-field">
            <label className="micro" htmlFor={ids.attach}>
              Attach a file
            </label>
            <input
              id={ids.attach}
              type="file"
              accept={UPLOAD_ACCEPT}
              multiple
              onChange={onAttach}
            />
            <p className="micro">Text-extractable files up to 16 MB each.</p>
          </div>
          {uploads.length > 0 && (
            <ul className="upload-list">
              {uploads.map(({ upload, included }) => (
                <li className="upload-row" key={upload.id}>
                  <input
                    type="checkbox"
                    checked={included}
                    onChange={() => toggleInclude(upload.id)}
                    aria-label={`Include ${upload.file_name}`}
                  />
                  <span className="upload-name">{upload.file_name}</span>
                  <span className="micro">{formatSize(upload.size_bytes)}</span>
                  <button
                    type="button"
                    className="btn-outline btn upload-delete"
                    onClick={() => onDeleteUpload(upload)}
                    aria-label={`Delete ${upload.file_name}`}
                  >
                    Delete
                  </button>
                </li>
              ))}
            </ul>
          )}
          {uploadError !== null && (
            <p className="field-error" role="alert">
              {uploadError}
            </p>
          )}
        </div>
      </div>

      <div className="panel-section">
        <span className="section-label">Network</span>
        <div className="field-stack">
          <div className="check-line">
            <input
              id={ids.network}
              type="checkbox"
              checked={allowNetwork}
              onChange={(e) => setNetwork(e.target.checked)}
            />
            <span>
              <label className="check-name" htmlFor={ids.network}>
                Allow network retrieval
              </label>
              <span className="check-desc">
                Permit fetching the source URLs. Off by default: a run without
                this grant reads only attached files.
              </span>
            </span>
          </div>
          <div className="check-line">
            <input
              id={ids.search}
              type="checkbox"
              checked={search}
              disabled={!allowNetwork}
              onChange={(e) => setSearch(e.target.checked)}
            />
            <span>
              <label className="check-name" htmlFor={ids.search}>
                Literature search
              </label>
              <span className="check-desc">
                Query open scholarly indexes. Sends goal-derived search terms to
                their operators, so it needs the network grant.
              </span>
            </span>
          </div>
          {allowNetwork && search && (
            <div className="conn-row">
              {CONNECTORS.map((connector) => (
                <label className="check-line" key={connector.value}>
                  <input
                    type="checkbox"
                    checked={connectors.includes(connector.value)}
                    onChange={() => toggleConnector(connector.value)}
                  />
                  <span className="check-name">{connector.label}</span>
                </label>
              ))}
            </div>
          )}
        </div>
      </div>

      {fieldError !== null && (
        <div className="panel-section">
          <p className="field-error" role="alert">
            {fieldError}
          </p>
        </div>
      )}

      <div className="panel-foot">
        <button
          type="button"
          className="btn btn-outline"
          onClick={onPreview}
          disabled={busy !== null}
        >
          {busy === "preview" ? "Previewing" : "Preview plan"}
        </button>
        <button type="submit" className="btn" disabled={busy !== null}>
          {busy === "start" ? "Starting run" : "Start research"}
        </button>
      </div>

      {preview !== null && (
        <div className="panel-section preview-box" aria-label="Plan preview">
          <span className="section-label">Plan preview</span>
          <dl className="preview-grid">
            <dt className="micro">Roles</dt>
            <dd>{preview.plan.roles.join(", ")}</dd>
            <dt className="micro">Methods</dt>
            <dd>{preview.plan.methods.join("; ")}</dd>
            <dt className="micro">Rationale</dt>
            <dd>{preview.plan.rationale}</dd>
            <dt className="micro">Inputs</dt>
            <dd>{preview.inputs.length > 0 ? preview.inputs.join(", ") : "none"}</dd>
            <dt className="micro">Permissions</dt>
            <dd>
              network {preview.permissions.network ? "on" : "off"} · execution
              never
              {preview.permissions.allowed_tools.length > 0 &&
                ` · ${preview.permissions.allowed_tools.join(", ")}`}
            </dd>
            <dt className="micro">Budget</dt>
            <dd>
              {preview.budget.max_model_calls} calls ·{" "}
              {preview.budget.max_seconds} s · {preview.budget.max_iterations}{" "}
              iterations · {preview.budget.max_debate_turns} debate turns ·{" "}
              {preview.budget.max_acquisitions} acquisitions
            </dd>
          </dl>
          <p className="micro">Nothing was created. Start research to run this plan.</p>
        </div>
      )}

      {apiError !== null && (
        <div className="panel-section" role="alert">
          <p className="field-error">
            {apiError.kind === "unreachable"
              ? "The uruk API is unreachable. Start it with: uruk serve"
              : apiError.error}
          </p>
          <p className="error-kind">kind: {apiError.kind}</p>
        </div>
      )}
    </form>
  );
}
