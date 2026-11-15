# 008 — Bounded signalling callback dispatch

Opt-in fixed worker pools with serial lanes per connection. Bound connections,
pending events per lane and total pending bytes; reserve lifecycle cleanup
outside message limits. Never wait for an application callback on a reactor.
Cancel queued messages on close; running callbacks cooperate. Fence retired
channel pushes. Preserve inline mode and pre-upgrade authorization.

Acceptance: one blocked handler leaves other connections and ping/pong
responsive; overload closes and cleans up once; rejoin invalidates stale
pushes. Gate per constitution. No dependency changes.
