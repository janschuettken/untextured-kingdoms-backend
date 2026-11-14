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
ENV MAPS_DIR=/usr/local/share/untextured-kingdoms/maps
# Balancing aus dem Editor landet hier (Volume), fehlt die Datei, gelten die eingebauten Werte.
ENV ECONOMY_FILE=/data/economy.json
ENV SAVE_FILE=/data/save.json
EXPOSE 8080
CMD ["server"]
