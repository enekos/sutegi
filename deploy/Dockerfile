# syntax=docker/dockerfile:1
#
# Multi-stage build. Pick the example with `--build-arg APP=todo|hello`.
# The runtime image is distroless (no shell, no package manager) — tiny and
# minimal attack surface, which suits sutegi's small static-ish binaries.

ARG APP=todo

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
ARG APP
# Build only the chosen example (and its needed features come from its manifest).
RUN cargo build --release -p ${APP}-example && cp target/release/${APP} /app-bin

# `:nonroot` runs as uid/gid 65532; USER pins it even if the tag changes.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=builder /app-bin /usr/local/bin/app
USER 65532:65532
# The ops surface (/__introspect, /__metrics, /__tools*) is closed unless the
# app sets an ops_guard; todo/storage read it from OPS_TOKEN at runtime — pass
# it as a secret (`-e OPS_TOKEN=…`), never bake it into the image.
ENV HOST=0.0.0.0 \
    PORT=8080 \
    WORKERS=8
EXPOSE 8080
# Liveness/readiness are handled by the orchestrator hitting /__health & /__ready.
ENTRYPOINT ["/usr/local/bin/app"]
