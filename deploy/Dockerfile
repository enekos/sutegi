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

FROM gcr.io/distroless/cc-debian12 AS runtime
COPY --from=builder /app-bin /usr/local/bin/app
ENV HOST=0.0.0.0 \
    PORT=8080 \
    WORKERS=8
EXPOSE 8080
# Liveness/readiness are handled by the orchestrator hitting /__health & /__ready.
ENTRYPOINT ["/usr/local/bin/app"]
