#!/usr/bin/env bash
#
# sutegi VPS provision — a *non-Docker* alternative to ontzi (deploy/Dockerfile
# + docker-compose.yml). Deploys a SINGLE sutegi instance directly onto an
# Ubuntu VPS: build the release binary, run it as a hardened systemd service
# bound to localhost, and front it with nginx as a reverse proxy.
#
# Why bare-metal: sutegi binaries are tiny and std-only, so there's little to
# gain from a container on a single box. systemd gives you the SIGTERM drain
# (run_graceful) + restart-on-crash; nginx gives you :80/:443, TLS termination,
# and SSE-friendly buffering-off proxying for the streaming endpoints.
#
# Usage (run on the VPS, from a checkout of this repo, as a sudo-capable user):
#
#   sudo ./deploy/vps/provision.sh                       # todo example on todo.example -> _
#   sudo APP=hello ./deploy/vps/provision.sh
#   sudo DOMAIN=api.example.com ./deploy/vps/provision.sh
#   sudo DOMAIN=api.example.com TLS=1 ./deploy/vps/provision.sh   # + Let's Encrypt via certbot
#
# Env knobs:
#   APP        which example to build/run: todo | hello | hexagonal | kv   (default: todo)
#   DOMAIN     nginx server_name (default: _  — matches any host)
#   PORT       localhost port the app binds to, nginx upstream (default: 8080)
#   WORKERS    thread-per-connection worker count (default: 8)
#   TLS        set to 1 to obtain a Let's Encrypt cert with certbot (needs a real DOMAIN)
#   SERVICE    systemd unit name (default: sutegi)
#
# Idempotent: re-running rebuilds the binary and re-renders the unit + nginx
# site, then restarts. Safe to run repeatedly (e.g. to deploy a new commit).

set -euo pipefail

# --- config -----------------------------------------------------------------
APP="${APP:-todo}"
DOMAIN="${DOMAIN:-_}"
PORT="${PORT:-8080}"
WORKERS="${WORKERS:-8}"
TLS="${TLS:-0}"
SERVICE="${SERVICE:-sutegi}"

RUN_USER="sutegi"
INSTALL_BIN="/usr/local/bin/${SERVICE}"
DATA_DIR="/var/lib/${SERVICE}"
ENV_FILE="/etc/${SERVICE}/${SERVICE}.env"
UNIT_FILE="/etc/systemd/system/${SERVICE}.service"
NGINX_SITE="/etc/nginx/sites-available/${SERVICE}.conf"

# Repo root = two levels up from this script (deploy/vps/provision.sh).
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

die() { echo "provision: $*" >&2; exit 1; }
log() { echo "==> $*"; }

[[ $EUID -eq 0 ]] || die "run as root (sudo)."
[[ -f "$REPO_ROOT/Cargo.toml" ]] || die "can't find repo root (expected $REPO_ROOT/Cargo.toml)."

# The example package is "<app>-example" with a bin named "<app>" (see examples/*/Cargo.toml).
case "$APP" in
  todo|hello|hexagonal|kv) ;;
  *) die "unknown APP='$APP' (want: todo | hello | hexagonal | kv)." ;;
esac
BUILT_BIN="$REPO_ROOT/target/release/${APP}"

# --- 1. system packages -----------------------------------------------------
log "installing system packages (nginx, build toolchain)"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq nginx build-essential curl ca-certificates pkg-config

# Rust: use the invoking user's rustup if present, else install system-wide.
if ! command -v cargo >/dev/null 2>&1; then
  log "installing Rust toolchain (rustup)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

# --- 2. build the release binary --------------------------------------------
log "building ${APP}-example (release)"
( cd "$REPO_ROOT" && cargo build --release -p "${APP}-example" )
[[ -f "$BUILT_BIN" ]] || die "build produced no binary at $BUILT_BIN."

# --- 3. service user + dirs -------------------------------------------------
if ! id -u "$RUN_USER" >/dev/null 2>&1; then
  log "creating system user '$RUN_USER'"
  useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin "$RUN_USER"
fi
install -d -o "$RUN_USER" -g "$RUN_USER" "$DATA_DIR"
install -d "$(dirname "$ENV_FILE")"

# --- 4. install binary ------------------------------------------------------
log "installing binary -> $INSTALL_BIN"
install -m 0755 "$BUILT_BIN" "$INSTALL_BIN"

# --- 5. env file (12-factor config; app reads HOST/PORT/WORKERS via env_or) --
# Bind to loopback only — nginx is the sole public entrypoint.
if [[ ! -f "$ENV_FILE" ]]; then
  log "writing $ENV_FILE"
  cat > "$ENV_FILE" <<EOF
HOST=127.0.0.1
PORT=${PORT}
WORKERS=${WORKERS}
# For the sqlite-backed examples you can point at a persistent file, e.g.:
# DATABASE_PATH=${DATA_DIR}/app.db
EOF
  chmod 0640 "$ENV_FILE"
  chown root:"$RUN_USER" "$ENV_FILE"
else
  log "$ENV_FILE exists — leaving it untouched"
fi

# --- 6. systemd unit --------------------------------------------------------
log "rendering $UNIT_FILE"
cat > "$UNIT_FILE" <<EOF
[Unit]
Description=sutegi app (${APP})
Documentation=https://github.com/enekos/sutegi
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${RUN_USER}
Group=${RUN_USER}
EnvironmentFile=${ENV_FILE}
WorkingDirectory=${DATA_DIR}
ExecStart=${INSTALL_BIN}
Restart=on-failure
RestartSec=2s
# run_graceful traps SIGTERM and drains in-flight requests; give it room.
KillSignal=SIGTERM
TimeoutStopSec=30s

# Hardening — the app only needs to write its own data dir.
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=${DATA_DIR}
ProtectKernelTunables=true
ProtectControlGroups=true
RestrictSUIDSGID=true

[Install]
WantedBy=multi-user.target
EOF

# --- 7. nginx reverse proxy -------------------------------------------------
log "rendering nginx site -> $NGINX_SITE"
cat > "$NGINX_SITE" <<EOF
# sutegi single-instance reverse proxy (generated by deploy/vps/provision.sh).
upstream ${SERVICE}_upstream {
    server 127.0.0.1:${PORT};
    keepalive 16;
}

server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN};

    location / {
        proxy_pass http://${SERVICE}_upstream;
        proxy_set_header Host              \$host;
        proxy_set_header X-Real-IP         \$remote_addr;
        proxy_set_header X-Forwarded-For   \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;

        # Critical for sutegi SSE / streaming (POST /__tools/:name/stream, /stream):
        # don't buffer so streamed frames reach the client as they're produced.
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 1h;

        # sutegi is currently connection-per-request; HTTP/1.1 + emptied
        # Connection header lets nginx pool upstream conns where it can.
        proxy_http_version 1.1;
        proxy_set_header Connection "";
    }
}
EOF

ln -sfn "$NGINX_SITE" "/etc/nginx/sites-enabled/${SERVICE}.conf"
# Drop the stock default site so it doesn't shadow server_name=_.
rm -f /etc/nginx/sites-enabled/default

log "validating nginx config"
nginx -t

# --- 8. start everything ----------------------------------------------------
log "enabling + (re)starting ${SERVICE}.service"
systemctl daemon-reload
systemctl enable "${SERVICE}.service" >/dev/null
systemctl restart "${SERVICE}.service"

log "reloading nginx"
systemctl reload nginx

# --- 9. optional TLS via certbot -------------------------------------------
if [[ "$TLS" == "1" ]]; then
  [[ "$DOMAIN" != "_" ]] || die "TLS=1 requires a real DOMAIN (got '_')."
  log "obtaining Let's Encrypt certificate for $DOMAIN"
  apt-get install -y -qq certbot python3-certbot-nginx
  certbot --nginx -d "$DOMAIN" --non-interactive --agree-tos --register-unsafely-without-email --redirect
fi

# --- 10. smoke check --------------------------------------------------------
log "waiting for readiness on 127.0.0.1:${PORT}/__health"
for _ in $(seq 1 20); do
  if curl -fsS "http://127.0.0.1:${PORT}/__health" >/dev/null 2>&1; then
    log "sutegi '${APP}' is up: nginx :80 -> 127.0.0.1:${PORT} (server_name ${DOMAIN})"
    echo
    echo "  logs:    journalctl -u ${SERVICE} -f"
    echo "  status:  systemctl status ${SERVICE}"
    echo "  config:  ${ENV_FILE}  (edit + 'systemctl restart ${SERVICE}')"
    echo "  probes:  /__health  /__ready  /__metrics  /__introspect"
    exit 0
  fi
  sleep 0.5
done

die "app did not answer /__health — check: journalctl -u ${SERVICE} -e"
