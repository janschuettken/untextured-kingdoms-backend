#!/usr/bin/env bash
# Neue Version ausrollen: beide Repos auf den Stand von "dev" bringen, neu bauen, neu starten.
# Aufruf (im Ordner untextured-kingdoms-backend):  ./deploy/update.sh
# Laufende Spiele werden beim Neustart gespeichert und laufen danach weiter.
#
# Balancing: Was im Balancing-Editor auf dem Server gespeichert wurde, liegt im Volume
# (/data/economy.json) und gilt weiter, auch wenn im Repo eine andere economy.json steht.
# Weichen die beiden ab, sagt das Skript Bescheid. Mit
#   ./deploy/update.sh --economy-from-repo
# gilt danach die Fassung aus dem Repo (die Server-Fassung bleibt als economy.json.vor-update liegen).
set -euo pipefail

ECONOMY_FROM_REPO=0
RESTART_SERVER=0
for arg in "$@"; do
  case "$arg" in
    --economy-from-repo) ECONOMY_FROM_REPO=1 ;;
    *) echo "Unbekannte Option: $arg"; exit 2 ;;
  esac
done

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

echo "== Balancing prüfen"
LIVE="$(docker compose exec -T server cat /data/economy.json 2>/dev/null || true)"
# Repo-Fassung in dasselbe Format bringen wie die Server-Fassung (Reihenfolge, Standardwerte).
REPO="$(docker compose exec -T server server economy-canonical < server/data/economy.json 2>/dev/null || cat server/data/economy.json)"
if [ -z "$LIVE" ]; then
  echo "Auf dem Server wurde nichts im Balancing gespeichert: es gilt die economy.json aus dem Repo."
elif [ "$LIVE" = "$REPO" ]; then
  echo "Balancing auf dem Server und im Repo sind gleich."
elif [ "$ECONOMY_FROM_REPO" = 1 ]; then
  docker compose exec -T server sh -c 'cp /data/economy.json /data/economy.json.vor-update && rm /data/economy.json'
  RESTART_SERVER=1
  echo "Balancing: ab jetzt gilt die Fassung aus dem Repo (alte Server-Fassung: /data/economy.json.vor-update)."
else
  echo "ACHTUNG: Das Balancing auf dem Server weicht von der economy.json im Repo ab."
  echo "         Es bleibt die Server-Fassung. Willst du die aus dem Repo: ./deploy/update.sh --economy-from-repo"
  echo "         Server-Fassung sichern: im Balancing-Editor \"economy.json herunterladen\" und ins Repo committen."
fi

echo "== Baue und starte neu (das dauert beim ersten Mal ein paar Minuten)"
docker compose up -d --build --remove-orphans
# Neue Balancing-Fassung braucht einen Neustart, auch wenn sich das Image nicht geändert hat.
if [ "$RESTART_SERVER" = 1 ]; then docker compose restart server; fi

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
