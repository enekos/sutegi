#!/usr/bin/env bash
# sutegi Sail — a thin wrapper over Docker Compose (and kubectl) for local
# multi-instance dev, mirroring Laravel Sail's ergonomics.
#
#   ./sail up [N]        build + start with N app replicas (default 3) + LB on :8080
#   ./sail down          stop and remove
#   ./sail build         (re)build the image
#   ./sail logs [svc]    tail logs (default: all)
#   ./sail ps            list containers
#   ./sail curl <path>   curl the load balancer (e.g. ./sail curl /api/todos)
#   ./sail shell         shell into a fresh builder image (distroless runtime has none)
#   ./sail k8s [apply|delete]   apply/delete the Kubernetes manifests
#
# Env: APP=todo|hello (which example to run), DATABASE_PATH=:memory:|/data/app.db
set -euo pipefail

cd "$(dirname "$0")"

# Prefer `docker compose`, fall back to `docker-compose`.
if docker compose version >/dev/null 2>&1; then
  DC=(docker compose)
else
  DC=(docker-compose)
fi

cmd="${1:-help}"; shift || true

case "$cmd" in
  up)
    replicas="${1:-3}"
    "${DC[@]}" up -d --build --scale app="$replicas"
    echo "sutegi up: $replicas app replicas behind http://localhost:8080"
    ;;
  down)    "${DC[@]}" down --remove-orphans ;;
  build)   "${DC[@]}" build ;;
  logs)    "${DC[@]}" logs -f "${1:-}" ;;
  ps)      "${DC[@]}" ps ;;
  curl)    curl -sN "http://localhost:8080${1:-/}"; echo ;;
  shell)   docker run --rm -it -v "$PWD":/src -w /src rust:1-bookworm bash ;;
  k8s)
    action="${1:-apply}"
    kubectl "$action" -f deploy/k8s/
    ;;
  help|*)
    awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
    ;;
esac
