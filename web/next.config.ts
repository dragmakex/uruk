import type { NextConfig } from "next";

/**
 * Where the Rust API listens. Server-side fetches hit it directly; the
 * browser always talks to the same-origin `/api` path, which `next dev`
 * rewrites here. In production a reverse proxy routes `/api` to the Rust
 * service before requests ever reach Next (docs/WEB.md), so this rewrite is
 * only a fallback for running `next start` without a proxy.
 */
const apiOrigin = process.env.URUK_API_URL ?? "http://127.0.0.1:7913";

const nextConfig: NextConfig = {
  async rewrites() {
    return [
      {
        source: "/api/:path*",
        destination: `${apiOrigin}/api/:path*`,
      },
    ];
  },
};

export default nextConfig;
