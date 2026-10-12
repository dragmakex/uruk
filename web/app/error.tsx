"use client";

import { Button } from "@/components/Button";
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
        <Button variant="outline" size="sm" onClick={reset}>
          Try again
        </Button>
      </div>
    </div>
  );
}
