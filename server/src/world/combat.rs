//! Kampf (Grundgerüst) und Spielziel.
//!
//! Soldaten und König mit `attack` greifen von selbst die nächste feindliche
//! Kampfeinheit in Reichweite an, solange sie stehen. Fernkämpfer (Reichweite
//! über `army.melee_range`) können Einheiten mit `ranged_immune` (dem König)
//! nichts anhaben. Nahkampf geht nur auf derselben Ebene (Boden oder Wehrgang).
//!
//! Fällt ein König, scheidet sein Spieler aus: alles, was ihm gehört, verschwindet.
//! Bleibt nur noch ein Spieler oder ein Bündnis übrig, ist das Spiel entschieden.
//!
//! Noch nicht da: Trefferchance, Höhenvorteil, Deckung, Angriffsbefehl und
//! Verfolgen, Angriff auf Gebäude und Zivilisten.

use std::collections::HashMap;

use protocol::{ServerMsg, UnitKind};

use super::{Unit, World};

/// Kantenlänge der Suchzellen in Feldern.
const CELL: f32 = 8.0;
/// Halbe Körperbreite: so weit reicht ein Schlag über die Reichweite hinaus.
const REACH: f32 = 0.5;

/// Bündnis eines Spielers: (true, Bündnis) oder allein (false, Spieler).
type TeamKey = (bool, u32);

impl World {
    /// Lebenspunkte einer Einheit (0 = hat keine, z. B. Träger).
    pub(super) fn unit_hp(&self, u: &Unit) -> u32 {
        self.economy.unit(u.kind).map_or(0, |d| d.hp.saturating_sub(u.damage))
    }

    fn team(&self, player: u32) -> TeamKey {
        match self.teams.get(&player) {
            Some(&t) => (true, t),
            None => (false, player),
        }
    }

    /// Bündnisse setzen (Spieler → Bündnis-Nummer); wer fehlt, kämpft allein.
    /// Verbündete greifen sich nicht an und gewinnen gemeinsam.
    #[allow(dead_code)]
    pub fn set_teams(&mut self, teams: HashMap<u32, u32>) {
        self.teams = teams;
    }

    pub fn is_eliminated(&self, player: u32) -> bool {
        self.eliminated.contains(&player)
    }

    /// Ausgeschiedene Spieler.
    pub fn eliminated(&self) -> Vec<u32> {
        self.eliminated.iter().copied().collect()
    }

    /// Gewinner, wenn das Spiel entschieden ist.
    pub fn winners(&self) -> Option<&[u32]> {
        self.winners.as_deref()
    }

    pub(super) fn step_combat(&mut self, dt: f32) {
        let attack_seconds = self.economy.def.army.attack_seconds;
        // Wer kämpfen kann (hat Werte) und wer angreift (hat Schaden und Reichweite).
        let mut fighters = Vec::new();
        let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        let mut teams: Vec<TeamKey> = Vec::new();
        for (k, u) in self.units.iter_mut().enumerate() {
            let Some(def) = self.economy.unit(u.kind) else { continue };
            u.attack_cooldown = (u.attack_cooldown - dt).max(0.0);
            let team = match self.teams.get(&u.owner) {
                Some(&t) => (true, t),
                None => (false, u.owner),
            };
            if !teams.contains(&team) {
                teams.push(team);
            }
            grid.entry(((u.pos.0 / CELL) as i32, (u.pos.1 / CELL) as i32)).or_default().push(k);
            if def.attack > 0 && def.range > 0.0 && u.attack_cooldown <= 0.0 && u.path.is_empty() {
                fighters.push(k);
            }
        }
        if teams.len() < 2 {
            return;
        }
        let mut hits: Vec<(usize, u32, u32)> = Vec::new();
        for &k in &fighters {
            let u = &self.units[k];
            let def = self.economy.unit(u.kind).expect("oben geprüft");
            let ranged = self.economy.is_ranged(def.range);
            let reach = def.range + REACH;
            let team = self.team(u.owner);
            let (cx, cy) = ((u.pos.0 / CELL) as i32, (u.pos.1 / CELL) as i32);
            let r = (reach / CELL).ceil() as i32;
            let mut best: Option<(f32, usize)> = None;
            for gy in cy - r..=cy + r {
                for gx in cx - r..=cx + r {
                    for &j in grid.get(&(gx, gy)).map_or(&[][..], |v| v.as_slice()) {
                        let t = &self.units[j];
                        if self.team(t.owner) == team {
                            continue;
                        }
                        let Some(tdef) = self.economy.unit(t.kind) else { continue };
                        if (ranged && tdef.ranged_immune) || (!ranged && t.on_wall != u.on_wall) {
                            continue;
                        }
                        let d = ((t.pos.0 - u.pos.0).powi(2) + (t.pos.1 - u.pos.1).powi(2)).sqrt();
                        if d <= reach && best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, j));
                        }
                    }
                }
            }
            if let Some((_, j)) = best {
                hits.push((j, def.attack, u.owner));
                self.units[k].attack_cooldown = attack_seconds;
            }
        }
        if hits.is_empty() {
            return;
        }
        let mut killed_by: HashMap<usize, u32> = HashMap::new();
        for (j, attack, by) in hits {
            let max = self.economy.unit(self.units[j].kind).map_or(0, |d| d.hp);
            let u = &mut self.units[j];
            u.damage = u.damage.saturating_add(attack);
            if u.damage >= max {
                killed_by.entry(j).or_insert(by);
            }
        }
        let mut kings: Vec<(u32, u32)> = Vec::new();
        let mut dead: Vec<usize> = killed_by.keys().copied().collect();
        dead.sort_unstable_by(|a, b| b.cmp(a));
        for j in dead {
            let u = self.units.remove(j);
            if u.kind == UnitKind::King {
                kings.push((u.owner, killed_by[&j]));
            }
            self.dirty_stocks.insert(u.owner);
        }
        for (player, by) in kings {
            self.eliminate(player, Some(by));
        }
    }

    /// Spieler scheidet aus: alles von ihm verschwindet, danach prüfen, ob jemand gewonnen hat.
    pub fn eliminate(&mut self, player: u32, by: Option<u32>) {
        if !self.eliminated.insert(player) {
            return;
        }
        for building_id in self.remove_player(player) {
            self.events.push(ServerMsg::BuildingRemoved { building_id });
        }
        self.pay_timer.remove(&player);
        self.unpaid.remove(&player);
        self.replan_all();
        self.events.push(ServerMsg::PlayerDefeated { player, by });
        self.check_victory();
    }

    /// Nur noch ein Bündnis (oder ein Spieler) übrig: Spiel entschieden.
    fn check_victory(&mut self) {
        if self.winners.is_some() {
            return;
        }
        let alive = self.players();
        let mut teams: Vec<TeamKey> = alive.iter().map(|&p| self.team(p)).collect();
        teams.dedup();
        teams.sort();
        teams.dedup();
        if teams.len() <= 1 && !alive.is_empty() {
            self.winners = Some(alive.clone());
            self.events.push(ServerMsg::GameOver { winners: alive });
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, ServerMsg, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::{Unit, World}};

    /// Zwei Spieler mit Bergfried; ohne Bewohner, damit nichts Anderes dazwischenfunkt.
    fn world() -> World {
        let map = GameMap::parse(crate::map::DEFAULT_MAP).unwrap();
        let mut w = World::new(map, Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people());
        w.spawn_player(1);
        w.spawn_player(2);
        w.take_events();
        w
    }

    fn king(w: &World, p: u32) -> Option<&Unit> {
        w.units.iter().find(|u| u.owner == p && u.kind == UnitKind::King)
    }

    /// Einheit von `owner` direkt neben den König von `of` stellen.
    fn next_to_king(w: &mut World, owner: u32, kind: UnitKind, of: u32, dx: f32) -> u32 {
        let pos = king(w, of).unwrap().pos;
        let id = 10_000 + w.units.len() as u32;
        w.units.push(Unit::new(id, owner, kind, (pos.0 + dx, pos.1)));
        id
    }

    fn run(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 20.0) as u32 {
            w.step_combat(0.05);
        }
    }

    #[test]
    fn bogenschuetzen_koennen_den_koenig_nicht_verletzen() {
        let mut w = world();
        next_to_king(&mut w, 1, UnitKind::Archer, 2, 3.0);
        run(&mut w, 60.0);
        let k = king(&w, 2).expect("König lebt noch");
        assert_eq!(k.damage, 0);
        assert!(!w.is_eliminated(2));
    }

    #[test]
    fn nahkaempfer_toeten_den_koenig_und_der_gegner_verschwindet() {
        let mut w = world();
        assert!(w.buildings.iter().any(|b| b.owner == 2 && b.kind == BuildingKind::Keep));
        for i in 0..4 {
            next_to_king(&mut w, 1, UnitKind::Swordsman, 2, if i % 2 == 0 { 1.0 } else { -1.0 });
        }
        run(&mut w, 120.0);
        assert!(king(&w, 2).is_none(), "König 2 ist tot");
        assert!(w.is_eliminated(2));
        assert!(!w.units.iter().any(|u| u.owner == 2), "keine Einheiten von 2 mehr");
        assert!(!w.buildings.iter().any(|b| b.owner == 2), "keine Gebäude von 2 mehr");
        assert!(w.has_player(2), "ausgeschieden zählt als bekannt (kein neuer König beim Verbinden)");
        let events = w.take_events();
        assert!(events.iter().any(|e| matches!(e, ServerMsg::PlayerDefeated { player: 2, by: Some(1) })));
        assert!(events.iter().any(|e| matches!(e, ServerMsg::BuildingRemoved { .. })));
        assert!(events.iter().any(|e| matches!(e, ServerMsg::GameOver { winners } if winners == &vec![1])));
        assert_eq!(w.winners(), Some(&[1][..]));
        assert!(king(&w, 1).is_some());
    }

    #[test]
    fn koenig_wehrt_sich_im_nahkampf() {
        let mut w = world();
        let id = next_to_king(&mut w, 1, UnitKind::Spearman, 2, 1.0);
        run(&mut w, 60.0);
        // Ein Speerträger (60 LP) gegen den König (300 LP, 14 Schaden): der Speerträger fällt.
        assert!(!w.units.iter().any(|u| u.id == id));
        assert!(king(&w, 2).is_some_and(|k| k.damage > 0));
    }

    #[test]
    fn verbuendete_greifen_sich_nicht_an_und_gewinnen_zusammen() {
        let mut w = world();
        w.spawn_player(3);
        w.set_teams([(1, 7), (2, 7)].into_iter().collect());
        next_to_king(&mut w, 1, UnitKind::Knight, 2, 1.0);
        run(&mut w, 30.0);
        assert_eq!(king(&w, 2).unwrap().damage, 0, "Verbündete kämpfen nicht");
        // Spieler 3 fällt: Bündnis 1+2 gewinnt.
        w.take_events();
        w.eliminate(3, Some(1));
        assert_eq!(w.winners(), Some(&[1, 2][..]));
    }

    #[test]
    fn ausgeschieden_bleibt_nach_dem_laden() {
        let mut w = world();
        w.eliminate(2, Some(1));
        let save = w.to_save("test");
        let json = serde_json::to_string(&save).unwrap();
        let save = serde_json::from_str(&json).unwrap();
        let map = GameMap::parse(crate::map::DEFAULT_MAP).unwrap();
        let l = World::from_save(map, Economy::parse(crate::economy::ECONOMY_JSON).unwrap(), save).unwrap();
        assert!(l.is_eliminated(2));
        assert!(l.has_player(2));
        assert_eq!(l.winners(), Some(&[1][..]));
    }
}
