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
RUN useradd --system --no-create-home game
COPY --from=build /src/target/release/server /usr/local/bin/server
COPY server/maps /usr/local/share/untextured-kingdoms/maps
USER game
ENV PORT=8080
ENV MAPS_DIR=/usr/local/share/untextured-kingdoms/maps
EXPOSE 8080
CMD ["server"]
