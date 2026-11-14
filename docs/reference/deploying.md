<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Directory structure](layout.md) · [Documentation index](../README.md) · [Request lifecycle →](internals.md)

# Deploying

With the `graceful` feature, `.serve()` does the right thing for a rolling update: it traps SIGTERM/SIGINT, stops accepting new connections, and drains in-flight requests before exiting. (`run(addr)` serves forever and `run_until(addr, flag)` gives manual control without the signal feature.) `ontzi` (Basque: *vessel*) wraps Docker Compose to run the horizontally-scaled shape locally — N replicas behind an nginx load balancer configured with `proxy_buffering off` so SSE streams pass straight through — and promotes the same shape to Kubernetes with manifests that already wire probes, graceful drain and Prometheus annotations. For a single box, a provisioning script installs the binary as a hardened systemd unit behind nginx instead.

```bash
./ontzi up 3            # 3 replicas behind an nginx LB on http://localhost:8080
./ontzi curl /api/todos
./ontzi logs
./ontzi down
./ontzi k8s apply      # promote deploy/k8s/ — probes, drain, Prometheus annotations wired
```

```mermaid
flowchart LR
    subgraph Compose["ontzi up N: Docker Compose"]
        LB["nginx lb on :8080"] -->|"/ with proxy_buffering off"| A1["app replica 1"]
        LB -->|"/ with proxy_buffering off"| AN["app replica N"]
        LB -->|"/__health and /__ready"| A1
        LB --> NF["any other /__ path: 404 at the LB"]
    end
    subgraph K8s["ontzi k8s apply: deploy/k8s"]
        SVC["Service :80"] --> POD["pod :8080, 3 replicas"]
        KL["kubelet"] -->|"liveness /__health"| POD
        KL -->|"readiness /__ready"| POD
        KL -->|"SIGTERM, 30s grace: stop accepting, drain"| POD
        PROM["Prometheus"] -->|"/__metrics with Bearer OPS_TOKEN"| POD
    end
    POD -.->|"shared state across pods"| PGDB[("Postgres")]
```

*The same shape locally under ontzi (replicas behind nginx) and on Kubernetes (probes, SIGTERM drain, authenticated metrics scrape).*

Pick the backend for the deployment, not the code: one instance runs on SQLite (embedded, zero-ops); many pods run on Postgres, which is also what turns the queue’s exclusivity, the advisory locks, the watchers and the channel broker from process-scoped into cluster-scoped. The request surface is stateless and scales horizontally either way, and the binary is small enough that `requests: 32Mi` is a reasonable ask.

---

[← Directory structure](layout.md) · [Documentation index](../README.md) · [Request lifecycle →](internals.md)
