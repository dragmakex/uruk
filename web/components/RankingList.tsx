import type { ItemCard } from "@/lib/types";
import { formatElo } from "@/lib/format";

/**
 * Ranking cards: candidates best-first as the API orders them, showing the
 * recorded rating, match count, assessment, and review facts. Unrated
 * candidates say so instead of showing an invented number.
 */
export function RankingList({ items }: { items: ItemCard[] }) {
  if (items.length === 0) {
    return (
      <p className="micro">
        No candidates yet. Items appear here once Generation drafts them.
      </p>
    );
  }

  return (
    <ol style={{ listStyle: "none", margin: 0, padding: 0 }}>
      {items.map((item, index) => (
        <li key={item.id} className="item-row">
          <div className="item-main">
            <span className="item-title">
              {index + 1}. {item.title}
            </span>
            <p className="item-meta micro">
              {item.kind}, {item.assessment}
              {item.disposition !== "active" ? `, ${item.disposition}` : ""}
              {item.review_count > 0
                ? `, ${item.review_count} review${item.review_count === 1 ? "" : "s"}`
                : ", unreviewed"}
            </p>
          </div>
          <div className="item-rating">
            {item.rating !== null ? (
              <>
                <span className="micro-ink">
                  elo {formatElo(item.rating)}, {item.matches_played}{" "}
                  {item.matches_played === 1 ? "match" : "matches"}
                </span>
                {item.stale_rating && (
                  <span className="micro stale-flag"> STALE</span>
                )}
              </>
            ) : (
              <span className="micro">unrated</span>
            )}
          </div>
        </li>
      ))}
    </ol>
  );
}
