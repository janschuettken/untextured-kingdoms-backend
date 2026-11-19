-- "Passwort vergessen": Einmal-Links per E-Mail. Gespeichert wird nur ein Hash des Links.
CREATE TABLE password_resets (
    token_hash  TEXT PRIMARY KEY,
    account_id  BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);
CREATE INDEX password_resets_account ON password_resets (account_id);
