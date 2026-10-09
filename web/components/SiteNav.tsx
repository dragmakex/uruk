import Image from "next/image";
import Link from "next/link";
import { ThemeToggle } from "./ThemeToggle";

/** The URUK top bar: wordmark left, section links and theme toggle right. */
export function SiteNav() {
  return (
    <header className="site-nav">
      <Link href="/" className="wordmark">
        <Image
          src="/favicon.svg"
          alt=""
          className="wordmark-mark"
          width={28}
          height={28}
          unoptimized
        />
        <span>URUK</span>
      </Link>
      <nav className="nav-links" aria-label="Primary">
        <Link href="/runs">Runs</Link>
        <Link href="/library">Library</Link>
        <ThemeToggle />
      </nav>
    </header>
  );
}
