import type { RunSnapshot } from "@/lib/types";
import { formatDelta } from "@/lib/format";
import "./ranked-overview.css";

/** A bounded view of the service's ranking, never a second client-side sort. */
export function RankedOverview({ snapshot }: { snapshot: RunSnapshot }) {
  const items = snapshot.items.filter((item) =>
    (item.kind === "hypothesis" || item.kind === "proposal") && item.disposition === "active",
  ).slice(0, 5);
  const baseline = snapshot.ranking_baseline;
  return (
    <section className="ranked-overview" aria-label="Top five candidates">
      <h2>Leading candidates</h2>
      <p className="micro">Elo ranks comparisons, not scientific truth. {baseline
        ? `Δ since round ${baseline.iteration}.`
        : "Round comparison unavailable until a prior rating checkpoint exists."}</p>
      {items.length === 0 ? <p className="micro">No candidates yet. Generation has not recorded a proposal.</p> : (
        <ol className="ranked-overview-list">
          {items.map((item, index) => {
            const before = baseline?.ratings[item.id];
            const delta = before !== undefined && item.rating !== null ? item.rating - before : null;
            return (
              <li key={item.id}>
                <span className="micro-ink">{index === 0 ? "Leader" : `#${index + 1}`} {item.rating === null
                  ? "unrated"
                  : item.rating.toLocaleString("en-US", { maximumFractionDigits: 0 })}</span>
                <h3>{item.title}</h3>
                <span className="micro">{delta === null ? "Δ unavailable" : `Δ ${formatDelta(delta)}`}{item.stale_rating ? " (stale)" : ""}</span>
                <span className="overview-assessment" title={item.assessment === "supported"
                  ? "The recorded assessment passed the claim-relevant evidence gate; Elo cannot promote a claim."
                  : "This is the recorded scientific assessment, independent of Elo."}>
                  {item.assessment === "supported" ? "supported / evidence-backed" : item.assessment}
                </span>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}
