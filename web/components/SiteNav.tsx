import Link from "next/link";
import { ThemeToggle } from "./ThemeToggle";

/** The URUK top bar: wordmark left, section links and theme toggle right. */
export function SiteNav() {
  return (
    <header className="site-nav">
      <Link href="/" className="wordmark">
        URUK
      </Link>
      <nav className="nav-links" aria-label="Primary">
        <Link href="/runs">Runs</Link>
        <Link href="/library">Library</Link>
        <ThemeToggle />
      </nav>
    </header>
  );
}
