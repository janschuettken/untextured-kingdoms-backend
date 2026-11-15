# Untextured Kingdoms auf dem VPS (Ubuntu + Apache)

Ziel: Das Spiel läuft unter <https://untexturedkingdoms.jan-schuettken.de>. Apache macht HTTPS (certbot ist schon
eingerichtet) und leitet alles an Docker weiter. In Docker laufen drei Container:

```
Browser ──https/wss──▶ Apache :443 ──▶ 127.0.0.1:8081  web (Caddy: Client-Dateien)
                                                         ├─ /api, /lobby, /ws ──▶ server (Rust, :8080 intern)
                                                         └─────────────────────▶ db (PostgreSQL, intern)
```

Von außen erreichbar ist nur Apache. Server und Datenbank haben gar keinen offenen Port, `web` nur auf 127.0.0.1.

## 1. Docker prüfen (einmalig)

```bash
docker --version
docker compose version
sudo usermod -aG docker $USER    # damit docker ohne sudo geht; danach einmal ab- und wieder anmelden
```

Hat der VPS wenig Arbeitsspeicher (unter 2 GB), bricht der erste Rust-Build gern ab („killed“). Dann vorher Swap anlegen:

```bash
sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

## 2. Repos holen (einmalig)

Beide Repos müssen nebeneinander liegen. Wir nehmen `/opt/untextured-kingdoms`:

```bash
sudo mkdir -p /opt/untextured-kingdoms && sudo chown $USER: /opt/untextured-kingdoms
cd /opt/untextured-kingdoms
git clone -b dev https://github.com/janschuettken/untextured-kingdoms-backend.git
git clone -b dev https://github.com/janschuettken/untextured-kingdoms-frontend.git
```

Sind die Repos privat, fragt Git nach Zugangsdaten. Dann als Passwort ein GitHub-Token mit Lesezugriff nehmen
(GitHub → Settings → Developer settings → Fine-grained tokens, nur diese zwei Repos, „Contents: Read“) und mit
`git config --global credential.helper store` einmal speichern lassen.

## 3. Einstellungen (einmalig)

```bash
cd /opt/untextured-kingdoms/untextured-kingdoms-backend
cp .env.example .env
echo "DB_PASSWORD=$(openssl rand -hex 24)"
echo "ADMIN_TOKEN=$(openssl rand -hex 16)"
nano .env
```

Die zwei ausgegebenen Zeilen in `.env` eintragen (die leeren Zeilen dort ersetzen). `DATABASE_URL` bleibt leer.

- `DB_PASSWORD` brauchst du nie wieder von Hand, es muss nur gleich bleiben (nicht mehr ändern, sobald die
  Datenbank angelegt ist).
- `ADMIN_TOKEN` gibst du im Spiel im Admin-Menü ein. Ohne Token wäre jeder Besucher Admin.

## 4. Bauen und starten

```bash
docker compose up -d --build
```

Das erste Mal dauert es ein paar Minuten (Rust und der Client werden kompiliert). Danach prüfen:

```bash
docker compose ps                         # alle drei "running", db "healthy"
curl -I http://127.0.0.1:8081/            # HTTP/1.1 200 OK
curl http://127.0.0.1:8081/api/maps       # JSON mit Kartennamen
docker compose logs --tail 20 server      # sollte "Accounts ... verbunden" o. Ä. ohne Fehler zeigen
```

Die Container starten nach einem Neustart des VPS von selbst wieder (`restart: unless-stopped`).

## 5. Apache einrichten (einmalig)

Module einschalten:

```bash
sudo a2enmod proxy proxy_http proxy_wstunnel headers
apache2 -v     # muss 2.4.47 oder neuer sein (Ubuntu 22.04 und 24.04 passen)
```

Dann den vHost öffnen:

```bash
sudo nano /etc/apache2/sites-enabled/untexturedkingdoms.jan-schuettken.de.conf
```

Im Block `<VirtualHost *:443>` diese Zeilen ergänzen (die SSL-Zeilen von certbot bleiben, wie sie sind; die
Zeile `DocumentRoot` kann bleiben oder raus, sie wird nicht mehr benutzt):

```apache
    ProxyPreserveHost On
    ProxyRequests Off
    RequestHeader set X-Forwarded-Proto "https"
    ProxyPass        / http://127.0.0.1:8081/ upgrade=websocket timeout=3600
    ProxyPassReverse / http://127.0.0.1:8081/
```

Eine vollständige Beispieldatei liegt in [`apache-vhost.conf`](apache-vhost.conf).

Falls certbot die Datei als `...-le-ssl.conf` angelegt hat und `untexturedkingdoms.jan-schuettken.de.conf` nur der
`:80`-Block ist: die Zeilen in die `-le-ssl.conf` schreiben (dort steht `<VirtualHost *:443>`).

Prüfen und neu laden:

```bash
sudo apache2ctl configtest     # "Syntax OK"
sudo systemctl reload apache2
```

## 6. Testen

<https://untexturedkingdoms.jan-schuettken.de> öffnen. Die Lobby sollte erscheinen und „verbunden“ sein. In den
Entwicklertools (F12 → Netzwerk → „WS“) siehst du `wss://untexturedkingdoms.jan-schuettken.de/lobby` mit Status 101.
Der Client nimmt dafür automatisch die Adresse der Seite (https → wss), da ist nichts einzustellen.

Karteneditor (`/editor.html`) und Balancing (`/balance.html`) gehen auch; Karten speichern ist auf dem Server
absichtlich aus.

## 7. Neue Version ausrollen

```bash
cd /opt/untextured-kingdoms/untextured-kingdoms-backend
./deploy/update.sh
```

Das Skript holt in beiden Repos den neuesten Stand von `dev`, baut neu, startet neu, räumt alte Images auf und
prüft am Ende, ob das Spiel antwortet. Laufende Spiele werden beim Stoppen gespeichert und laufen danach weiter;
Spieler sehen kurz „verbinde neu“. Wartende Lobbys gehen beim Neustart verloren.

## Daten und Sicherung

Alles Wichtige liegt in zwei Docker-Volumes:

- `game_data`: Spielstände (`/data/games`) und das Balancing aus dem Editor (`/data/economy.json`)
- `db_data`: die Datenbank mit den Accounts

Accounts sichern:

```bash
docker compose exec -T db pg_dump -U uk untextured_kingdoms > accounts-$(date +%F).sql
```

`docker compose down` lässt die Volumes stehen. Nur `docker compose down -v` löscht sie (also nicht benutzen).

## Wenn etwas nicht geht

| Problem | Ursache / Lösung |
| --- | --- |
| Browser zeigt „503 Service Unavailable“ | Container läuft nicht: `docker compose ps`, `docker compose logs --tail 50` |
| Seite lädt, aber Lobby verbindet nie | WebSocket kommt nicht durch: `proxy_wstunnel` aktiv? `upgrade=websocket` in der ProxyPass-Zeile? Apache neu geladen? |
| `docker compose` meckert „DB_PASSWORD in .env setzen“ | `.env` fehlt oder `DB_PASSWORD=` ist leer |
| Build bricht mit „killed“ ab | Zu wenig RAM, Swap anlegen (Schritt 1) |
| Accounts gehen nicht, Log sagt „password authentication failed“ | `DB_PASSWORD` wurde nach dem ersten Start geändert. Altes Passwort wieder eintragen |
| Port 8081 schon belegt | In `.env` `WEB_PORT=8082` setzen und in Apache die beiden `8081` ebenfalls ändern |
