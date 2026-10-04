"use client";

/** Last-resort render error boundary; API failures are handled per page. */
export default function GlobalError({
  error,
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  return (
    <div className="canvas canvas-narrow">
      <div className="error-box" role="alert">
        <h1>Something broke while rendering</h1>
        <p>{error.message || "Unknown rendering error."}</p>
        <button type="button" className="btn btn-outline" onClick={reset}>
          Try again
        </button>
      </div>
    </div>
  );
}
