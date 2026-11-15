# Untextured Kingdoms – Backend

Autoritativer Spielserver für **Untextured Kingdoms**, ein browserbasiertes 3D-Echtzeitstrategiespiel
mit Multiplayer-Fokus. Der Browser-Client liegt in
[untextured-kingdoms-frontend](https://github.com/janschuettken/untextured-kingdoms-frontend).

## Aufbau

```
server/     Spielserver in Rust (tokio + axum): Lobby unter /lobby, jedes Spiel mit eigenem Thread unter /ws (20 Ticks/s)
protocol/   Netzwerk-Nachrichten zwischen Server und Client (einzige Quelle der Wahrheit)
Dockerfile          baut das Server-Image
docker-compose.yml  startet das komplette Spiel (Server + Datenbank + Frontend mit Caddy)
deploy/             Anleitung, Apache-vHost und Update-Skript für den VPS
```

Aktueller Stand: Der Server lädt die Karte aus `server/maps/zwei-burgen.txt`, schickt sie jedem neuen
Client (`welcome`) und gibt jedem Spieler einen König auf einem Startfeld. Bewegungsbefehle
(`move_units`) werden serverseitig mit A* über das Navigationsgitter ausgeführt. 20-mal pro Sekunde
geht ein `tick` mit allen Einheiten an alle Clients.

Wirtschaft: Waren, Gebäude (Größe, Lebenspunkte, Stufen mit Kosten), Rezepte, Startlager und Lagerplatz stehen
in **`server/data/economy.json`**. Das ist die einzige Quelle: der Server prüft die Datei beim Start (und in
`cargo test`) und schickt sie im `welcome` an den Client, der daraus Baumenü, Kosten und Infofelder baut.
Balancing heißt also nur diese Datei ändern. Ein Ausbau schaltet neue Rezepte frei statt pauschaler Boni
(Bogner Stufe 1 Langbogen, Stufe 2 dazu Armbrust).

Gebäude (`place_building`, `upgrade_building`, `demolish_building`): Der Server prüft die Bauregeln (ebener Boden,
keine Rampe, kein Wasser, kein Wald oder Fels, nichts im Weg, Höchstzahl pro Spieler) und die Kosten, blockiert die
Felder im Navigationsgitter und lässt laufende Einheiten neu planen. Abriss erstattet einen Teil aller Bau- und
Ausbaukosten. Neue Clients bekommen im `welcome` alle stehenden Gebäude und ihr Lager, danach nur Änderungen
(`building_placed`, `building_changed`, `building_removed`, `stock` nur an den eigenen Spieler).

Produktion (`server/src/world/production.rs`): Jeder Betrieb arbeitet sein eingestelltes Rezept ab
(`configure_building`: Rezept, Pause, Lieferungen annehmen). Eingangswaren kommen beim Start aus dem eigenen Lager
des Betriebs (`inventory`, höchstens `transport.building_capacity` je Ware), das Ergebnis geht am Ende hinein. Abbaubetriebe brauchen Rohstoff in Reichweite (`gather_radius`) und verbrauchen ihn
(`resource_amounts`); ein leeres Waldfeld verschwindet und gibt den Weg frei (`resource_changed`). `placement` legt
fest, worauf ein Gebäude stehen muss (Steinbruch auf Stein, Mine auf Eisen, Bauernhof und Rinder auf Acker;
fruchtbarer Boden macht sie schneller). Die Obstplantage lässt sich aufgeben (`release_trees`), dann wird ihre
Fläche zu Wald. Zustand und Lager jedes Betriebs gehen bei Änderung an alle (`building_changed`).

Transport (`server/src/world/transport.rs`): Der gemeinsame Vorrat liegt in Bergfried und Vorratslagern (`hub`).
Träger und Karren kommen nur aus dem Wagenschuppen (`spawns` je Stufe: Stufe 1 Träger, Stufe 2 dazu ein Karren). Ohne
sie bedient jeder Betrieb sich selbst (`server/src/world/errand.rs`): sein Arbeiter trägt Fertiges ins nächste Lager
und holt fehlende Eingänge, in der Zeit steht der Betrieb (`WorkerAway`). Träger holen fertige Waren aus den Betrieben
ins nächste Lager und bringen fehlende Eingänge hin, wenn der Betrieb Lieferungen annimmt. Der Karren fährt erst ab
`cart_min_load` Waren los, lädt viel und ist schneller; solange er frei ist, lassen die Träger so große Ladungen für
ihn liegen. Lager lassen sich auf bestimmte Waren beschränken (`stores`), dann bringen Träger nur diese dorthin. Jeder Betrieb
kann je Eingang einen Mindestbestand im Vorrat festlegen (`keep_in_stock`): geliefert wird nur, was darüber liegt.
Träger und Karren fahren nur Betriebe im Umkreis ihres Schuppens an (`service_radius`); der Karren sammelt bei mehreren gleichen
Betrieben nacheinander ein, wenn einer allein nicht genug hat. Träger und Karren nehmen keine Befehle an und weichen neuen Gebäuden aus. Bauen bezahlt man vorerst
direkt aus dem Vorrat.

Bewohner (`server/src/world/people.rs`): Betriebe brauchen Arbeiter (`workers`), Träger und Karren je
`population.per_transport` Bewohner. Platz gibt es nur in Wohnhäusern (`housing` je Stufe: Stufe 1 = 4, Stufe 2 = 8).
Zuerst bekommen Träger und Karren eine Wohnung, dann die Betriebe in Bau-Reihenfolge; wer keine hat, arbeitet nicht
(Status `no_workers`). Alle `meal_seconds` isst jeder arbeitende Bewohner eine Einheit aus `population.food`; reicht es
nicht, arbeiten und laufen alle mit `hungry_speed_percent` Tempo. Man kann auch gleich auf einer höheren Stufe bauen
(`place_building.level`), das kostet alle Stufen bis dahin zusammen.

Abbau (`server/src/world/gather.rs`): Holzfäller, Steinbruch und Mine haben Arbeiter (`UnitKind::Worker`, Anzahl =
`workers`). Ein Arbeiter läuft zum nächsten Rohstoff-Feld in `gather_radius`, arbeitet dort `seconds` lang und trägt
die Ware zurück. Gibt es Träger oder Karren, legt er sie im Betrieb ab; sonst bringt er sie selbst ins nächste Lager
und baut in der Zeit nichts ab. Lauftempo `movement.worker_speed`. Wählt man eine Einheit aus, schickt der Client
`watch_unit`, und der Server meldet ihren Weg (`unit_path`), wenn er sich ändert.

Taler und Markt (`server/src/world/market.rs`): Jeder Spieler startet mit `market.start_taler`. Satte Bewohner zahlen
pro Mahlzeit `market.tax_per_meal` Taler Steuern. Mit einem eigenen Markt (mit Arbeiter) kauft und verkauft man Waren
zu den festen Preisen aus `market.prices` (`trade`); vorerst ein gemeinsamer Markt, die Ware geht direkt in den Vorrat.
Kosten fürs Heer in Talern folgen, sobald es Truppen gibt.

Tore und Wehrgänge (`server/src/world/gate.rs`): Ein Tor ist 3x1 Felder, der mittlere Durchgang ist offen oder zu
(`BuildingState.gate_open`). Modus je Tor (`settings.gate`): `auto` schließt, sobald fremde Einheiten näher als
`gate_radius` sind, `open` und `closed` bleiben fest. Wer beim Schließen im Durchgang steht, wird hinausgeschoben.
Ab `walkway_from_level` laufen eigene Einheiten oben auf dem Gebäude (Mauer und hohe Mauer ab Stufe 2, Turm und Tor
immer); hinauf geht es nur über die Treppe eines Tores ab `stairs_from_level` (Stufe 2), vom Feld vor oder hinter dem
Durchgang. Einheiten haben dafür `on_wall`; Wege über Mauer und Treppen plant `World::plan`.
Tore und Türme (`on_walls`) darf man direkt auf eigene Mauern setzen; die Mauerstücke darunter werden dabei
abgerissen (mit der üblichen Rückerstattung). Türme gibt es zweimal: den Turm (3x3, `garrison` 12) und den hohen Turm
(2x2, viel höher, `garrison` 4). `garrison` begrenzt, wie viele Einheiten oben stehen; wer nicht mehr hinaufpasst,
stellt sich unten neben den Turm.

Lagerbestand je Lager (`server/src/world/storage.rs`): Der Vorrat (`stocks`) bleibt die Summe, die oben angezeigt
wird. Zusätzlich führt jedes Lager Buch, was bei ihm liegt (`BuildingState.inventory` bei Lagern). Träger und
Arbeiter buchen beim Ablegen und Holen um; was sonst den Vorrat ändert (Bauen, Markt, Essen), gleicht
`balance_hubs` jeden Tick aus. Der Bergfried fasst `base_capacity`, jedes Vorratslager `storehouse_capacity` je Ware;
ist ein Lager voll, geht der Rest ins nächste. Ohne Lager ist der Platz 0: die Startgüter liegen dann „virtuell“
im Vorrat (man kann damit bauen) und ziehen ins erste Lager, sobald es steht.

Gebäude mit `first_free` (heute das Vorratslager) kosten beim ersten Mal nichts; reißt man das letzte ab, gibt es dafür
nichts zurück.

Besondere Lager: Kornspeicher (`granary`) und Waffenkammer (`armory`) haben `hub_goods` und nehmen nur diese Waren;
ihr Platz (`hub_capacity`) zählt als Zusatzplatz nur für diese Waren (`Stock::extra`, `StockState.extra_capacity`).
Auch Vorratslager und Bergfried haben `hub_goods` (nur Waren/Rohstoffe): Essen passt nur in den Kornspeicher,
Waffen nur in die Waffenkammer. Was kein Lager nimmt (z. B. Start-Brot ohne Kornspeicher), bleibt im virtuellen Vorrat.

Lager im Betrieb: `transport.building_capacity` (Standard 20) je Ware, je Gebäudeart überschreibbar,
getrennt für Eingangswaren (`storage_inputs`) und Ergebnisse (`storage_outputs`); nur im Balancing, nicht im Spiel. Einheiten laden und abladen nur am Eingangsfeld (`at_door`),
auch wenn Gebäude direkt nebeneinander stehen.

Rezepte: `unattended_percent` = so viel Prozent am Anfang des Durchlaufs laufen ohne Arbeiter weiter (Wachsen),
auch wenn er gerade Waren trägt. Fortschritt je Betrieb: `BuildingState.progress` + `progress_rate` (Client rechnet
weiter, Server schickt nur bei Abweichung, siehe `world/progress.rs`). Status `stock_reserved`: Eingänge nur bis zum
Mindestbestand da. Beobachtete eigene Einheit (`WatchUnit`): Server schickt `UnitInfo` (Tätigkeit, Ziel, Ladung).


Einstellungen je Betrieb: `worker_carries` (Standard: an) lässt den Arbeiter selbst tragen, auch wenn es Träger gibt (die lassen
den Betrieb dann aus). `keep_in_stock` ist der Mindestbestand je Eingang, `stop_at` der Höchstbestand je Ergebnis:
liegt so viel im Vorrat, fängt der Betrieb nichts Neues an (Status `target_reached`). Steht ein Arbeiter ohne Weg und Last an seinem
Gebäude, ist er drinnen (`UnitState.inside`) und wird außen nicht gezeigt.

Versetzen (`move_building`): Eigene Gebäude lassen sich gegen eine Gebühr umsetzen, `move_cost_percent` der Baukosten
bis zu seiner Stufe (aufgerundet). Stufe, Einstellungen und Inhalt bleiben.

Texte: Der Server schickt nie fertige Sätze, sondern Schlüssel mit Platzhaltern (`Text { key, args }`, z. B.
`error.not_enough` mit `good` und `need`). Übersetzt wird im Client (Deutsch und Englisch).

Lager (`server/src/stock.rs`): Jede Ware liegt in Chargen mit optionalem Verfallsdatum. Heute verdirbt nichts;
verderbliche Waren brauchen später nur ein Verfallsdatum beim Einlagern und einen Aufruf von `expire` pro Tick.

```
server/src/map.rs    Karte aus Textdatei lesen: Höhenstufen, Wasser, Eckhöhen (Schrägen/Wände)
server/src/nav.rs    Navigationsgitter und hierarchische Wegsuche (Blöcke 16×16, Korridor-A*, Glätten)
server/src/mapgen.rs Zufallskarten bis 1024 × 1024 für bis zu 8 Spieler
server/src/maps_api.rs  HTTP-API für den Karteneditor
server/src/world.rs  Spielzustand: Einheiten, Gebäude, Befehle, Bewegung pro Tick
server/src/economy.rs   Wirtschaftsdaten aus data/economy.json laden und prüfen
server/src/stock.rs     Lagerbestand mit Chargen (vorbereitet für verderbliche Waren)
server/data/economy.json  Waren, Gebäude, Stufen, Kosten, Rezepte (Balancing hier)
server/maps/         Karten als Text, Legende steht oben in der Datei
```

## Karten bearbeiten

Am bequemsten mit dem **Karteneditor** im Browser (`/editor.html`, siehe Frontend-README). Er liest und
schreibt dieselben Textdateien:

```
# Kommentar
spawn 10 10
[terrain]
0001123333
...
[resources]
..WW....
```

Maßstab: ein Feld ≈ 3 × 3 m, eine Höhenstufe ≈ 1,5 m. Ein einfaches Haus ist etwa 3 × 3 Felder groß,
Bäume 3–4 Felder hoch. Karten dürfen bis 1024 × 1024 Felder groß sein.

- `[terrain]`, ein Zeichen pro Feld: `0`–`9` Boden auf dieser Höhenstufe, `~` Wasser. Nachbarn, die sich
  um genau eine Stufe unterscheiden, gehen als begehbare Schräge ineinander über; ab zwei Stufen
  Unterschied steht dazwischen eine Wand. Rampen malt man also nicht, man legt Stufen nebeneinander
  (`0123` ist eine Rampe drei Stufen hoch). Die alten Pfeile `> < v ^` meldet die Prüfung als Fehler.
- `[resources]` (optional, gleiche Größe): `.` nichts, `W` Wald, `S` Steinbruch, `E` Eisen, `A` Acker,
  `F` besonders fruchtbar. Wald, Stein und Eisen blockieren Einheiten, bis sie abgebaut sind.
- `spawn x y` legt Startfelder fest.

Welche Karte der Server lädt, steuert `MAP=<name>` (ohne `.txt`, Standard `zwei-burgen`), den Ordner
`MAPS_DIR` (Standard `server/maps`). `cargo test` prüft alle Karten im Ordner.

### Karten-API (für den Editor)

| Methode | Pfad | Zweck |
| --- | --- | --- |
| GET | `/api/maps` | Kartennamen und ob Speichern erlaubt ist |
| GET | `/api/maps/{name}` | Kartendatei als Text |
| POST | `/api/maps/check` | Text prüfen: Größe, Startfelder, Fehler, Hinweise |
| PUT | `/api/maps/{name}` | Karte speichern (nur ohne Fehler) |
| GET | `/api/maps/generate?width=&height=&players=&seed=` | Zufallskarte als Text |
| GET | `/api/games/{id}/map?token=` | Die laufende Karte eines Spiels mit aktuellem Stand (nur Mitspieler; der Client lädt sie nach `welcome`) |

## Große Karten

Gebaut für 1000 × 1000 Felder mit etwa 8 Spielern:

- Wegsuche hierarchisch: erst über Gebiete in Blöcken zu 16 × 16 Feldern, dann feldgenau nur im
  Korridor. Ein Weg quer über eine 1000er-Karte dauert etwa 2 ms. Gebäude und Abholzen teilen nur die
  betroffenen Blöcke neu ein.
- Die Karte geht nicht über den WebSocket, sondern als Text über `/api/games/{id}/map` (2 MB, mit gzip
  deutlich weniger). Danach kommen nur Änderungen (`resource_changed`, Gebäude).
- Ticks gehen pro Client: nur Einheiten im Sichtbereich (`view`, plus Rand) und die eigenen, und davon
  nur neue oder veränderte; `gone` nennt Einheiten, die weg oder aus dem Bild sind.
- `cargo test --release -p server -- --ignored --nocapture` erzeugt und prüft eine 1000er-Karte mit
  8 Spielern und gibt die Zeiten aus.

Speichern dürfen Admins und Nutzer mit dem Recht „Karteneditor“ (`accounts.can_edit_maps`, schaltet ein Admin auf
der Admin-Seite frei; lokal im Debug-Build jeder). Den Link zum Editor gibt es nur in der Lobby und nur für sie.
Löschen (`DELETE /api/maps/{name}`) dürfen nur Admins; die letzte Karte bleibt. Gelöschte Karten merkt sich der
Kartenordner in `.deleted/`, damit sie nicht aus `MAPS_SEED_DIR` (in Docker die Karten aus dem Image) wiederkommen.
`MAP_EDITOR_SAVE=0` schaltet Speichern ganz ab. In Docker liegen die Karten im Volume `game_data` (`/data/maps`),
damit gespeicherte Karten Updates überleben; beim Start kommen Karten aus dem Repo dazu, die dort noch fehlen.
Eine Karte, die es schon im Volume gibt, wird vom Repo nicht überschrieben.

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

## Admin und Balancing

Im Spiel gibt es ein Admin-Menü (Waren und Taler geben, Tempo) und den Balancing-Editor
(`/balance.html` im Frontend), in dem man alle Werte aus `economy.json` ändert: Kosten, Stufen, Rezepte,
Zeiten, Lager, Transport. Gespeichert wird auf dem Server, geprüft wie beim Start, und es gilt sofort für alle.

Admin-Rechte hängen am Account (Spalte `is_admin` in der Tabelle `accounts`). Admin-Menü, Balancing-Speichern und
„Spielen als“ gehen nur, wenn man in der Lobby mit so einem Account angemeldet ist. Den ersten Admin setzt man über
`ADMIN_USERS` (Namen durch Komma getrennt, z. B. `ADMIN_USERS=Jan` in `.env`): Wer so heißt, ist Admin und bekommt
`is_admin` beim nächsten Start fest eingetragen. Weitere Admins per SQL:
`UPDATE accounts SET is_admin = true WHERE lower(username) = lower('Name');`

Ohne Datenbank gibt es keine Accounts, also keine Admins. Für die lokale Entwicklung ist im Debug-Build (`cargo run`)
deshalb jeder Admin (`ADMIN_OPEN`, Standard an im Debug-Build, aus im Release-Build/Docker; `ADMIN_OPEN=0` bzw. `1`
überschreibt das).

Nutzerverwaltung: In der Lobby sehen Admins oben den Link „Admin“ (`/admin.html`): alle Accounts, Admin-Recht
geben/nehmen, Karteneditor erlauben/sperren und „Anmelden als“ (der Browser übernimmt den Spieler-Token des Nutzers und merkt sich den eigenen; in der
Lobby geht es mit „Zurück zu …“ zurück). Schnittstelle: `/api/admin/users…` in `server/src/admin.rs`.

Im Spiel oben rechts auf „Admin“ klicken. Dort gibt es auch das
Lauftempo (König, Träger, Karren; wird in `economy.json` gespeichert) und das Spieltempo für alle (25–800 %,
gilt bis zum Neustart des Servers).
Gespeichert wird nach `ECONOMY_FILE`, lokal standardmäßig `server/data/economy.json` im Repo: so lassen sich
gute Werte einfach committen und teilen. Die vorige Fassung bleibt als `economy.json.bak` liegen. In Docker liegt die
Datei im Volume `game_data` (`/data/economy.json`); fehlt sie, gelten die Werte aus dem Repo.
Schnittstelle: siehe `server/src/admin.rs`.

### Spielstand

Jedes laufende Spiel speichert sich jede Minute nach `SAVES_DIR/<id>.json` (lokal `server/data/games/`, nicht im
Repo, in Docker `/data/games` im Volume `game_data`), samt Karte und Startplätzen. Nach einem Neustart des Servers
laufen alle gespeicherten Spiele weiter. Wartende Lobbys gehen beim Neustart verloren. Im Admin-Menü eines Spiels
gibt es „Speichern“, „Laden“, „Neues Spiel“ und „Spiel beenden“ (`POST /api/games/{id}/save`, `/load`, `/new-game`,
`/end`); danach laden alle Browser neu. `GET /api/games/{id}/players` listet die Spieler; als Admin kann man „Spielen
als“ einen anderen Startplatz übernehmen (`/ws?game=..&token=..&as=<nr>`) oder ein Reich entfernen.

## Lobby und Spiele

- Wer die Seite öffnet, landet in der Lobby (WebSocket `/lobby`). Dort legt man ein Spiel auf einer Karte an oder
  tritt einem wartenden bei, sieht auf der Vorschau, wer wo startet, wechselt den Startplatz, und der Gastgeber startet.
- Viele Spiele laufen gleichzeitig, jedes mit eigener Welt und eigenem Thread (`spiel-<id>`).
- Spieler-Id: Der Browser erzeugt einmal einen geheimen Token und speichert ihn im `localStorage`. Wer neu lädt, kommt
  mit demselben Token zurück in seine Lobby oder sein laufendes Spiel. Ein laufendes Spiel nimmt niemand Neues auf.
  Zum Testen mit mehreren Spielern auf einem Rechner: ein privates Fenster oder ein anderer Browser.
- Jeder ist höchstens in einem Spiel. Verlassen gibt den Platz frei; ist niemand mehr da, wird das Spiel beendet.
- Code: `server/src/lobby.rs` (Lobby, Speichern), `server/src/game.rs` (Spiel-Thread, WebSocket eines Spiels).

## Accounts (PostgreSQL)

Optional kann man sich einen Account anlegen (Name und Passwort). Er bindet die Browser-Id an den Namen: Wer sich
auf einem anderen Gerät anmeldet, ist wieder derselbe Spieler. Ohne `DATABASE_URL` sind Accounts aus, alles andere
geht trotzdem. Passwörter werden mit Argon2 gehasht; die Tabellen legt der Server beim Start selbst an
(`server/migrations/`). Schnittstelle: siehe `server/src/accounts.rs`.

Lokal unter Windows:

1. PostgreSQL installieren (<https://www.postgresql.org/download/windows/>), Port 5432.
2. Mit `psql -U postgres` (oder pgAdmin):
   ```sql
   CREATE USER uk WITH PASSWORD 'dein-passwort';
   CREATE DATABASE untextured_kingdoms OWNER uk;
   ```
3. In `.env` im Backend-Ordner (der Server liest sie beim Start):
   `DATABASE_URL=postgres://uk:dein-passwort@localhost:5432/untextured_kingdoms`

In Docker läuft die Datenbank als Dienst `db` mit (Passwort `DB_PASSWORD` in `.env`).

## Protokoll ändern

Nachrichten werden nur in `protocol/src/lib.rs` definiert. Ablauf bei einer Änderung:

1. `protocol/src/lib.rs` anpassen
2. Im **Frontend**-Repo `npm run sync-protocol` ausführen. Das ruft hier `cargo test -p protocol` auf,
   generiert die TypeScript-Typen nach `protocol/bindings/` und kopiert sie ins Frontend nach `src/protocol/`.
3. In beiden Repos committen (hier `protocol/bindings/` gleich mit).

## Deployment mit Docker

Auf dem Server brauchst du **nur Docker und Git**, kein Rust und kein Node. Alles wird beim Bauen im
Container kompiliert. Die Schritt-für-Schritt-Anleitung für den VPS mit Apache steht in
[`deploy/ANLEITUNG.md`](deploy/ANLEITUNG.md).

Kurz:

```bash
cp .env.example .env          # DB_PASSWORD und ADMIN_USERS setzen
docker compose up -d --build  # Spiel läuft dann auf http://127.0.0.1:8081
./deploy/update.sh            # neue Version von dev holen und neu starten
```

- `server`: das Rust-Programm (schlankes Debian-Image), nur intern erreichbar. Beim Stoppen speichert es
  alle laufenden Spiele (Volume `game_data`).
- `db`: PostgreSQL für die Accounts (Volume `db_data`), nur auf `127.0.0.1:5433` (für DBeaver per SSH-Tunnel).
- `adminer`: die Datenbank im Browser ansehen (wie phpMyAdmin), nur auf `127.0.0.1:8082`, per SSH-Tunnel.
- `web`: aus dem Frontend-Repo gebaut. Caddy liefert den Client aus und reicht `/ws`, `/lobby` und `/api`
  an den Server weiter. Lauscht nur auf `127.0.0.1:8081`; HTTPS macht der Webserver davor
  (Apache-Beispiel: `deploy/apache-vhost.conf`).

```bash
docker compose ps               # Was läuft?
docker compose logs -f server   # Server-Logs live ansehen
docker compose down             # Alles stoppen (Daten bleiben in den Volumes)
```
