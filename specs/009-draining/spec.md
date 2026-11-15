# 009 — Coordinated server draining

Share a monotonic Running/Draining/Stopped lifecycle across HTTP, readiness,
WebSocket admission and listeners. Refuse new WS sessions during drain unless
an explicit application gate authorizes an existing session. Continue existing
connections until a deadline. Stop reactors, cancel pending HTTP jobs and close
active HTTP sockets at expiry. Do not block the accept loop on a full queue.
Running callbacks require cooperation; document detached-thread behavior.

Acceptance: real-socket echo during drain followed by exactly-once cleanup;
reconnect admission; readiness returns 503; repeated drain never extends grace.
Gate per constitution; no dependencies.
