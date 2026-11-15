-- Spieler-Accounts. Der Token ist dieselbe geheime Spieler-Id, die der Browser
-- im localStorage hält: Wer sich anmeldet, bekommt ihn zurück und ist damit
-- wieder derselbe Spieler (Lobby, laufende Spiele).
CREATE TABLE accounts (
    id            BIGSERIAL PRIMARY KEY,
    username      TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    player_token  TEXT NOT NULL UNIQUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login    TIMESTAMPTZ
);
-- Benutzername ohne Rücksicht auf Groß-/Kleinschreibung eindeutig.
CREATE UNIQUE INDEX accounts_username_lower ON accounts (lower(username));
