// AMT-34: the board has no authentication, so its only boundary is "requests
// from this machine, made by the board's own page". Three layers enforce it:
//
// 1. server.ts binds BOARD_HOSTNAME (loopback), so other machines can't
//    connect at all.
// 2. Every API request must carry a Host header naming the board itself.
//    DNS rebinding points an attacker's hostname at 127.0.0.1, so the browser
//    sends `Host: attacker.example:1776`; refusing it keeps a hostile page from
//    reading the board even though the TCP connection is local.
// 3. Mutations must not come from another origin. A page on any site can POST
//    a no-preflight `text/plain` body to localhost; browsers stamp such
//    requests with Origin (and Sec-Fetch-Site), so refuse any that name
//    somewhere else. Requests with neither header come from non-browser local
//    processes, which could open the database directly anyway.
//
// Pure (request in, verdict out) so the rules are unit-testable.

export const BOARD_HOSTNAME = "127.0.0.1";

const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"]);

/** The Host header values that name this board. No `[::1]`: the board binds
 * IPv4 loopback only, so nothing can reach it under that name. A port
 * forward or proxy must keep the board's port number, or Host won't match. */
export function boardHosts(port: number): Set<string> {
  return new Set([`localhost:${port}`, `127.0.0.1:${port}`]);
}

/** The origins the board's own page can have. */
export function boardOrigins(port: number): Set<string> {
  return new Set([...boardHosts(port)].map((h) => `http://${h}`));
}

/** Why the request must be refused, or null when it may proceed. */
export function rejectReason(req: Request, port: number): string | null {
  const host = req.headers.get("host")?.toLowerCase();
  if (!host || !boardHosts(port).has(host)) {
    return `host '${host ?? ""}' is not this board`;
  }
  if (SAFE_METHODS.has(req.method.toUpperCase())) return null;
  const origin = req.headers.get("origin");
  // "null" (sandboxed iframes, file:// pages) is not in the set, so it fails too.
  if (origin !== null && !boardOrigins(port).has(origin.toLowerCase())) {
    return `cross-origin ${req.method} from '${origin}'`;
  }
  const site = req.headers.get("sec-fetch-site");
  if (site !== null && site !== "same-origin" && site !== "none") {
    return `cross-site ${req.method} (sec-fetch-site: ${site})`;
  }
  return null;
}

/** Wrap a route handler so refused requests never reach it. */
export function guarded<H extends (req: any, ...rest: any[]) => any>(handler: H, port: number): H {
  return ((req: Request, ...rest: any[]) => {
    const reason = rejectReason(req, port);
    if (reason) return Response.json({ error: `forbidden: ${reason}` }, { status: 403 });
    return handler(req, ...rest);
  }) as H;
}
