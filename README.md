# Untextured Kingdoms – Backend

Autoritativer Spielserver für **Untextured Kingdoms**, ein browserbasiertes 3D-Echtzeitstrategiespiel
mit Multiplayer-Fokus. Der Browser-Client liegt in
[untextured-kingdoms-frontend](https://github.com/janschuettken/untextured-kingdoms-frontend).

## Aufbau

```
server/     Spielserver in Rust (tokio + axum, WebSocket unter /ws, feste Tick-Rate von 20/s)
protocol/   Netzwerk-Nachrichten zwischen Server und Client (einzige Quelle der Wahrheit)
Dockerfile          baut das Server-Image
docker-compose.yml  startet das komplette Spiel (Server + Frontend mit Caddy)
```

Aktueller Stand: Der Server lädt die Karte aus `server/maps/zwei-burgen.txt`, schickt sie jedem neuen
Client (`welcome`) und gibt jedem Spieler einen König auf einem Startfeld. Bewegungsbefehle
(`move_units`) werden serverseitig mit A* über das Navigationsgitter ausgeführt. 20-mal pro Sekunde
geht ein `tick` mit allen Einheiten an alle Clients.

```
server/src/map.rs    Karte aus Textdatei lesen: Höhenstufen, Rampen, Wasser, Eckhöhen
server/src/nav.rs    Navigationsgitter (wer darf wohin) und Wegsuche (A* + Glätten)
server/src/world.rs  Spielzustand: Einheiten, Befehle, Bewegung pro Tick
server/maps/         Karten als Text, Legende steht oben in der Datei
```

## Karten bearbeiten

Karten sind einfache Textdateien, ein Zeichen pro Feld: `0`–`9` Boden auf dieser Höhenstufe,
`~` Wasser, `> < v ^` Rampe, die in Pfeilrichtung ansteigt. Jeder andere Höhensprung ist eine Klippe,
über die man nicht laufen kann. `spawn x y` legt Startfelder fest. Rampen werden beim Laden geprüft;
`cargo test` meldet zum Beispiel, wenn eine Rampe oben nicht an höheren Boden grenzt oder ein Teil der
Karte nicht erreichbar ist.

## Ordner nebeneinander

Beide Repos werden am einfachsten direkt nebeneinander geklont, darauf verlassen sich
`docker-compose.yml` und das Sync-Skript im Frontend:

```bash
mkdir untextured-kingdoms && cd untextured-kingdoms
git clone git@github.com:janschuettken/untextured-kingdoms-backend.git
git clone git@github.com:janschuettken/untextured-kingdoms-frontend.git
```

## Lokal entwickeln

1. **Rust** über rustup installieren: <https://rustup.rs>
   - macOS/Linux: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
   - Windows: `rustup-init.exe` herunterladen und ausführen
   - Prüfen: `cargo --version`
2. Server starten (Port 8080):

```bash
cargo run -p server
```

Den Client startest du im Frontend-Repo mit `npm run dev` (siehe dort).
Gesundheitscheck: <http://localhost:8080/health> liefert `ok`.

## Protokoll ändern

Nachrichten werden nur in `protocol/src/lib.rs` definiert. Ablauf bei einer Änderung:

1. `protocol/src/lib.rs` anpassen
2. Im **Frontend**-Repo `npm run sync-protocol` ausführen. Das ruft hier `cargo test -p protocol` auf,
   generiert die TypeScript-Typen nach `protocol/bindings/` und kopiert sie ins Frontend nach `src/protocol/`.
3. In beiden Repos committen (hier `protocol/bindings/` gleich mit).

## Deployment mit Docker

Auf dem Server brauchst du **nur Docker und Git**, kein Rust und kein Node.
Alles wird beim Bauen im Container kompiliert.

### 1. Docker installieren (einmalig, Linux-Server z. B. Ubuntu/Debian)

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER   # danach einmal ab- und wieder anmelden
docker --version
docker compose version
```

Für lokale Tests auf Windows/macOS gibt es Docker Desktop: <https://www.docker.com/products/docker-desktop/>

### 2. Repos holen und konfigurieren

Beide Repos nebeneinander klonen (siehe oben), dann:

```bash
cd untextured-kingdoms-backend
cp .env.example .env
```

In `.env` setzt du `SITE_ADDRESS`:

- `:80` zum Testen ohne Domain (nur HTTP, Aufruf über die Server-IP)
- deine Domain, z. B. `rts.example.de`, für automatisches HTTPS über Let's Encrypt.
  Die Domain muss per DNS-A-Record auf den Server zeigen und die Ports 80 und 443 müssen offen sein.

### 3. Bauen und starten

```bash
docker compose up -d --build
```

Der erste Build dauert ein paar Minuten (Rust kompiliert alles), danach geht es schneller.

### Nützliche Befehle

```bash
docker compose ps               # Was läuft?
docker compose logs -f server   # Server-Logs live ansehen
docker compose down             # Alles stoppen

# Neue Version ausrollen
git pull && git -C ../untextured-kingdoms-frontend pull && docker compose up -d --build
```

### Was dabei passiert

- `server`: Das Rust-Programm läuft in einem schlanken Debian-Image (ca. 30 MB) und ist nur intern erreichbar.
- `web`: wird aus dem Frontend-Repo gebaut. Caddy liefert den Client aus, leitet `/ws` an den Server weiter
  und kümmert sich um HTTPS-Zertifikate.
