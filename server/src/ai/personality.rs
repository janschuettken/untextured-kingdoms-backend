//! Charaktere der Computergegner: was er lieber baut und ausbildet, wie gern er angreift.
//!
//! Daten in `server/data/ai_personalities.json` (Editor `/ai.html`, Schnittstelle in
//! `admin.rs`). Die Stärke (`AiLevel`) ist davon getrennt: sie bestimmt Tempo, Bauliste und
//! Mauer; der Charakter verschiebt Gewichte darauf (siehe `Profile::build`).

use std::collections::HashSet;

pub use protocol::{AiPersonalities, AiPersonality};

/// Die eingebauten Charaktere (aus dem Repo).
pub const PERSONALITIES_JSON: &str = include_str!("../../data/ai_personalities.json");

/// Höchster Prozentwert, den der Editor erlaubt.
const MAX_PERCENT: u32 = 1000;

/// Charaktere prüfen und laden.
pub fn parse(json: &str) -> Result<AiPersonalities, String> {
    let all: AiPersonalities = serde_json::from_str(json).map_err(|e| e.to_string())?;
    validate(&all)?;
    Ok(all)
}

fn validate(all: &AiPersonalities) -> Result<(), String> {
    if all.personalities.is_empty() {
        return Err("mindestens ein Charakter".into());
    }
    let mut ids = HashSet::new();
    for p in &all.personalities {
        let ok = (1..=32).contains(&p.id.len()) && p.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            return Err(format!("Id „{}“: 1 bis 32 Zeichen, nur a-z, 0-9 und _", p.id));
        }
        if !ids.insert(p.id.as_str()) {
            return Err(format!("Id „{}“ doppelt", p.id));
        }
        if p.aggression > MAX_PERCENT || p.army > MAX_PERCENT {
            return Err(format!("{}: Prozent höchstens {MAX_PERCENT}", p.id));
        }
        if p.daring > 100 || p.helpfulness > 100 {
            return Err(format!("{}: Regler gehen von 0 bis 100", p.id));
        }
        if let Some((k, _)) = p.buildings.iter().find(|(_, &v)| v > MAX_PERCENT) {
            return Err(format!("{}: {k:?} höchstens {MAX_PERCENT} %", p.id));
        }
        if let Some((k, _)) = p.units.iter().find(|(k, &v)| v > MAX_PERCENT || !k.is_soldier()) {
            return Err(format!("{}: {k:?} ist kein Soldat oder über {MAX_PERCENT} %", p.id));
        }
    }
    Ok(())
}

/// Laden aus der Datei; fehlt sie, gelten die eingebauten.
pub fn load(path: &std::path::Path) -> Result<AiPersonalities, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => parse(PERSONALITIES_JSON),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Als Datei (eingerückt, wie im Repo).
pub fn to_json(all: &AiPersonalities) -> String {
    let mut text = serde_json::to_string_pretty(all).expect("Charaktere lassen sich schreiben");
    text.push('\n');
    text
}

/// Der Charakter zu dieser Id, sonst der erste.
pub fn find<'a>(all: &'a AiPersonalities, id: &str) -> &'a AiPersonality {
    all.personalities.iter().find(|p| p.id == id).unwrap_or(&all.personalities[0])
}

/// Neutral: alles wie die Stufe (für Tests).
#[cfg(test)]
pub fn neutral() -> AiPersonality {
    AiPersonality {
        id: "balanced".into(),
        name: Default::default(),
        description: Default::default(),
        aggression: 100,
        army: 100,
        daring: 30,
        helpfulness: 50,
        buildings: Default::default(),
        units: Default::default(),
        castle: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eingebaute_charaktere_sind_gueltig() {
        let all = parse(PERSONALITIES_JSON).unwrap();
        assert!(all.personalities.len() >= 3);
        assert_eq!(find(&all, "gibt_es_nicht").id, all.personalities[0].id);
        // Hin und zurück ohne Verlust.
        assert_eq!(parse(&to_json(&all)).unwrap(), all);
    }

    #[test]
    fn falsche_charaktere_werden_abgelehnt() {
        assert!(parse(r#"{"personalities": []}"#).is_err());
        assert!(parse(r#"{"personalities": [{"id": "A"}]}"#).is_err());
        assert!(parse(r#"{"personalities": [{"id": "a"}, {"id": "a"}]}"#).is_err());
        assert!(parse(r#"{"personalities": [{"id": "a", "units": {"carrier": 100}}]}"#).is_err());
        let one = parse(r#"{"personalities": [{"id": "a"}]}"#).unwrap();
        assert_eq!(one.personalities[0].aggression, 100);
    }
}
