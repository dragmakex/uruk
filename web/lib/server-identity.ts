/**
 * Server-side access to the browser's anonymous identity.
 *
 * Server Components fetch the Rust API directly (not through the public
 * origin), so the incoming request's cookies must be forwarded by hand
 * for the API to answer as the right anonymous workspace.
 *
 * This module imports `next/headers` and therefore must only be imported
 * from Server Components (the pages); importing it from a `"use client"`
 * module is a build error by design. Client code never needs it: the
 * cookie is HttpOnly and rides same-origin `/api` requests automatically.
 *
 * Server Components can read cookies but never set them: when a visitor
 * has no identity yet, the first-paint fetch runs as a throwaway empty
 * identity and the browser's own first `/api` request mints the durable
 * cookie (Next docs: cookies are read-only during Server Component
 * rendering).
 */
import { cookies } from "next/headers";

/**
 * The incoming request's cookie header, verbatim, or `undefined` when the
 * visitor has no cookies yet. Pass it to the `lib/api.ts` read functions.
 */
export async function identityCookieHeader(): Promise<string | undefined> {
  const jar = await cookies();
  const header = jar.toString();
  return header === "" ? undefined : header;
}
