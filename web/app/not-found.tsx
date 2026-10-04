import Link from "next/link";

export default function NotFound() {
  return (
    <div className="canvas canvas-narrow">
      <div className="empty-state">
        <h1>Page not found</h1>
        <p>Nothing lives at this address.</p>
        <Link href="/" className="btn btn-outline">
          Back to URUK
        </Link>
      </div>
    </div>
  );
}
