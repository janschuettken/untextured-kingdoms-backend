-- Profil: E-Mail (freiwillig) und Profilbild (kleines Bild als data:-URL, nur für die Anzeige).
ALTER TABLE accounts ADD COLUMN email TEXT;
ALTER TABLE accounts ADD COLUMN avatar TEXT;
CREATE UNIQUE INDEX accounts_email_lower ON accounts (lower(email)) WHERE email IS NOT NULL;
