# Untextured Kingdoms auf dem VPS (Ubuntu + Apache)

Ziel: Das Spiel läuft unter <https://untexturedkingdoms.jan-schuettken.de>. Apache macht HTTPS (certbot ist schon
eingerichtet) und leitet alles an Docker weiter. In Docker laufen drei Container:

```
Browser ──https/wss──▶ Apache :443 ──▶ 127.0.0.1:8081  web (Caddy: Client-Dateien)
                                                         ├─ /api, /lobby, /ws ──▶ server (Rust, :8080 intern)
                                                         └─────────────────────▶ db (PostgreSQL)
                         127.0.0.1:8082  adminer (Datenbank im Browser) ──────▶ db
```

Von außen erreichbar ist nur Apache. `web`, `adminer` und die Datenbank lauschen nur auf 127.0.0.1, der Server gar nicht.

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
nano .env
```

- Die ausgegebene Zeile `DB_PASSWORD=...` in `.env` eintragen. Es muss danach gleich bleiben (nicht mehr ändern,
  sobald die Datenbank angelegt ist). Du brauchst es nur zum Anmelden in Adminer oder DBeaver.
- `ADMIN_USERS=Jan` steht schon drin: Dein Account mit diesem Namen wird Admin (siehe Schritt 6).
- `DATABASE_URL` bleibt leer.

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

Karteneditor (`/editor.html`) und Balancing (`/balance.html`) gehen auch. Speichern darf in beiden nur ein Admin.
Im Editor gespeicherte Karten liegen im Volume `game_data` und überleben Updates. Neue Karten aus dem Repo kommen
beim Start dazu, eine gleichnamige Karte im Volume wird aber nicht überschrieben.

### Admin werden und Nutzer verwalten

Admin-Rechte hängen am Account, ein Token gibt es nicht mehr.

1. In der Lobby einen Account mit dem Namen aus `ADMIN_USERS` anlegen (z. B. `Jan`) und angemeldet bleiben.
2. Fertig: Admin-Menü im Spiel und Speichern im Balancing gehen sofort. Beim nächsten Neustart wird `is_admin`
   zusätzlich fest in die Datenbank geschrieben.

Als Admin siehst du in der Lobby oben den Link **Admin** (`/admin.html`). Dort stehen alle Nutzer. Du kannst
anderen Admin-Rechte geben oder nehmen und dich mit „Anmelden als“ als jeder Nutzer anmelden. In der Lobby steht
dann oben „Zurück zu Jan“, damit kommst du wieder in deinen eigenen Account.

Weitere Admins gehen auch in Adminer (Tabelle `accounts`, Spalte `is_admin` auf `true`) oder per SQL:

```bash
docker compose exec db psql -U uk untextured_kingdoms -c "UPDATE accounts SET is_admin = true WHERE lower(username) = lower('Name');"
```

Wichtig: Wer in `ADMIN_USERS` steht, ist Admin, sobald es einen Account mit dem Namen gibt. Lege den Account also
gleich nach dem ersten Start selbst an, bevor es jemand anderes tut.

## 7. Neue Version ausrollen

```bash
cd /opt/untextured-kingdoms/untextured-kingdoms-backend
./deploy/update.sh
```

Das Skript holt in beiden Repos den neuesten Stand von `dev`, baut neu, startet neu, räumt alte Images auf und
prüft am Ende, ob das Spiel antwortet. Laufende Spiele werden beim Stoppen gespeichert und laufen danach weiter;
Spieler sehen kurz „verbinde neu“. Wartende Lobbys gehen beim Neustart verloren.

## Balancing auf dem Server ändern und ins Repo bringen

Was du im Balancing-Editor auf dem Server speicherst, gilt sofort für alle Spiele. Es liegt im Volume
(`/data/economy.json`), nicht im Git-Repo. Deshalb stört es auch kein `git pull`.

So kommen die Werte ins Repo:

1. Im Balancing-Editor (`/balance.html`, als Admin) auf **„economy.json herunterladen“** klicken.
2. Die Datei auf deinem PC nach `untextured-kingdoms-backend\server\data\economy.json` kopieren, committen und
   nach `dev` pushen. Das Format ist genau das der Datei im Repo, der Diff zeigt also nur deine Änderungen.

Beim nächsten `./deploy/update.sh` vergleicht das Skript die Server-Fassung mit der aus dem Repo:

- Beide sind gleich: Alles passt, es gibt nichts zu tun.
- Sie weichen ab: **Es bleibt die Server-Fassung** und das Skript zeigt eine Warnung. Hast du absichtlich im Repo
  geändert (z. B. lokal balanciert und gepusht), nimm `./deploy/update.sh --economy-from-repo`. Dann gilt die
  Repo-Fassung, und die alte Server-Fassung bleibt als `/data/economy.json.vor-update` im Volume liegen.

## Datenbank ansehen

Die Datenbank ist von außen nicht erreichbar. Du kommst über einen SSH-Tunnel von deinem PC aus dran.

### Im Browser mit Adminer (wie phpMyAdmin)

Auf deinem PC (PowerShell), Tunnel öffnen und offen lassen:

```powershell
ssh -L 8082:localhost:8082 dein-benutzer@untexturedkingdoms.jan-schuettken.de
```

Dann im Browser <http://localhost:8082> öffnen und anmelden:

| Feld | Wert |
| --- | --- |
| System | PostgreSQL |
| Server | `db` |
| Benutzer | `uk` |
| Passwort | `DB_PASSWORD` aus `.env` |
| Datenbank | `untextured_kingdoms` |

Die Accounts stehen in der Tabelle `accounts`. Tunnel schließen = Fenster mit `ssh` schließen.

### Mit DBeaver (Programm auf dem PC)

Neue Verbindung → PostgreSQL. Reiter „SSH“: SSH-Tunnel an, Host `untexturedkingdoms.jan-schuettken.de`, dein
SSH-Benutzer und Passwort oder Schlüssel. Reiter „Main“: Host `127.0.0.1`, Port `5433`, Datenbank
`untextured_kingdoms`, Benutzer `uk`, Passwort = `DB_PASSWORD`.

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
| Admin-Menü sagt „Nur für Admins“ | In der Lobby angemeldet? Heißt der Account wie in `ADMIN_USERS`? Nach Änderung an `.env`: `docker compose up -d` |
| `docker compose` meckert „DB_PASSWORD in .env setzen“ | `.env` fehlt oder `DB_PASSWORD=` ist leer |
| Build bricht mit „killed“ ab | Zu wenig RAM, Swap anlegen (Schritt 1) |
| Accounts gehen nicht, Log sagt „password authentication failed“ | `DB_PASSWORD` wurde nach dem ersten Start geändert. Altes Passwort wieder eintragen |
| Port 8081 schon belegt | In `.env` `WEB_PORT=8083` setzen und in Apache die beiden `8081` ebenfalls ändern |
