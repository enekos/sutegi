# Bare-metal VPS deploy

A non-Docker alternative to ontzi (`../Dockerfile` + `../../docker-compose.yml`).
Runs **one** sutegi instance directly on an Ubuntu VPS: the release binary as a
hardened systemd service bound to `127.0.0.1`, fronted by nginx.

```sh
# on the VPS, from a checkout of this repo, as a sudo user:
sudo ./deploy/vps/provision.sh
sudo APP=hello DOMAIN=api.example.com TLS=1 ./deploy/vps/provision.sh
```

Knobs (env): `APP` (todo|hello|hexagonal|kv) · `DOMAIN` · `PORT` · `WORKERS` ·
`TLS=1` (Let's Encrypt via certbot) · `SERVICE` (unit name).

What it sets up:

- builds `target/release/<app>`, installs it to `/usr/local/bin/sutegi`
- system user `sutegi`, data dir `/var/lib/sutegi`, env at `/etc/sutegi/sutegi.env`
- systemd unit `sutegi.service` — SIGTERM drain (`run_graceful`), restart-on-crash, sandboxed
- nginx site proxying `:80 → 127.0.0.1:$PORT`, **`proxy_buffering off`** so SSE /
  streaming endpoints flush frames live

Idempotent — re-run to redeploy a new commit. Manage with
`systemctl {status,restart} sutegi` and `journalctl -u sutegi -f`.

For horizontal scaling / multi-pod, use ontzi (compose) or the k8s manifests in
`../k8s/` instead — this path is deliberately single-node.
