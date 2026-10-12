import { Button } from "@/components/Button";

export default function NotFound() {
  return (
    <div className="canvas canvas-narrow">
      <div className="empty-state">
        <h1>Page not found</h1>
        <p>Nothing lives at this address.</p>
        <Button href="/" variant="outline" size="sm">
          Back to URUK
        </Button>
      </div>
    </div>
  );
}
