-- Admin-Rechte hängen am Account. Setzen per SQL:
--   UPDATE accounts SET is_admin = true WHERE lower(username) = lower('Jan');
-- oder über ADMIN_USERS in der Umgebung (siehe server/src/admin.rs).
ALTER TABLE accounts ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT false;
