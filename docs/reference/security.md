<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Deploying](deploying.md) · [Documentation index](../README.md)

# Security posture

sutegi ships panic isolation, bounded bodies and headers, two slowloris deadlines, capped keep-alive, per-IP rate limiting, secure-header and CORS middleware, bearer/basic guards, signed-cookie sessions with server-side expiry, CSRF tokens, login throttling and hash-bound sessions. Passwords are PBKDF2-HMAC-SHA256 PHC strings; agent tokens are stored hashed. 5xx messages never reach the client. The query builder guards identifiers, the search grammar sanitizes input before it can reach engine syntax, JSON paths and search queries are bound as parameters, and the fuzz and differential harness runs as a required CI gate.

## Two traps worth reading even if you never touch the code

- **Gate on router segments, not on the path string.** The ops guard used to test `req.path.starts_with("/__")` — but the router trims and splits, so `//__tools/x` reached the same route while failing that test, leaving tool invocation and introspection reachable without the configured credential (CWE-288). The check and the dispatch now agree by construction. **Audit your reverse proxy too**: Apache’s idiomatic `<LocationMatch "^/__">` has the identical blind spot, so a deployment can look doubly protected and be neither. (`MergeSlashes`, on by default since 2.4.39, collapses it — which makes the anchored form correct by accident and one directive away from not being. `^/+__` is free.)
- **A credentialed WebSocket needs `check_origin`.** The same-origin policy does not stop a cross-origin handshake from carrying cookies. See [WebSockets](websockets.md).

The S3 path is hardened in the places subprocess-based HTTP usually is not: credentials go to `curl` on stdin so `Authorization` is invisible to `ps`, the protocol is pinned with `--proto =https`, redirect following is off so a 3xx cannot replay a signature at an attacker-chosen host, TLS floors at 1.2, bodies are capped, certificate verification cannot be disabled, `PUT` bodies stage through a `0600` `O_EXCL` temp file, and `S3Store`’s `Debug` redacts the secret key.

> [!WARNING]
> **Read this before deploying to hostile traffic**
>
> The honest limits are laid out on the [Is it production-ready?](honesty.md) page: no TLS yet (terminate at the LB; keep PG/SMTP in-cluster), no independent security audit, and auth-path timing that is defended but not yet measured. Deploy within a trusted network boundary, and do not put the hand-rolled crypto directly in front of the open internet until those gaps are closed.

---

[← Deploying](deploying.md) · [Documentation index](../README.md)
