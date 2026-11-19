-- Gäste: wer die Seite betritt und nur einen Namen eingibt (kein Passwort).
-- Der Token ist die geheime Spieler-Id aus dem localStorage. So sieht der Admin
-- auch Spieler ohne Account (Nutzerliste: "kein Passwort gesetzt").
CREATE TABLE guests (
    id            BIGSERIAL PRIMARY KEY,
    player_token  TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen     TIMESTAMPTZ NOT NULL DEFAULT now()
);
