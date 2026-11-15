-- Wer den Karteneditor benutzen (Karten speichern) darf. Admins dürfen es immer.
ALTER TABLE accounts ADD COLUMN can_edit_maps BOOLEAN NOT NULL DEFAULT false;
