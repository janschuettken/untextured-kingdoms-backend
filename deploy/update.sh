#!/usr/bin/env bash
# Neue Version ausrollen: beide Repos auf den Stand von "dev" bringen, neu bauen, neu starten.
# Aufruf (im Ordner untextured-kingdoms-backend):  ./deploy/update.sh
# Laufende Spiele werden beim Neustart gespeichert und laufen danach weiter.
set -euo pipefail

BRANCH="${BRANCH:-dev}"
cd "$(dirname "$0")/.."
BACKEND="$(pwd)"
FRONTEND="$BACKEND/../untextured-kingdoms-frontend"
# WEB_PORT aus .env lesen (für den Test am Ende).
WEB_PORT="$(sed -n 's/^WEB_PORT=//p' .env 2>/dev/null | tail -1)"
WEB_PORT="${WEB_PORT:-8081}"

for repo in "$BACKEND" "$FRONTEND"; do
  echo "== $(basename "$repo"): hole $BRANCH"
  git -C "$repo" fetch --quiet origin "$BRANCH"
  git -C "$repo" checkout --quiet "$BRANCH"
  git -C "$repo" merge --ff-only --quiet "origin/$BRANCH"
  git -C "$repo" log --oneline -1
done

echo "== Baue und starte neu (das dauert beim ersten Mal ein paar Minuten)"
docker compose up -d --build --remove-orphans

echo "== Räume alte Images auf"
docker image prune -f >/dev/null

echo "== Status"
docker compose ps
sleep 5
if curl -fsS "http://127.0.0.1:$WEB_PORT/" >/dev/null && curl -fsS "http://127.0.0.1:$WEB_PORT/api/maps" >/dev/null; then
  echo "OK: Spiel antwortet auf 127.0.0.1:$WEB_PORT"
else
  echo "Achtung: keine Antwort auf 127.0.0.1:$WEB_PORT, siehe: docker compose logs --tail 50"
  exit 1
fi
