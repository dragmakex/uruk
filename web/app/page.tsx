import Link from "next/link";

/**
 * Minimal public landing shell. Static: it states what the local console
 * is and links into it; all live data lives on the app pages.
 */
export default function LandingPage() {
  return (
    <div className="canvas landing">
      <h1>URUK</h1>
      <p className="lede">
        An autonomous research engine. Specify a research goal, watch a
        supervised team of agents draft, review, and rank hypotheses in a
        live tournament, and read the evidence-backed report it exports.
      </p>
      <div className="landing-actions">
        <Link href="/runs/new" className="btn landing-action">
          Start research
        </Link>
        <Link href="/runs" className="btn btn-outline landing-action">
          View runs
        </Link>
      </div>
    </div>
  );
}
