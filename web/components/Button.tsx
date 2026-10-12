import Link from "next/link";
import type { AnchorHTMLAttributes, ButtonHTMLAttributes, ReactNode } from "react";

/** Colour treatment. A variant never changes a control's box. */
export type ButtonVariant = "primary" | "outline";

/**
 * Scale token. Geometry (padding, type size, and for `cta` a fixed height
 * and minimum width) lives only here, so two controls of one size are the
 * same size whatever their variant.
 */
export type ButtonSize = "xs" | "sm" | "md" | "cta";

interface Appearance {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Layout hooks only (positioning within a parent); never geometry. */
  className?: string;
  children: ReactNode;
}

/** Props the component owns; call sites cannot override them. */
type Owned = "className" | "style" | "children";

/** A native `<button>`; defaults to `type="button"` so forms are opted into. */
export type NativeButtonProps = Appearance &
  Omit<ButtonHTMLAttributes<HTMLButtonElement>, Owned>;

/**
 * A link styled as a button. App routes navigate through the client router;
 * API routes, absolute URLs, and anything with `download` are plain anchors
 * so the router never intercepts a document the server must serve.
 */
export type LinkButtonProps = Appearance &
  Omit<AnchorHTMLAttributes<HTMLAnchorElement>, Owned | "href"> & { href: string };

export type ButtonProps = NativeButtonProps | LinkButtonProps;

/** The class list for a given appearance; exported for tests and audits. */
export function buttonClassName(
  variant: ButtonVariant,
  size: ButtonSize,
  className?: string,
): string {
  return ["btn", `btn--${variant}`, `btn--${size}`, className]
    .filter((token): token is string => typeof token === "string" && token !== "")
    .join(" ");
}

/** Whether `href` is a page of this app rather than a served document. */
function isAppRoute(href: string, download: unknown): boolean {
  return download === undefined && href.startsWith("/") && !href.startsWith("/api/");
}

/**
 * The one button-like control: a native button, an internal navigation
 * link, or a download/API anchor, chosen by the props given. Every
 * button-like control in the web UI renders through here so variants,
 * sizes, focus, and disabled styling stay a single contract.
 */
export function Button(props: ButtonProps) {
  if ("href" in props) {
    const { variant = "primary", size = "md", className, children, href, ...anchor } = props;
    const classes = buttonClassName(variant, size, className);
    if (isAppRoute(href, anchor.download)) {
      return (
        <Link href={href} className={classes} {...anchor}>
          {children}
        </Link>
      );
    }
    return (
      <a href={href} className={classes} {...anchor}>
        {children}
      </a>
    );
  }
  const { variant = "primary", size = "md", className, children, type = "button", ...button } =
    props;
  return (
    <button type={type} className={buttonClassName(variant, size, className)} {...button}>
      {children}
    </button>
  );
}

interface ButtonGroupProps {
  /**
   * Size every child to the same width by layout: equal grid columns, so
   * siblings stay exactly equal however long their labels are. Pair with
   * `size="cta"`, whose fixed height makes the boxes identical.
   */
  equal?: boolean;
  className?: string;
  children: ReactNode;
}

/** A row of buttons; `equal` makes them exactly equal boxes. */
export function ButtonGroup({ equal = false, className, children }: ButtonGroupProps) {
  const classes = ["btn-group", equal ? "btn-group--equal" : undefined, className]
    .filter((token): token is string => typeof token === "string" && token !== "")
    .join(" ");
  return <div className={classes}>{children}</div>;
}
