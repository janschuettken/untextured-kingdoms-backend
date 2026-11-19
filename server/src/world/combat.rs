//! Kampf (Grundgerüst) und Spielziel.
//!
//! Soldaten und König mit `attack` greifen von selbst die nächste feindliche
//! Einheit in Reichweite an (auch Zivilisten), solange sie stehen. Mit einem
//! Angriffsbefehl (`order_attack`) verfolgen sie ein Ziel, bis es fällt; Nahkämpfer
//! reißen so auch Gebäude, Mauern und Tore ein. Fernkämpfer (Reichweite
//! über `army.melee_range`) können Einheiten mit `ranged_immune` (dem König)
//! nichts anhaben. Nahkampf geht nur auf derselben Ebene (Boden oder Wehrgang).
//!
//! Fällt ein König, scheidet sein Spieler aus: alles, was ihm gehört, verschwindet.
//! Bleibt nur noch ein Spieler oder ein Bündnis übrig, ist das Spiel entschieden.
//!
//! Deckung: wer oben auf einem Gebäude steht (Mauer, Turm, Tor …), wird von Fernkämpfern
//! seltener getroffen – `cover_percent` der Gebäudestufe sagt, wie viel Prozent der Schüsse
//! danebengehen. Der Schuss fliegt trotzdem (Pfeil sichtbar), richtet aber nichts an.
//!
//! Noch nicht da: Höhenvorteil.

use std::collections::HashMap;

use protocol::{AttackTarget, BuildingKind, ServerMsg, Shot, UnitKind};

use super::{Unit, World};

/// Kantenlänge der Suchzellen in Feldern.
const CELL: f32 = 8.0;
/// Halbe Körperbreite: so weit reicht ein Schlag über die Reichweite hinaus.
const REACH: f32 = 0.5;
/// Beim Verfolgen: so oft (Sekunden) wird der Weg zum Ziel neu gesucht.
const CHASE_SECONDS: f32 = 1.0;

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

    /// Angriffsbefehl: wer dem Ziel schaden kann, verfolgt und greift es an; die
    /// anderen (Fernkämpfer gegen König oder Gebäude, Verbündete) laufen nur hin.
    pub fn order_attack(&mut self, player: u32, unit_ids: &[u32], target: AttackTarget) -> bool {
        let Some((owner, pos)) = self.attack_target_info(target) else { return false };
        let mut movers = Vec::new();
        for k in 0..self.units.len() {
            let u = &self.units[k];
            if u.owner != player || u.is_civilian() || !unit_ids.contains(&u.id) {
                continue;
            }
            if self.can_attack(player, owner) && self.can_hurt(u, target) {
                let u = &mut self.units[k];
                u.attack_order = Some(target);
                u.chase_timer = 0.0;
                u.speed_cap = None;
            } else {
                movers.push(u.id);
            }
        }
        if !movers.is_empty() {
            self.order_move(player, &movers, pos);
        }
        true
    }

    /// Besitzer und Lage eines Angriffsziels (Gebäude: Mitte).
    fn attack_target_info(&self, target: AttackTarget) -> Option<(u32, (f32, f32))> {
        match target {
            AttackTarget::Unit { id } => self.units.iter().find(|u| u.id == id).map(|u| (u.owner, u.pos)),
            AttackTarget::Building { id } => {
                let b = self.buildings.iter().find(|b| b.id == id && b.hp > 0)?;
                Some((b.owner, (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)))
            }
        }
    }

    /// Kann diese Einheit dem Ziel überhaupt schaden? Fernkämpfer nichts gegen `ranged_immune`
    /// (den König) und nichts gegen Gebäude; Zivilisten nie.
    fn can_hurt(&self, u: &Unit, target: AttackTarget) -> bool {
        let Some(def) = self.economy.unit(u.kind) else { return false };
        if def.attack == 0 || def.range <= 0.0 {
            return false;
        }
        let ranged = self.economy.is_ranged(def.range);
        match target {
            AttackTarget::Building { .. } => !ranged,
            AttackTarget::Unit { id } => self
                .units
                .iter()
                .find(|t| t.id == id)
                .and_then(|t| self.economy.unit(t.kind))
                .is_some_and(|t| !(ranged && t.ranged_immune)),
        }
    }

    /// Deckung (Prozent) für eine Einheit: oben auf einem Gebäude dessen `cover_percent`, sonst 0.
    fn cover_of(&self, t: &Unit) -> u32 {
        if !t.on_wall {
            return 0;
        }
        let Some(tile) = self.tile_of(t.pos) else { return 0 };
        let id = self.occupied[tile];
        self.buildings
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| self.economy.building(b.kind).map(|d| d.at(b.level).cover_percent))
            .unwrap_or(0)
            .min(100)
    }

    /// Geht ein Schuss wegen Deckung daneben? (`percent` von 100)
    fn misses(&mut self, percent: u32) -> bool {
        if percent == 0 {
            return false;
        }
        // xorshift64*: schnell, für Treffer genügt das.
        let mut x = self.combat_rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.combat_rng = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) % 100 < percent as u64
    }

    pub(super) fn step_combat(&mut self, dt: f32) {
        // Wer kämpfen kann (hat Werte) und wer von selbst angreift (hat Schaden und Reichweite).
        let mut fighters = Vec::new();
        let mut ordered = Vec::new();
        let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        let mut teams: Vec<TeamKey> = Vec::new();
        let mut index: HashMap<u32, usize> = HashMap::new();
        for (k, u) in self.units.iter_mut().enumerate() {
            index.insert(u.id, k);
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
            if u.attack_order.is_some() {
                ordered.push(k);
            } else if def.attack > 0 && def.range > 0.0 && u.attack_cooldown <= 0.0 && u.path.is_empty() {
                fighters.push(k);
            }
        }
        if teams.len() < 2 && ordered.is_empty() {
            return;
        }
        // (Ziel, Schaden, Angreifer, Fernkampf)
        let mut hits: Vec<(usize, u32, u32, bool)> = Vec::new();
        let mut building_hits: Vec<(u32, u32)> = Vec::new();
        let mut shots: Vec<Shot> = Vec::new();
        // Angriffsbefehle: hingehen, verfolgen, zuschlagen.
        for k in ordered {
            let u = &self.units[k];
            let Some(order) = u.attack_order else { continue };
            let Some(def) = self.economy.unit(u.kind) else { continue };
            let ranged = self.economy.is_ranged(def.range);
            let reach = def.range + REACH;
            // Unterwegs zu einem Gebäude: feindliche Kämpfer in Reichweite gehen vor. Erst die
            // abwehren, dann weiter zum Gebäude.
            if matches!(order, AttackTarget::Building { .. }) {
                if let Some((j, _)) = self.nearest_foe(k, &grid, true) {
                    let (attack, by) = (def.attack, u.owner);
                    let u = &mut self.units[k];
                    u.path.clear();
                    u.layers.clear();
                    u.target = Some(u.pos);
                    u.chase_timer = 0.0;
                    if u.attack_cooldown <= 0.0 {
                        u.attack_cooldown = def.attack_seconds;
                        hits.push((j, attack, by, ranged));
                    }
                    continue;
                }
            }
            // Wo steht das Ziel, wie weit ist es, und kann man es von hier treffen?
            let found = match order {
                AttackTarget::Unit { id } => index.get(&id).map(|&j| {
                    let t = &self.units[j];
                    let d = (t.pos.0 - u.pos.0).hypot(t.pos.1 - u.pos.1);
                    // Fernkampf: nur wenn Wurfbahn bzw. Sichtlinie frei ist, sonst näher heran.
                    let flight = if ranged && d <= reach { self.flight(u, t, def.range) } else { None };
                    let ok = if ranged { flight.is_some() } else { t.on_wall == u.on_wall };
                    (t.owner, t.pos, t.on_wall, d, ok, Some((j, flight)))
                }),
                AttackTarget::Building { id } => self.buildings.iter().find(|b| b.id == id && b.hp > 0).map(|b| {
                    let dx = (b.x as f32 - u.pos.0).max(u.pos.0 - (b.x + b.w) as f32).max(0.0);
                    let dy = (b.y as f32 - u.pos.1).max(u.pos.1 - (b.y + b.h) as f32).max(0.0);
                    let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
                    (b.owner, self.foot_of(center, u.pos), false, dx.hypot(dy), true, None)
                }),
            };
            let Some((owner, pos, on_wall, dist, level_ok, j)) = found.filter(|f| self.can_attack(u.owner, f.0) && self.can_hurt(u, order)) else {
                // Ziel gefallen, abgerissen oder nicht (mehr) angreifbar: stehen bleiben.
                let u = &mut self.units[k];
                u.attack_order = None;
                u.path.clear();
                u.layers.clear();
                u.target = Some(u.pos);
                continue;
            };
            if dist <= reach && level_ok {
                let (attack, by) = (def.attack, u.owner);
                let u = &mut self.units[k];
                // Fernkämpfer schießen nur im Stand: wer gerade noch lief, hält erst an.
                let was_moving = !u.path.is_empty();
                u.path.clear();
                u.layers.clear();
                u.target = Some(u.pos);
                if ranged && was_moving {
                    continue;
                }
                if u.attack_cooldown <= 0.0 {
                    u.attack_cooldown = def.attack_seconds;
                    match (order, j) {
                        (AttackTarget::Unit { .. }, Some((j, flight))) => {
                            hits.push((j, attack, by, ranged));
                            if let Some(f) = flight {
                                shots.push(self.make_shot(&self.units[k], &self.units[j], f, def.range));
                            }
                        }
                        (AttackTarget::Building { id }, _) => building_hits.push((id, attack)),
                        _ => {}
                    }
                }
                continue;
            }
            let _ = owner;
            // Verfolgen: ab und zu den Weg zum Ziel neu suchen.
            let timer = self.units[k].chase_timer - dt;
            if timer > 0.0 && !self.units[k].path.is_empty() {
                self.units[k].chase_timer = timer;
                continue;
            }
            let (from, from_wall, me) = (u.pos, u.on_wall, u.owner);
            let (path, layers) = self.plan(me, from, from_wall, pos, on_wall);
            let u = &mut self.units[k];
            u.chase_timer = CHASE_SECONDS;
            u.path = path;
            u.layers = layers;
            u.target = Some(pos);
            u.target_wall = on_wall;
        }
        // Von selbst: die nächste feindliche Einheit in Reichweite, Kämpfer vor Zivilisten.
        for &k in &fighters {
            let u = &self.units[k];
            let def = self.economy.unit(u.kind).expect("oben geprüft");
            if let Some((j, flight)) = self.nearest_foe(k, &grid, false) {
                hits.push((j, def.attack, u.owner, self.economy.is_ranged(def.range)));
                if let Some(f) = flight {
                    shots.push(self.make_shot(u, &self.units[j], f, def.range));
                }
                self.units[k].attack_cooldown = def.attack_seconds;
            }
        }
        if !shots.is_empty() {
            self.events.push(ServerMsg::Shots { shots });
        }
        // Gebäude: Schaden, bei 0 eingerissen.
        for (id, attack) in building_hits {
            let Some(b) = self.buildings.iter_mut().find(|b| b.id == id) else { continue };
            b.hp = b.hp.saturating_sub(attack);
            // Der Bergfried bleibt als Ruine stehen: nur die Tür ist weg, jeder kann hinauf.
            if b.hp == 0 && b.kind == BuildingKind::Keep {
                self.dirty_buildings.insert(id);
                self.replan_all();
            } else if b.hp == 0 {
                self.destroy_building(id);
            } else {
                self.dirty_buildings.insert(id);
            }
        }
        // Deckung: Schüsse auf Einheiten oben auf Gebäuden gehen teils daneben.
        let mut landed = Vec::with_capacity(hits.len());
        for (j, attack, by, ranged) in hits {
            let cover = if ranged { self.cover_of(&self.units[j]) } else { 0 };
            if !self.misses(cover) {
                landed.push((j, attack, by));
            }
        }
        if landed.is_empty() {
            return;
        }
        let mut killed_by: HashMap<usize, u32> = HashMap::new();
        for (j, attack, by) in landed {
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
        let mut died = Vec::new();
        for j in dead {
            let u = self.units.remove(j);
            died.push(u.id);
            if u.kind == UnitKind::King {
                kings.push((u.owner, killed_by[&j]));
            }
            self.dirty_stocks.insert(u.owner);
        }
        self.events.push(ServerMsg::UnitsDied { unit_ids: died });
        let kings_empty = kings.is_empty();
        for (player, by) in kings {
            self.knock_out(player, Some(by));
        }
        // Erst alle ausscheiden lassen: fallen beide letzten Könige im selben Takt, ist es unentschieden.
        if !kings_empty {
            self.check_victory();
        }
    }

    /// Nächste feindliche Einheit, die Einheit `k` von hier treffen kann. Kämpfer (Soldaten,
    /// König) gehen vor Zivilisten; `only_fighters`: nur Kämpfer.
    /// Fernkämpfer: nur wen Wurfbahn bzw. Sichtlinie erreicht (mit der Bahn dazu).
    fn nearest_foe(&self, k: usize, grid: &HashMap<(i32, i32), Vec<usize>>, only_fighters: bool) -> Option<(usize, Option<super::ballistics::Flight>)> {
        let u = &self.units[k];
        let def = self.economy.unit(u.kind)?;
        let ranged = self.economy.is_ranged(def.range);
        let reach = def.range + REACH;
        let team = self.team(u.owner);
        let (cx, cy) = ((u.pos.0 / CELL) as i32, (u.pos.1 / CELL) as i32);
        let r = (reach / CELL).ceil() as i32;
        // (Zivilist?, Abstand): kleinster gewinnt.
        let mut near: Vec<((bool, f32), usize)> = Vec::new();
        for gy in cy - r..=cy + r {
            for gx in cx - r..=cx + r {
                for &j in grid.get(&(gx, gy)).map_or(&[][..], |v| v.as_slice()) {
                    let t = &self.units[j];
                    if self.team(t.owner) == team {
                        continue;
                    }
                    let Some(tdef) = self.economy.unit(t.kind) else { continue };
                    let civilian = t.kind.is_civilian() || tdef.attack == 0;
                    if civilian && only_fighters {
                        continue;
                    }
                    // Wer drinnen arbeitet oder wartet, ist nicht zu erreichen.
                    if t.kind.is_civilian() && self.inside(t) {
                        continue;
                    }
                    if (ranged && tdef.ranged_immune) || (!ranged && t.on_wall != u.on_wall) {
                        continue;
                    }
                    let d = (t.pos.0 - u.pos.0).hypot(t.pos.1 - u.pos.1);
                    let key = (civilian, d);
                    if d <= reach {
                        near.push((key, j));
                    }
                }
            }
        }
        near.sort_by(|a, b| a.0.0.cmp(&b.0.0).then(a.0.1.total_cmp(&b.0.1)));
        near.into_iter().find_map(|(_, j)| {
            if !ranged {
                return Some((j, None));
            }
            self.flight(u, &self.units[j], def.range).map(|f| (j, Some(f)))
        })
    }

    /// Steht ein feindlicher Soldat oder König im Umkreis `army.enemy_block_radius`?
    /// Dann darf hier nicht gebaut, ausgebaut oder repariert werden.
    pub(super) fn check_no_enemy(&self, player: u32, at: (f32, f32)) -> Result<(), protocol::Text> {
        let radius = self.economy.def.army.enemy_block_radius;
        if radius <= 0.0 {
            return Ok(());
        }
        let near = self.units.iter().any(|u| {
            self.economy.unit(u.kind).is_some_and(|d| d.attack > 0)
                && self.can_attack(player, u.owner)
                && (u.pos.0 - at.0).hypot(u.pos.1 - at.1) <= radius
        });
        if near {
            return Err(protocol::Text::new("error.enemy_near").arg("radius", radius));
        }
        Ok(())
    }

    /// Beschädigtes Gebäude reparieren: kostet den fehlenden Anteil aller Bau- und Ausbaukosten
    /// (aufgerundet), danach ist es wieder heil.
    pub fn repair(&mut self, player: u32, id: u32) -> Result<protocol::BuildingState, protocol::Text> {
        let b = self.buildings.iter().find(|b| b.id == id).ok_or_else(|| protocol::Text::new("error.no_building"))?;
        if b.owner != player {
            return Err(protocol::Text::new("error.not_yours"));
        }
        let def = self.economy.building(b.kind).ok_or_else(|| protocol::Text::new("error.unknown_building"))?;
        let max = def.at(b.level).max_hp.max(1);
        if b.hp >= max {
            return Err(protocol::Text::new("error.not_damaged"));
        }
        let cost = self.repair_cost(b);
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        self.check_no_enemy(player, center)?;
        self.pay(player, &cost)?;
        let b = self.buildings.iter_mut().find(|b| b.id == id).expect("eben gefunden");
        let was_ruin = b.hp == 0;
        b.hp = max;
        let b = b.clone();
        if was_ruin {
            // Tür wieder drin: Feinde kommen nicht mehr hinauf.
            self.replan_all();
        }
        Ok(b)
    }

    /// Was die Reparatur kostet (fehlender Anteil der Kosten bis zur jetzigen Stufe).
    pub fn repair_cost(&self, b: &protocol::BuildingState) -> Vec<protocol::GoodAmount> {
        let Some(def) = self.economy.building(b.kind) else { return Vec::new() };
        let max = def.at(b.level).max_hp.max(1);
        let missing = max.saturating_sub(b.hp.min(max));
        super::level_cost(def, b.level.min(255) as u8)
            .into_iter()
            .map(|c| protocol::GoodAmount { good: c.good, amount: (c.amount * missing).div_ceil(max) })
            .filter(|c| c.amount > 0)
            .collect()
    }

    /// Schuss für den Client: gleiche Bahn wie auf dem Server gerechnet.
    fn make_shot(&self, s: &Unit, t: &Unit, flight: super::ballistics::Flight, range: f32) -> Shot {
        Shot {
            from: s.pos,
            to: t.pos,
            from_wall: s.on_wall,
            to_wall: t.on_wall,
            kind: s.kind,
            from_h: self.feet_h(s) + 0.7,
            to_h: self.feet_h(t) + 0.7,
            arc: match flight {
                super::ballistics::Flight::Arc(tan) => Some((tan, range)),
                super::ballistics::Flight::Straight => None,
            },
        }
    }

    /// Gebäude im Kampf eingerissen: verschwindet wie beim Abriss, nur ohne Erstattung.
    fn destroy_building(&mut self, id: u32) {
        let Some(b) = self.buildings.iter().find(|b| b.id == id).cloned() else { return };
        self.remove_building(id);
        if self.economy.building(b.kind).is_some_and(|d| d.hub) {
            self.update_capacity(b.owner);
        }
        self.events.push(ServerMsg::BuildingRemoved { building_id: id });
        self.replan_all();
    }

    /// Spieler scheidet aus: alles von ihm verschwindet, danach prüfen, ob jemand gewonnen hat.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn eliminate(&mut self, player: u32, by: Option<u32>) {
        self.knock_out(player, by);
        self.check_victory();
    }

    /// Spieler scheidet aus, ohne schon den Sieg zu prüfen.
    fn knock_out(&mut self, player: u32, by: Option<u32>) {
        if !self.eliminated.insert(player) {
            return;
        }
        for building_id in self.remove_player(player) {
            self.events.push(ServerMsg::BuildingRemoved { building_id });
        }
        self.pay_timer.remove(&player);
        self.unpaid.remove(&player);
        self.army_hungry.remove(&player);
        self.replan_all();
        self.events.push(ServerMsg::PlayerDefeated { player, by });
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
        // Niemand mehr übrig (alle letzten Könige zugleich gefallen): unentschieden, `winners` leer.
        if teams.len() <= 1 {
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
        let mut eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        // Feste Kampfwerte, damit die Tests nicht vom Balancing abhängen.
        for u in &mut eco.def.units {
            let (hp, attack, range, immune) = match u.kind {
                UnitKind::King => (300, 14, 1.2, true),
                UnitKind::Spearman => (60, 8, 1.5, false),
                UnitKind::Archer => (40, 6, 18.0, false),
                UnitKind::Swordsman => (110, 12, 1.0, false),
                UnitKind::Knight => (140, 16, 1.0, false),
                _ => continue,
            };
            (u.hp, u.attack, u.range, u.ranged_immune, u.attack_seconds) = (hp, attack, range, immune, 1.5);
        }
        let mut w = World::new(map, eco);
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
    fn fallen_beide_koenige_zugleich_ist_es_unentschieden() {
        let mut w = world();
        for p in [1, 2] {
            let k = w.units.iter_mut().find(|u| u.owner == p && u.kind == UnitKind::King).unwrap();
            k.damage = 299;
        }
        let pos = king(&w, 1).unwrap().pos;
        w.units.iter_mut().find(|u| u.owner == 2 && u.kind == UnitKind::King).unwrap().pos = (pos.0 + 1.0, pos.1);
        run(&mut w, 2.0);
        assert!(w.is_eliminated(1) && w.is_eliminated(2));
        assert_eq!(w.winners(), Some(&[][..]));
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
    fn soldaten_greifen_feindliche_zivilisten_an_aber_nicht_verbuendete() {
        let mut w = world();
        let pos = king(&w, 2).unwrap().pos;
        w.units.push(Unit::new(20_001, 2, UnitKind::Carrier, (pos.0 + 4.0, pos.1)));
        w.units.push(Unit::new(20_002, 1, UnitKind::Archer, (pos.0 + 8.0, pos.1)));
        // Ohne König daneben (Kämpfer gehen vor).
        w.units.retain(|u| u.kind != UnitKind::King);
        run(&mut w, 30.0);
        assert!(!w.units.iter().any(|u| u.id == 20_001), "feindlicher Träger ist gefallen");

        let mut w = world();
        w.set_teams([(1, 7), (2, 7)].into_iter().collect());
        let pos = king(&w, 2).unwrap().pos;
        w.units.push(Unit::new(20_001, 2, UnitKind::Carrier, (pos.0 + 4.0, pos.1)));
        w.units.push(Unit::new(20_002, 1, UnitKind::Archer, (pos.0 + 8.0, pos.1)));
        run(&mut w, 30.0);
        assert!(w.units.iter().any(|u| u.id == 20_001 && u.damage == 0), "Verbündete bleiben verschont");
    }

    /// Ganze Schritte (mit Laufen), nicht nur der Kampf.
    fn steps(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 20.0) as u32 {
            w.step(0.05);
        }
    }

    #[test]
    fn angriffsbefehl_verfolgt_das_ziel() {
        let mut w = world();
        let k2 = king(&w, 2).unwrap().id;
        let pos = king(&w, 2).unwrap().pos;
        // Vor dem Bergfried (Süden), nicht in ihm.
        let ids: Vec<u32> = (0..4)
            .map(|i| {
                let id = 30_000 + i;
                w.units.push(Unit::new(id, 1, UnitKind::Knight, (pos.0 - 1.5 + i as f32, pos.1 + 6.0)));
                id
            })
            .collect();
        assert!(w.order_attack(1, &ids, protocol::AttackTarget::Unit { id: k2 }));
        // Der König läuft weg, die Ritter hinterher.
        w.order_move(2, &[k2], (pos.0, pos.1 + 8.0));
        steps(&mut w, 90.0);
        assert!(w.is_eliminated(2), "König eingeholt und getötet");
    }

    #[test]
    fn bogenschuetze_laeuft_nur_hin_bei_koenig_und_schiesst_auf_soldaten() {
        let mut w = world();
        let k2 = king(&w, 2).unwrap().id;
        let a = next_to_king(&mut w, 1, UnitKind::Archer, 2, 10.0);
        w.order_attack(1, &[a], protocol::AttackTarget::Unit { id: k2 });
        assert!(w.units.iter().find(|u| u.id == a).unwrap().attack_order.is_none(), "kein Angriff auf den König");
        let s = next_to_king(&mut w, 2, UnitKind::Spearman, 2, 3.0);
        let a2 = next_to_king(&mut w, 1, UnitKind::Archer, 2, 12.0);
        w.order_attack(1, &[a2], protocol::AttackTarget::Unit { id: s });
        w.take_events();
        steps(&mut w, 3.0);
        assert!(w.take_events().iter().any(|e| matches!(e, ServerMsg::Shots { .. })), "Pfeile fliegen");
    }

    /// Schaden, den ein Speerträger oben auf dem Bergfried in `seconds` von einem Bogenschützen
    /// bekommt, wenn der Bergfried `cover` Prozent Deckung gibt und der Bogenschütze `rate`-mal
    /// so schnell schießt wie laut Daten. Der Speerträger hält dabei beliebig viel aus.
    fn damage_on_keep(cover: u32, rate: f32, seconds: f32) -> u32 {
        let mut w = world();
        let mut eco = w.economy.clone();
        for l in &mut eco.def.buildings.iter_mut().find(|d| d.kind == BuildingKind::Keep).unwrap().levels {
            l.cover_percent = cover;
        }
        eco.def.units.iter_mut().find(|u| u.kind == UnitKind::Spearman).unwrap().hp = u32::MAX;
        eco.def.units.iter_mut().find(|u| u.kind == UnitKind::Archer).unwrap().attack_seconds /= rate;
        w.set_economy(eco);
        w.units.retain(|u| u.kind != UnitKind::King);
        let keep = w.buildings.iter().find(|b| b.owner == 2 && b.kind == BuildingKind::Keep).unwrap().clone();
        let top = (keep.x as f32 + keep.w as f32 / 2.0, keep.y as f32 + keep.h as f32 / 2.0);
        let mut s = Unit::new(40_000, 2, UnitKind::Spearman, top);
        s.on_wall = true;
        w.units.push(s);
        let range = w.economy.unit(UnitKind::Archer).unwrap().range;
        w.units.push(Unit::new(40_001, 1, UnitKind::Archer, (top.0, top.1 + range.min(keep.h as f32 / 2.0 + 2.0))));
        run(&mut w, seconds);
        w.units.iter().find(|u| u.id == 40_000).expect("Speerträger lebt").damage
    }

    #[test]
    fn deckung_auf_gebaeuden_laesst_schuesse_danebengehen() {
        let open = damage_on_keep(0, 1.0, 60.0);
        assert!(open > 0, "ohne Deckung trifft der Bogenschütze");
        assert_eq!(damage_on_keep(100, 1.0, 60.0), 0, "volle Deckung: alles daneben");
        let half = damage_on_keep(50, 1.0, 60.0);
        assert!(half > open / 4 && half < open * 3 / 4, "halbe Deckung: etwa halber Schaden ({half} von {open})");
    }

    #[test]
    fn angriffstakt_je_einheit() {
        let slow = damage_on_keep(0, 1.0, 30.0);
        let fast = damage_on_keep(0, 2.0, 30.0);
        assert!(slow > 0 && fast > slow * 3 / 2, "doppelter Takt: etwa doppelter Schaden ({fast} gegen {slow})");
    }

    #[test]
    fn zerstoerter_bergfried_bleibt_stehen_und_ist_offen() {
        let mut w = world();
        let keep = w.buildings.iter().find(|b| b.owner == 2 && b.kind == BuildingKind::Keep).unwrap().clone();
        let a = next_to_king(&mut w, 1, UnitKind::Archer, 2, 6.0);
        w.order_attack(1, &[a], protocol::AttackTarget::Building { id: keep.id });
        assert!(w.units.iter().find(|u| u.id == a).unwrap().attack_order.is_none(), "Fernkämpfer nicht gegen Gebäude");
        let ids: Vec<u32> = (0..6).map(|i| next_to_king(&mut w, 1, UnitKind::Swordsman, 2, 3.0 + i as f32 * 0.7)).collect();
        // König 2 weg, damit er die Schwertkämpfer nicht aufhält.
        w.units.retain(|u| !(u.owner == 2 && u.kind == UnitKind::King));
        let center = (keep.x as f32 + keep.w as f32 / 2.0, keep.y as f32 + keep.h as f32 / 2.0);
        assert!(!w.is_walkway(1, center), "vorher kommt der Feind nicht hinauf");
        w.order_attack(1, &ids, protocol::AttackTarget::Building { id: keep.id });
        steps(&mut w, 300.0);
        let b = w.buildings.iter().find(|b| b.id == keep.id).expect("Bergfried steht noch");
        assert_eq!(b.hp, 0);
        assert!(!w.take_events().iter().any(|e| matches!(e, ServerMsg::BuildingRemoved { building_id } if *building_id == keep.id)));
        assert!(w.units.iter().filter(|u| ids.contains(&u.id)).all(|u| u.attack_order.is_none()), "Ruine ist kein Ziel mehr");
        assert!(w.is_walkway(1, center), "Tür weg: der Feind kann hinauf");
    }

    #[test]
    fn auf_dem_weg_zum_gebaeude_gehen_feinde_vor() {
        let mut w = world();
        let keep = w.buildings.iter().find(|b| b.owner == 2 && b.kind == BuildingKind::Keep).unwrap().clone();
        w.units.retain(|u| !(u.owner == 2 && u.kind == UnitKind::King));
        let pos = (keep.x as f32 + keep.w as f32 / 2.0, (keep.y + keep.h) as f32 + 6.0);
        w.units.push(Unit::new(40_001, 1, UnitKind::Swordsman, pos));
        w.units.push(Unit::new(40_002, 2, UnitKind::Spearman, (pos.0 + 1.0, pos.1)));
        w.order_attack(1, &[40_001], protocol::AttackTarget::Building { id: keep.id });
        steps(&mut w, 2.0);
        let s = w.units.iter().find(|u| u.id == 40_002);
        assert!(s.is_none_or(|s| s.damage > 0), "erst den Speerträger");
        assert_eq!(w.buildings.iter().find(|b| b.id == keep.id).unwrap().hp, w.economy.building(BuildingKind::Keep).unwrap().at(1).max_hp);
        assert!(w.units.iter().find(|u| u.id == 40_001).unwrap().attack_order.is_some(), "Befehl bleibt");
    }

    #[test]
    fn soldaten_bevorzugen_kaempfer_vor_zivilisten() {
        let mut w = world();
        w.units.retain(|u| u.kind != UnitKind::King);
        let pos = (20.0, 20.0);
        w.units.push(Unit::new(41_001, 2, UnitKind::Carrier, (pos.0 + 0.8, pos.1)));
        w.units.push(Unit::new(41_002, 2, UnitKind::Spearman, (pos.0 - 1.2, pos.1)));
        w.units.push(Unit::new(41_003, 1, UnitKind::Swordsman, pos));
        run(&mut w, 1.0);
        assert_eq!(w.units.iter().find(|u| u.id == 41_001).unwrap().damage, 0, "Träger noch heil");
        assert!(w.units.iter().find(|u| u.id == 41_002).is_none_or(|u| u.damage > 0));
    }

    #[test]
    fn reparieren_kostet_anteilig_und_nicht_bei_feinden() {
        let mut w = world();
        let keep = w.buildings.iter().find(|b| b.owner == 2 && b.kind == BuildingKind::Keep).unwrap().id;
        let max = w.economy.building(BuildingKind::Keep).unwrap().at(1).max_hp;
        w.buildings.iter_mut().find(|b| b.id == keep).unwrap().hp = max / 2;
        let cost = w.repair_cost(w.buildings.iter().find(|b| b.id == keep).unwrap());
        let s = next_to_king(&mut w, 1, UnitKind::Spearman, 2, 20.0);
        assert_eq!(w.repair(2, keep).unwrap_err().key, "error.enemy_near");
        // Weg mit dem Speerträger und dem König von 1 (steht womöglich auch nah).
        w.units.retain(|u| u.id != s && u.owner != 1);
        w.admin_give(2, &cost, 0);
        let b = w.repair(2, keep).unwrap();
        assert_eq!(b.hp, max);
        assert_eq!(w.repair(2, keep).unwrap_err().key, "error.not_damaged");
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
