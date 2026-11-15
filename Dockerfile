# syntax=docker/dockerfile:1
# ---------- Bauen ----------
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY protocol protocol
COPY server server
RUN cargo build --release -p server

# ---------- Laufzeit ----------
FROM debian:bookworm-slim
RUN useradd --system --no-create-home game && mkdir -p /data && chown game /data
COPY --from=build /src/target/release/server /usr/local/bin/server
COPY server/maps /usr/local/share/untextured-kingdoms/maps
USER game
ENV PORT=8080
# Karten liegen im Volume, damit im Editor gespeicherte Karten Updates überleben.
# Beim Start kommen Karten aus dem Repo dazu, die dort noch fehlen (vorhandene bleiben unangetastet).
ENV MAPS_DIR=/data/maps
# Balancing aus dem Editor landet hier (Volume), fehlt die Datei, gelten die eingebauten Werte.
ENV ECONOMY_FILE=/data/economy.json
# Laufende Spiele (je Spiel eine Datei), werden beim Start fortgesetzt.
ENV SAVES_DIR=/data/games
EXPOSE 8080
CMD ["sh", "-c", "mkdir -p /data/maps && (cp -rn /usr/local/share/untextured-kingdoms/maps/. /data/maps/ || true) && exec server"]
