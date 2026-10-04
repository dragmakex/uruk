/** Shared backend-unavailable state for server-rendered pages. */
export function ApiDown({ detail }: { detail: string }) {
  return (
    <div className="error-box" role="alert">
      <h2>The uruk API is unreachable</h2>
      <p>
        Start it locally with <code>uruk serve</code> (default
        127.0.0.1:7913), then reload this page.
      </p>
      <p className="error-kind">{detail}</p>
    </div>
  );
}
