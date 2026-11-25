import Image from "next/image";
import Link from "next/link";
import { Button } from "./Button";
import { ThemeToggle } from "./ThemeToggle";

/** The persistent URUKAI top bar and its primary navigation. */
export function SiteNav() {
  return (
    <header className="site-nav">
      <Link href="/" className="wordmark" aria-label="URUKAI home">
        <Image
          src="/favicon.svg"
          alt=""
          className="wordmark-mark"
          width={28}
          height={28}
          unoptimized
        />
        <span className="wordmark-name">URUKAI</span>
      </Link>
      <nav className="nav-links" aria-label="Primary">
        <Link href="/runs">Runs</Link>
        <Link href="/library">Library</Link>
        <Button href="/runs/new" size="xs" className="nav-start">
          Start research
        </Button>
        <ThemeToggle />
      </nav>
    </header>
  );
}
