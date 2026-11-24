//! Kampf (Grundgerüst) und Spielziel.
//!
//! Soldaten und König mit `attack` greifen von selbst die nächste feindliche
//! Einheit in Reichweite an (auch Zivilisten), solange sie stehen. Mit einem
//! Angriffsbefehl (`order_attack`) verfolgen sie ein Ziel, bis es fällt; Nahkämpfer
//! reißen so auch Gebäude, Mauern und Tore ein. Fernkämpfer (Reichweite
//! über `army.melee_range`) schießen nicht auf Gebäude. Nahkampf geht nur auf derselben
//! Ebene (Boden oder Wehrgang).
//!
//! Schaden: jede Einheit hat Schadenswerte je Art (`damage`), Einheiten, Pferde und Gebäude
//! (je Stufe) eine Rüstung je Art. Ein Treffer richtet (X − min) × (1 − block) an, mit der Art,
//! die am meisten durchkommt (siehe `Economy::hit_damage`). Wen die Rüstung ganz schützt
//! (block 1, z. B. der König gegen Fernkampf), den greift man damit gar nicht erst an.
//! Treffer gegen Reiter gehen zu `army.horse_hit_percent` aufs Pferd, sonst auf den Reiter.
//!
//! Fällt ein König, scheidet sein Spieler aus: alles, was ihm gehört, verschwindet.
//! Bleibt nur noch ein Spieler oder ein Bündnis übrig, ist das Spiel entschieden.
//!
//! Deckung: wer oben auf einem Gebäude steht (Mauer, Turm, Tor …), wird von Fernkämpfern
//! seltener getroffen – `cover_percent` der Gebäudestufe sagt, wie viel Prozent der Schüsse
//! danebengehen. Der Schuss fliegt trotzdem (Pfeil sichtbar), richtet aber nichts an.
//!
//! Pfeile und Bolzen fliegen wirklich: Schaden gibt es erst beim Aufschlag. Der Schütze hält
//! auf ein laufendes Ziel vor (Tempo und Richtung aus dem letzten Schritt), läuft es geradeaus
//! weiter, trifft er meistens. Biegt es ab oder bleibt stehen, schlägt der Pfeil am vorausberechneten
//! Punkt ein und geht daneben – außer dort steht gerade ein anderer Feind (`army.arrow_hit_radius`).
//! Ohne freie Bahn zum Vorhaltepunkt zielt er dorthin, wo das Ziel gerade steht.
//!
//! Höhenvorteil (nur relativ, Mauern und Türme zählen mit): Nahkampf geht nur bis
//! `army.melee_max_levels` Stufen Unterschied. Fernkämpfer reichen je Stufe, die sie höher
//! stehen als ihr Ziel, `army.height_range_percent` weiter. Gleiche Höhe = kein Bonus.

use std::collections::HashMap;

use protocol::{AttackTarget, BuildingKind, ServerMsg, Shot, UnitKind};

use super::{Unit, World};

/// So lange (Sekunden) zeigt der Client nach einem selbst gewählten Schlag noch, wen die Einheit angreift.
const FOE_SHOWN_SECONDS: f32 = 3.0;
/// Kantenlänge der Suchzellen in Feldern.
const CELL: f32 = 8.0;
/// Halbe Körperbreite: so weit reicht ein Schlag über die Reichweite hinaus.
const REACH: f32 = 0.5;
/// Beim Verfolgen: so oft (Sekunden) wird der Weg zum Ziel neu gesucht.
const CHASE_SECONDS: f32 = 1.0;
/// Gegenwehr: wer vor so kurzer Zeit (Sekunden) getroffen wurde, wendet sich dem Angreifer zu …
const STRUCK_SECONDS: f32 = 3.0;
/// … und gibt nach so vielen Sekunden auf, wenn er ihn nicht erwischt (dann weiter am Gebäude).
const RETALIATE_SECONDS: f32 = 12.0;

/// Darf eine Einheit dieser Art jetzt angreifen? Zu Pferd zählt `attack_mounted`
/// (im Stehen und im Reiten), zu Fuß `attack_standing` bzw. im Laufen `attack_moving`.
pub(super) fn may_attack(def: &protocol::UnitDef, mounted: bool, moving: bool) -> bool {
    if mounted {
        def.attack_mounted
    } else if moving {
        def.attack_moving
    } else {
        def.attack_standing
    }
}

/// Ein Pfeil oder Bolzen in der Luft.
#[derive(Clone, Debug)]
pub(super) struct Projectile {
    /// Schütze (Id), sein Spieler und seine Art (für den Schaden, auch wenn er inzwischen fiel).
    shooter: u32,
    owner: u32,
    kind: UnitKind,
    /// Wen er treffen wollte.
    target: u32,
    /// Einschlagpunkt und Höhe der Füße dort.
    to: (f32, f32),
    to_feet: f32,
    /// Sekunden bis zum Aufschlag.
    left: f32,
}

/// Ein Treffer: (Ziel (Index), Angreifer (Id), Art des Angreifers, Spieler des Angreifers, Fernkampf).
pub(super) type Hit = (usize, u32, UnitKind, u32, bool);

/// So weit darf die Höhe am Ziel von der Höhe am Einschlagpunkt abweichen (Weltmaß), sonst fliegt
/// der Pfeil drüber oder schlägt darunter ein (z. B. oben auf der Mauer gezielt, unten getroffen).
const HIT_HEIGHT: f32 = 0.8;

/// Bündnis eines Spielers: (true, Bündnis) oder allein (false, Spieler).
type TeamKey = (bool, u32);

impl World {
    /// Lebenspunkte einer Einheit (0 = hat keine, z. B. Träger).
    /// Wie viele Geländestufen steht `u` höher als `t` (negativ: tiefer)? Wehrgänge zählen mit.
    pub(super) fn levels_above(&self, u: &Unit, t: &Unit) -> f32 {
        (self.feet_h(u) - self.feet_h(t)) / super::ballistics::LEVEL_HEIGHT
    }

    /// Reichweite gegen dieses Ziel: wer höher steht, reicht weiter (`army.height_range_percent` je Stufe).
    pub(super) fn range_against(&self, u: &Unit, t: &Unit, range: f32) -> f32 {
        let pct = self.economy.def.army.height_range_percent;
        if pct <= 0.0 {
            return range;
        }
        range * (1.0 + pct / 100.0 * self.levels_above(u, t).max(0.0))
    }

    /// Nahkampf: gleiche Ebene (Boden oder Wehrgang) und nicht mehr als `army.melee_max_levels` Stufen Unterschied.
    pub(super) fn melee_reaches(&self, u: &Unit, t: &Unit) -> bool {
        t.on_wall == u.on_wall && self.levels_above(u, t).abs() <= self.economy.def.army.melee_max_levels + 0.05
    }

    /// Größte Reichweite, die eine Einheit mit Höhenvorteil haben kann (für die Suche).
    fn max_range(&self, range: f32) -> f32 {
        // 10 Geländestufen plus der höchste Turm reichen dicke.
        range * (1.0 + self.economy.def.army.height_range_percent / 100.0 * 24.0)
    }

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
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn order_attack(&mut self, player: u32, unit_ids: &[u32], target: AttackTarget) -> bool {
        self.order_attack_full(player, unit_ids, target, false)
    }

    /// Wie `order_attack`; `force` (Knopf „Alles angreifen“) erlaubt auch eigene,
    /// verbündete und neutrale Ziele. Niemand greift sich selbst an.
    pub fn order_attack_full(&mut self, player: u32, unit_ids: &[u32], target: AttackTarget, force: bool) -> bool {
        let Some((owner, pos)) = self.attack_target_info(target) else { return false };
        let mut movers = Vec::new();
        for k in 0..self.units.len() {
            let u = &self.units[k];
            if u.owner != player || u.is_civilian() || !unit_ids.contains(&u.id) {
                continue;
            }
            let itself = target == AttackTarget::Unit { id: u.id };
            if (force || self.can_attack(player, owner)) && !itself && self.can_hurt(u, target) {
                let u = &mut self.units[k];
                u.attack_order = Some(target);
                u.resume_order = None;
                u.attack_force = force;
                u.waypoints.clear();
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

    /// Kann diese Einheit dem Ziel überhaupt schaden? Nicht, wenn dessen Rüstung jeden ihrer
    /// Schadenswerte ganz abwehrt (König gegen Fernkampf); Fernkämpfer nichts gegen Gebäude;
    /// Zivilisten nie.
    fn can_hurt(&self, u: &Unit, target: AttackTarget) -> bool {
        let Some(def) = self.economy.unit(u.kind) else { return false };
        if def.attack == 0 || def.range <= 0.0 {
            return false;
        }
        let ranged = self.economy.is_ranged(def.range);
        // Belagerungsgeräte: was sie laut `siege` treffen.
        if let Some(sd) = &def.siege {
            return match target {
                AttackTarget::Building { id } => {
                    sd.hits_buildings
                        && self
                            .buildings
                            .iter()
                            .find(|b| b.id == id)
                            .and_then(|b| self.economy.building(b.kind).map(|d| self.economy.hit_damage(def, &d.at(b.level).armor)))
                            .is_some_and(|y| y > 0)
                }
                AttackTarget::Unit { id } => sd.hits_units && self.units.iter().find(|t| t.id == id).is_some_and(|t| self.can_damage(def, t)),
            };
        }
        match target {
            AttackTarget::Building { id } => {
                !ranged
                    && self
                        .buildings
                        .iter()
                        .find(|b| b.id == id)
                        .and_then(|b| self.economy.building(b.kind).map(|d| self.economy.hit_damage(def, &d.at(b.level).armor)))
                        .is_some_and(|y| y > 0)
            }
            AttackTarget::Unit { id } => self.units.iter().find(|t| t.id == id).is_some_and(|t| self.can_damage(def, t)),
        }
    }

    /// Richtet diese Einheitenart gegen das Ziel etwas aus (gegen Reiter: an Pferd oder Reiter)?
    pub(crate) fn can_damage(&self, attacker: &protocol::UnitDef, t: &Unit) -> bool {
        let Some(tdef) = self.economy.unit(t.kind) else { return false };
        self.economy.unit_hit_damage(attacker, &tdef.armor) > 0 || (t.mounted && self.economy.unit_hit_damage(attacker, &tdef.horse_armor) > 0)
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

    /// Zufall mit `percent` von 100 (Deckung: geht der Schuss daneben? Reiter: trifft es das Pferd?)
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
            // Selbst gewähltes Ziel nur kurz anzeigen, solange weiter gekämpft wird.
            u.last_foe = u.last_foe.map(|(id, age)| (id, age + dt)).filter(|&(_, age)| age < FOE_SHOWN_SECONDS);
            u.attacked_by = u.attacked_by.map(|(id, age)| (id, age + dt)).filter(|&(_, age)| age < STRUCK_SECONDS);
            // Gegenwehr zu lange erfolglos: zurück zum Gebäude.
            if let Some((order, left)) = u.resume_order {
                if left - dt <= 0.0 {
                    u.attack_order = Some(order);
                    u.resume_order = None;
                    u.chase_timer = 0.0;
                } else {
                    u.resume_order = Some((order, left - dt));
                }
            }
            let team = match self.teams.get(&u.owner) {
                Some(&t) => (true, t),
                None => (false, u.owner),
            };
            if !teams.contains(&team) {
                teams.push(team);
            }
            grid.entry(((u.pos.0 / CELL) as i32, (u.pos.1 / CELL) as i32)).or_default().push(k);
            // Nach dem Sturz vom Pferd: liegt am Boden und kämpft nicht.
            if u.down > 0.0 {
                continue;
            }
            // Belagerungsgeräte zielen selbst (siehe `siege.rs`).
            if u.kind.is_siege() {
                continue;
            }
            if u.attack_order.is_some() {
                ordered.push(k);
            } else if def.attack > 0 && def.range > 0.0 && u.attack_cooldown <= 0.0 && may_attack(def, u.mounted, !u.path.is_empty()) {
                fighters.push(k);
            }
        }
        // Pfeile, die jetzt aufschlagen.
        let mut hits: Vec<Hit> = self.land_projectiles(dt, &index, &grid);
        // Steine und Bolzen der Belagerungsgeräte.
        let (siege_hits, siege_building_hits) = self.land_siege(dt, &grid, CELL);
        hits.extend(siege_hits);
        // Neue Schüsse: (Schütze, Ziel (Indizes), Reichweite, Bahn).
        let mut launches: Vec<(usize, usize, f32, super::ballistics::Flight)> = Vec::new();
        if teams.len() < 2 && ordered.is_empty() && hits.is_empty() && siege_building_hits.is_empty() {
            return;
        }
        // (Gebäude, Art des Angreifers)
        let mut building_hits: Vec<(u32, UnitKind)> = siege_building_hits;
        let mut shots: Vec<Shot> = self.siege_fire(dt, &grid, CELL);
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
                if let Some((j, flight)) = self.nearest_foe(k, &grid, true) {
                    let (by, me, kind) = (u.owner, u.id, u.kind);
                    let range = self.range_against(u, &self.units[j], def.range);
                    let u = &mut self.units[k];
                    u.path.clear();
                    u.layers.clear();
                    u.target = Some(u.pos);
                    u.chase_timer = 0.0;
                    if u.attack_cooldown <= 0.0 {
                        u.attack_cooldown = def.attack_seconds;
                        u.strikes = u.strikes.wrapping_add(1);
                        match flight {
                            Some(f) if ranged => launches.push((k, j, range, f)),
                            _ => hits.push((j, me, kind, by, ranged)),
                        }
                    }
                    let foe = self.units[j].id;
                    self.units[k].last_foe = Some((foe, 0.0));
                    continue;
                }
            }
            // Am Gebäude, aber selbst angegriffen (z. B. von Schützen außer Reichweite): erst den
            // Angreifer verfolgen, danach weiter am Gebäude.
            if let (AttackTarget::Building { .. }, Some((aid, _))) = (order, u.attacked_by) {
                let foe = AttackTarget::Unit { id: aid };
                let alive = index.get(&aid).map(|&j| self.units[j].owner);
                if alive.is_some_and(|o| self.can_attack(u.owner, o)) && self.can_hurt(u, foe) {
                    let u = &mut self.units[k];
                    u.resume_order = Some((order, RETALIATE_SECONDS));
                    u.attack_order = Some(foe);
                    u.attacked_by = None;
                    u.chase_timer = 0.0;
                    continue;
                }
            }
            // Wo steht das Ziel, wie weit ist es, und kann man es von hier treffen?
            let found = match order {
                AttackTarget::Unit { id } => index.get(&id).map(|&j| {
                    let t = &self.units[j];
                    let d = (t.pos.0 - u.pos.0).hypot(t.pos.1 - u.pos.1);
                    // Fernkampf: nur wenn Wurfbahn bzw. Sichtlinie frei ist, sonst näher heran.
                    // Von oben reicht es weiter.
                    let range = if ranged { self.range_against(u, t, def.range) } else { def.range };
                    let flight = if ranged && d <= range + REACH { self.flight(u, t, range) } else { None };
                    let ok = if ranged { flight.is_some() } else { self.melee_reaches(u, t) };
                    (t.owner, t.pos, t.on_wall, d - (range - def.range), ok, Some((j, flight, range)))
                }),
                AttackTarget::Building { id } => self.buildings.iter().find(|b| b.id == id && b.hp > 0).map(|b| {
                    let dx = (b.x as f32 - u.pos.0).max(u.pos.0 - (b.x + b.w) as f32).max(0.0);
                    let dy = (b.y as f32 - u.pos.1).max(u.pos.1 - (b.y + b.h) as f32).max(0.0);
                    let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
                    (b.owner, self.foot_of(center, u.pos), false, dx.hypot(dy), true, None)
                }),
            };
            let Some((owner, pos, on_wall, dist, level_ok, j)) = found.filter(|f| (u.attack_force || self.can_attack(u.owner, f.0)) && self.can_hurt(u, order)) else {
                // Ziel gefallen, abgerissen oder nicht (mehr) angreifbar: stehen bleiben
                // (nach der Gegenwehr: zurück zum Gebäude).
                let u = &mut self.units[k];
                if let Some((resume, _)) = u.resume_order.take() {
                    u.attack_order = Some(resume);
                    u.chase_timer = 0.0;
                    continue;
                }
                u.attack_order = None;
                u.path.clear();
                u.layers.clear();
                u.target = Some(u.pos);
                continue;
            };
            if dist <= reach && level_ok {
                let by = u.owner;
                let u = &mut self.units[k];
                // Fernkämpfer schießen nur im Stand (außer sie dürfen im Laufen): wer gerade noch lief, hält erst an.
                let was_moving = !u.path.is_empty();
                u.path.clear();
                u.layers.clear();
                u.target = Some(u.pos);
                if ranged && was_moving && !(u.mounted || def.attack_moving) {
                    continue;
                }
                // Darf sie so (zu Fuß im Stand bzw. zu Pferd) überhaupt angreifen? Sonst bleibt sie nur dran.
                if !may_attack(def, u.mounted, false) {
                    continue;
                }
                if u.attack_cooldown <= 0.0 {
                    u.attack_cooldown = def.attack_seconds;
                    u.strikes = u.strikes.wrapping_add(1);
                    match (order, j) {
                        (AttackTarget::Unit { .. }, Some((j, flight, range))) => match flight {
                            Some(f) if ranged => launches.push((k, j, range, f)),
                            _ => hits.push((j, u.id, u.kind, by, ranged)),
                        },
                        (AttackTarget::Building { id }, _) => building_hits.push((id, u.kind)),
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
            // Reiter kommen nicht auf Wehrgänge: ein Ziel oben verfolgen sie nur bis zum Fuß.
            let (pos, on_wall) = if on_wall && u.mounted { (self.foot_of(pos, from), false) } else { (pos, on_wall) };
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
                match flight {
                    Some(f) => launches.push((k, j, self.range_against(u, &self.units[j], def.range), f)),
                    None => hits.push((j, u.id, u.kind, u.owner, self.economy.is_ranged(def.range))),
                }
                self.units[k].attack_cooldown = def.attack_seconds;
                self.units[k].strikes = self.units[k].strikes.wrapping_add(1);
                let foe = self.units[j].id;
                self.units[k].last_foe = Some((foe, 0.0));
            }
        }
        // Abschuss: vorhalten, Pfeil in die Luft. Das Ziel merkt schon jetzt, wer auf es schießt.
        for (k, j, range, flight) in launches {
            let (shot, arrow) = self.launch(k, j, range, flight);
            shots.push(shot);
            self.projectiles.push(arrow);
            let shooter = self.units[k].id;
            self.units[j].attacked_by = Some((shooter, 0.0));
        }
        if !shots.is_empty() {
            self.events.push(ServerMsg::Shots { shots });
        }
        // Gebäude: Schaden, bei 0 eingerissen.
        let mut building_alerts = Vec::new();
        for (id, kind) in building_hits {
            let Some(adef) = self.economy.unit(kind) else { continue };
            let attack = self
                .buildings
                .iter()
                .find(|b| b.id == id)
                .and_then(|b| self.economy.building(b.kind).map(|d| self.economy.hit_damage(adef, &d.at(b.level).armor)))
                .unwrap_or(0);
            let Some(b) = self.buildings.iter_mut().find(|b| b.id == id) else { continue };
            if attack > 0 && b.hp > 0 {
                building_alerts.push((b.owner, b.kind, (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)));
            }
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
        for (owner, kind, at) in building_alerts {
            self.alert_building_attacked(owner, kind, at);
        }
        // Deckung: Schüsse auf Einheiten oben auf Gebäuden gehen teils daneben (vor der Rüstung).
        // Dann trifft es bei Reitern das Pferd oder den Reiter, und dessen Rüstung zählt.
        let horse_percent = self.economy.def.army.horse_hit_percent.min(100);
        let mut landed = Vec::with_capacity(hits.len());
        for (j, attacker, kind, by, ranged) in hits {
            // Das Ziel merkt sich, wer es angreift (auch wenn der Schuss danebengeht).
            self.units[j].attacked_by = Some((attacker, 0.0));
            let cover = if ranged { self.cover_of(&self.units[j]) } else { 0 };
            if self.misses(cover) {
                continue;
            }
            let horse = self.units[j].mounted && self.misses(horse_percent);
            let (Some(adef), Some(tdef)) = (self.economy.unit(kind), self.economy.unit(self.units[j].kind)) else { continue };
            let attack = self.economy.unit_hit_damage(adef, if horse { &tdef.horse_armor } else { &tdef.armor });
            if attack > 0 {
                landed.push((j, attack, by, horse));
            }
        }
        if landed.is_empty() {
            return;
        }
        let mut killed_by: HashMap<usize, u32> = HashMap::new();
        for (j, attack, by, horse) in landed {
            let max = self.economy.unit(self.units[j].kind).map_or(0, |d| d.hp);
            self.hurt_unit(j, attack, horse);
            let u = &self.units[j];
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

    /// Darf dieses Gebäude hier gerade gebaut werden (kein Feind im Umkreis, siehe `check_no_enemy`)?
    pub fn enemy_near(&self, player: u32, kind: protocol::BuildingKind, at: (f32, f32)) -> bool {
        self.check_no_enemy(player, kind, at).is_err()
    }

    /// Nächste feindliche Einheit, die Einheit `k` von hier treffen kann. Kämpfer (Soldaten,
    /// König) gehen vor Zivilisten; `only_fighters`: nur Kämpfer.
    /// Fernkämpfer: nur wen Wurfbahn bzw. Sichtlinie erreicht (mit der Bahn dazu).
    fn nearest_foe(&self, k: usize, grid: &HashMap<(i32, i32), Vec<usize>>, only_fighters: bool) -> Option<(usize, Option<super::ballistics::Flight>)> {
        let u = &self.units[k];
        let def = self.economy.unit(u.kind)?;
        let ranged = self.economy.is_ranged(def.range);
        let reach = if ranged { self.max_range(def.range) } else { def.range } + REACH;
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
                    let d = (t.pos.0 - u.pos.0).hypot(t.pos.1 - u.pos.1);
                    if d > reach || !self.can_damage(def, t) {
                        continue;
                    }
                    if !ranged && !self.melee_reaches(u, t) {
                        continue;
                    }
                    if ranged && d > self.range_against(u, t, def.range) + REACH {
                        continue;
                    }
                    let key = (civilian, d);
                    near.push((key, j));
                }
            }
        }
        near.sort_by(|a, b| a.0.0.cmp(&b.0.0).then(a.0.1.total_cmp(&b.0.1)));
        near.into_iter().find_map(|(_, j)| {
            if !ranged {
                return Some((j, None));
            }
            let t = &self.units[j];
            self.flight(u, t, self.range_against(u, t, def.range)).map(|f| (j, Some(f)))
        })
    }

    /// Steht ein feindlicher Soldat oder König näher als `enemy_radius` des Gebäudes?
    /// Dann darf es hier nicht gebaut, ausgebaut oder repariert werden.
    pub fn check_no_enemy(&self, player: u32, kind: protocol::BuildingKind, at: (f32, f32)) -> Result<(), protocol::Text> {
        let radius = self.economy.building(kind).map_or(0.0, |d| d.enemy_radius);
        if radius <= 0.0 {
            return Ok(());
        }
        // Abstand zuerst: das ist billig, und die KI fragt das bei der Bauplatzsuche sehr oft.
        let r2 = radius * radius;
        let near = self.units.iter().any(|u| {
            let (dx, dy) = (u.pos.0 - at.0, u.pos.1 - at.1);
            dx * dx + dy * dy <= r2
                && self.economy.unit(u.kind).is_some_and(|d| d.attack > 0)
                && self.can_attack(player, u.owner)
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
        self.check_no_enemy(player, b.kind, center)?;
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

    /// Flugzeit (Sekunden) über `d` Felder mit dieser Bahn; `dh` = Höhenunterschied (Welt).
    fn flight_seconds(&self, d: f32, dh: f32, flight: super::ballistics::Flight) -> f32 {
        let len = match flight {
            super::ballistics::Flight::Arc(tan) => d * (1.0 + tan * tan).sqrt(),
            super::ballistics::Flight::Straight => d.hypot(dh),
        };
        (len / self.economy.def.army.arrow_speed.max(0.1)).max(0.15)
    }

    /// Schuss von `k` auf `j`: vorhalten (wo steht das Ziel, wenn der Pfeil ankommt?) und den Pfeil
    /// auf den Weg schicken. Ohne freie Bahn zum Vorhaltepunkt geht er dorthin, wo das Ziel jetzt steht.
    fn launch(&self, k: usize, j: usize, range: f32, flight: super::ballistics::Flight) -> (Shot, Projectile) {
        let (u, t) = (&self.units[k], &self.units[j]);
        let dist = |p: (f32, f32)| (p.0 - u.pos.0).hypot(p.1 - u.pos.1);
        let dh = self.feet_h(t) - self.feet_h(u);
        let mut aim = (t.pos, t.on_wall, flight, range);
        if t.vel.0.hypot(t.vel.1) > 1e-3 {
            // Ein paar Runden: Flugzeit zum Punkt → wo das Ziel dann ist → neue Flugzeit.
            let (w, h) = (self.map.width as f32 - 0.01, self.map.height as f32 - 0.01);
            let mut p = t.pos;
            for _ in 0..3 {
                let secs = self.flight_seconds(dist(p), dh, flight);
                p = ((t.pos.0 + t.vel.0 * secs).clamp(0.0, w), (t.pos.1 + t.vel.1 * secs).clamp(0.0, h));
            }
            let mut ghost = Unit::new(t.id, t.owner, t.kind, p);
            ghost.on_wall = t.on_wall && self.tile_of(p).is_some_and(|i| self.occupied[i] != 0);
            let def_range = self.economy.unit(u.kind).map_or(range, |d| d.range);
            let r = self.range_against(u, &ghost, def_range);
            if dist(p) <= r + REACH {
                if let Some(f) = self.flight(u, &ghost, r) {
                    aim = (p, ghost.on_wall, f, r);
                }
            }
        }
        let (to, to_wall, flight, range) = aim;
        let mut ghost = Unit::new(t.id, t.owner, t.kind, to);
        ghost.on_wall = to_wall;
        let to_feet = self.feet_h(&ghost);
        let seconds = self.flight_seconds(dist(to), to_feet - self.feet_h(u), flight);
        let shot = Shot {
            from: u.pos,
            to,
            from_wall: u.on_wall,
            to_wall,
            kind: u.kind,
            from_h: self.feet_h(u) + 0.7,
            to_h: to_feet + 0.7,
            arc: match flight {
                super::ballistics::Flight::Arc(tan) => Some((tan, range)),
                super::ballistics::Flight::Straight => None,
            },
            seconds,
        };
        let arrow = Projectile { shooter: u.id, owner: u.owner, kind: u.kind, target: t.id, to, to_feet, left: seconds };
        (shot, arrow)
    }

    /// Pfeile weiterfliegen lassen; wer beim Aufschlag am Einschlagpunkt steht, wird getroffen:
    /// das Ziel, sonst der nächste Feind des Schützen dort, dem er schaden kann.
    fn land_projectiles(&mut self, dt: f32, index: &HashMap<u32, usize>, grid: &HashMap<(i32, i32), Vec<usize>>) -> Vec<Hit> {
        if self.projectiles.is_empty() {
            return Vec::new();
        }
        let mut arrived = Vec::new();
        self.projectiles.retain_mut(|p| {
            p.left -= dt;
            if p.left > 0.0 {
                return true;
            }
            arrived.push(p.clone());
            false
        });
        let radius = self.economy.def.army.arrow_hit_radius;
        let mut hits = Vec::new();
        for p in arrived {
            let Some(adef) = self.economy.unit(p.kind) else { continue };
            let near = |t: &Unit| (t.pos.0 - p.to.0).hypot(t.pos.1 - p.to.1) <= radius && (self.feet_h(t) - p.to_feet).abs() <= HIT_HEIGHT;
            let aimed = index.get(&p.target).copied().filter(|&j| near(&self.units[j]));
            let struck = aimed.or_else(|| {
                let (cx, cy) = ((p.to.0 / CELL) as i32, (p.to.1 / CELL) as i32);
                let mut best: Option<(f32, usize)> = None;
                for gy in cy - 1..=cy + 1 {
                    for gx in cx - 1..=cx + 1 {
                        for &j in grid.get(&(gx, gy)).map_or(&[][..], |v| v.as_slice()) {
                            let t = &self.units[j];
                            if t.owner == p.owner || !self.can_attack(p.owner, t.owner) || !near(t) || !self.can_damage(adef, t) {
                                continue;
                            }
                            if t.kind.is_civilian() && self.inside(t) {
                                continue;
                            }
                            let d = (t.pos.0 - p.to.0).hypot(t.pos.1 - p.to.1);
                            if best.is_none_or(|(b, _)| d < b) {
                                best = Some((d, j));
                            }
                        }
                    }
                }
                best.map(|(_, j)| j)
            });
            if let Some(j) = struck {
                hits.push((j, p.shooter, p.kind, p.owner, true));
            }
        }
        hits
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
    pub(super) fn knock_out(&mut self, player: u32, by: Option<u32>) {
        if !self.eliminated.insert(player) {
            return;
        }
        if let Some(winner) = by.filter(|&w| w != player && !self.eliminated.contains(&w)) {
            self.send_loot(player, winner);
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
    pub(super) fn check_victory(&mut self) {
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
            (u.hp, u.attack, u.range, u.attack_seconds) = (hp, attack, range, 1.5);
            // Ohne Schadensliste zählt `attack`; Rüstung nur beim König (gegen Fernkampf).
            u.damage.clear();
            u.horse_armor.clear();
            u.armor = if immune {
                [protocol::DamageType::Ranged, protocol::DamageType::HeavyRanged].map(|kind| protocol::Armor { kind, min: 0, block: 1.0 }).to_vec()
            } else {
                Vec::new()
            };
        }
        // Gebäude ohne Rüstung (die eigenen Tests dazu setzen sie selbst).
        for b in &mut eco.def.buildings {
            b.levels.iter_mut().for_each(|l| l.armor.clear());
        }
        let mut w = World::new(map, eco);
        w.spawn_player(1);
        w.spawn_player(2);
        w.take_events();
        w
    }

    fn armor(list: &[(protocol::DamageType, u32, f32)]) -> Vec<protocol::Armor> {
        list.iter().map(|&(kind, min, block)| protocol::Armor { kind, min, block }).collect()
    }

    /// Schaden, den ein Schwertkämpfer (12, ohne Liste: schwerer Nahkampf) in `seconds` an einem
    /// Speerträger mit dieser Rüstung anrichtet.
    fn damage_on_spearman(a: Vec<protocol::Armor>, seconds: f32) -> u32 {
        let mut w = world();
        w.units.retain(|u| u.kind != UnitKind::King);
        if let Some(d) = w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Spearman) {
            d.armor = a;
            d.attack = 0;
            d.hp = 100_000;
        }
        w.units.push(Unit::new(20_001, 2, UnitKind::Spearman, (50.0, 50.0)));
        w.units.push(Unit::new(20_002, 1, UnitKind::Swordsman, (51.0, 50.0)));
        run(&mut w, seconds);
        w.units.iter().find(|u| u.id == 20_001).unwrap().damage
    }

    #[test]
    fn ruestung_zieht_mindestwert_ab_und_wehrt_anteil_ab() {
        use protocol::DamageType as D;
        let open = damage_on_spearman(Vec::new(), 15.0);
        assert!(open > 0 && open % 12 == 0, "ohne Rüstung voller Schaden: {open}");
        let hits = open / 12;
        // (12 − 2) × (1 − 0,5) = 5 je Treffer.
        assert_eq!(damage_on_spearman(armor(&[(D::HeavyMelee, 2, 0.5)]), 15.0), hits * 5);
        // Rüstung gegen eine andere Art hilft nicht.
        assert_eq!(damage_on_spearman(armor(&[(D::Melee, 5, 0.9)]), 15.0), open);
        // Fast alles abgewehrt: Mindestschaden 1 je Treffer; ganz abgewehrt: nichts.
        assert_eq!(damage_on_spearman(armor(&[(D::HeavyMelee, 20, 0.0)]), 15.0), hits);
        assert_eq!(damage_on_spearman(armor(&[(D::HeavyMelee, 0, 1.0)]), 15.0), 0);
    }

    #[test]
    fn die_staerkste_schadensart_zaehlt() {
        use protocol::DamageType as D;
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap();
        let mut knight = eco.unit(UnitKind::Knight).unwrap().clone();
        knight.damage = vec![protocol::Damage { kind: D::HeavyMelee, value: 16 }, protocol::Damage { kind: D::Siege, value: 12 }];
        let wall = armor(&[(D::HeavyMelee, 0, 0.5), (D::Siege, 0, 0.0)]);
        assert_eq!(eco.hit_damage(&knight, &wall), 12, "gegen die Mauer zählt Belagerung");
        assert_eq!(eco.hit_damage(&knight, &[]), 16, "sonst der schwere Nahkampf");
    }

    #[test]
    fn treffer_auf_reiter_nach_anteil_aufs_pferd() {
        for (percent, horse_hit, rider_hit) in [(100, true, false), (0, false, true)] {
            let mut w = world();
            w.units.retain(|u| u.kind != UnitKind::King);
            w.economy.def.army.horse_hit_percent = percent;
            w.economy.def.army.horse_hp = 100_000;
            if let Some(d) = w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Knight) {
                (d.attack, d.hp) = (0, 100_000);
            }
            let mut knight = Unit::new(20_001, 2, UnitKind::Knight, (50.0, 50.0));
            knight.mounted = true;
            w.units.push(knight);
            w.units.push(Unit::new(20_002, 1, UnitKind::Swordsman, (51.0, 50.0)));
            run(&mut w, 10.0);
            let k = w.units.iter().find(|u| u.id == 20_001).unwrap();
            assert_eq!(k.horse_damage > 0, horse_hit, "{percent} %: Pferd");
            assert_eq!(k.damage > 0, rider_hit, "{percent} %: Reiter");
        }
    }

    #[test]
    fn mauer_ruestung_haelt_nahkampf_auf() {
        use protocol::DamageType as D;
        let hit_keep = |a: Vec<protocol::Armor>| {
            let mut w = world();
            let keep = w.buildings.iter().find(|b| b.owner == 2 && b.kind == BuildingKind::Keep).unwrap().clone();
            if let Some(d) = w.economy.def.buildings.iter_mut().find(|b| b.kind == BuildingKind::Keep) {
                d.levels.iter_mut().for_each(|l| l.armor = a.clone());
            }
            w.units.retain(|u| !(u.owner == 2 && u.kind == UnitKind::King));
            let s = 30_000;
            w.units.push(Unit::new(s, 1, UnitKind::Swordsman, (keep.x as f32 - 0.6, keep.y as f32 + 1.0)));
            let ok = w.order_attack(1, &[s], protocol::AttackTarget::Building { id: keep.id });
            run(&mut w, 15.0);
            let hp = w.buildings.iter().find(|b| b.id == keep.id).unwrap().hp;
            (ok && w.units.iter().any(|u| u.id == s && u.attack_order.is_some()), keep.hp - hp)
        };
        let (_, open) = hit_keep(Vec::new());
        let (_, half) = hit_keep(armor(&[(D::HeavyMelee, 0, 0.5)]));
        assert!(open > 0 && half * 2 == open, "halbe Abwehr: halber Schaden ({half} von {open})");
        let (attacks, none) = hit_keep(armor(&[(D::HeavyMelee, 0, 1.0)]));
        assert!(!attacks && none == 0, "ganz abgewehrt: gar kein Angriff");
    }

    /// Links Stufe 0, rechts ab Spalte `cliff` Stufe 3 (eine Klippe); Kampfwerte wie `world()`.
    fn cliff_world(width: usize, cliff: usize) -> World {
        let row = format!("{}{}", "0".repeat(cliff), "3".repeat(width - cliff));
        let text = format!("spawn 1 1\nspawn 2 8\n{}", format!("{row}\n").repeat(10));
        let mut w = World::new(GameMap::parse(&text).unwrap(), world().economy.clone().without_start_building());
        w.spawn_player(1);
        w.spawn_player(2);
        w.units.retain(|u| u.kind != UnitKind::King);
        w.take_events();
        w
    }

    fn hurt(w: &World, id: u32) -> bool {
        w.units.iter().find(|u| u.id == id).is_none_or(|u| u.damage > 0)
    }

    #[test]
    fn nahkampf_nicht_ueber_die_klippe() {
        let mut w = cliff_world(30, 15);
        w.units.push(Unit::new(30_001, 1, UnitKind::Spearman, (14.5, 5.5)));
        w.units.push(Unit::new(30_002, 2, UnitKind::Spearman, (15.5, 5.5)));
        run(&mut w, 10.0);
        assert!(!hurt(&w, 30_001) && !hurt(&w, 30_002), "drei Stufen: kein Nahkampf");
        // Auf derselben Höhe geht es.
        w.units.iter_mut().find(|u| u.id == 30_001).unwrap().pos = (16.5, 5.5);
        run(&mut w, 10.0);
        assert!(hurt(&w, 30_001) || hurt(&w, 30_002), "gleiche Höhe: Nahkampf");
    }

    #[test]
    fn von_oben_reicht_der_bogen_weiter() {
        let range = world().economy.unit(UnitKind::Archer).unwrap().range;
        // 10 % weiter weg als die Reichweite: oben (3 Stufen, +15 %) trifft man, auf gleicher Höhe nicht.
        // Die Klippe steht nah beim Schützen, damit ihre Kante die Sicht nach unten nicht verdeckt.
        let d = range * 1.1;
        for (cliff, hits) in [(54, true), (0, false)] {
            let mut w = cliff_world(60, cliff);
            w.set_shot_mode(protocol::ShotMode::LineOfSight);
            w.units.push(Unit::new(30_011, 1, UnitKind::Archer, (59.5, 5.5)));
            w.units.push(Unit::new(30_012, 2, UnitKind::Spearman, (59.5 - d, 5.5)));
            run(&mut w, 10.0);
            assert_eq!(hurt(&w, 30_012), hits, "Klippe bei {cliff}");
        }
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
    fn angegriffene_wehren_sich_erst_und_machen_dann_am_gebaeude_weiter() {
        let mut w = world();
        let keep = w.buildings.iter().find(|b| b.owner == 2 && b.kind == BuildingKind::Keep).unwrap().clone();
        w.units.retain(|u| u.kind != UnitKind::King);
        let center = (keep.x as f32 + keep.w as f32 / 2.0, keep.y as f32 + keep.h as f32 / 2.0);
        let foot = w.foot_of(center, (center.0, center.1 + 10.0));
        w.units.push(Unit::new(41_000, 1, UnitKind::Swordsman, foot));
        let target = protocol::AttackTarget::Building { id: keep.id };
        w.order_attack(1, &[41_000], target);
        // Ein Bogenschütze schießt von weiter weg (außer Reichweite des Schwertkämpfers).
        w.units.push(Unit::new(41_001, 2, UnitKind::Archer, (foot.0, foot.1 + 8.0)));
        let mut turned = false;
        for _ in 0..20 * 20 {
            w.step(0.05);
            let s = w.units.iter().find(|u| u.id == 41_000).expect("lebt");
            turned |= s.attack_order == Some(protocol::AttackTarget::Unit { id: 41_001 });
            if !w.units.iter().any(|u| u.id == 41_001) {
                break;
            }
        }
        assert!(turned, "wendet sich dem Schützen zu");
        // Den Schützen gibt es nicht mehr (erschlagen oder hier entfernt): zurück zum Gebäude.
        w.units.retain(|u| u.id != 41_001);
        steps(&mut w, 1.0);
        let s = w.units.iter().find(|u| u.id == 41_000).unwrap();
        assert_eq!(s.attack_order, Some(target), "weiter am Bergfried");
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

    /// Flache, leere Karte; ein Bogenschütze von 1 bei (20, 20), Speerträger halten
    /// viel aus und wehren sich nicht.
    fn open_field() -> World {
        let row = "0".repeat(60);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(40));
        let mut eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        for u in &mut eco.def.units {
            let (hp, attack, range) = match u.kind {
                UnitKind::Archer => (40, 6, 18.0),
                UnitKind::Spearman => (100_000, 0, 1.5),
                _ => continue,
            };
            (u.hp, u.attack, u.range, u.attack_seconds) = (hp, attack, range, 1.5);
            u.damage.clear();
            u.armor.clear();
        }
        eco.def.army.arrow_speed = 10.0;
        eco.def.army.arrow_hit_radius = 0.6;
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.units.push(Unit::new(50_000, 1, UnitKind::Archer, (20.5, 20.5)));
        w
    }

    /// Lässt den Speerträger (Id 50_001) mit `vel` laufen (`walks`: wirklich, sonst nur so tun) und
    /// zählt Schüsse und angerichteten Schaden.
    fn shoot_at_runner(start: (f32, f32), vel: (f32, f32), walks: bool, seconds: f32) -> (u32, u32) {
        let mut w = open_field();
        w.units.push(Unit::new(50_001, 2, UnitKind::Spearman, start));
        let mut shots = 0;
        for _ in 0..(seconds * 20.0) as u32 {
            let s = w.units.iter_mut().find(|u| u.id == 50_001).unwrap();
            s.vel = vel;
            if walks {
                s.pos = (s.pos.0 + vel.0 * 0.05, s.pos.1 + vel.1 * 0.05);
            }
            w.step_combat(0.05);
            for e in w.take_events() {
                if let ServerMsg::Shots { shots: list } = e {
                    shots += list.len() as u32;
                }
            }
        }
        // Noch fliegende Pfeile landen lassen (Ziel bleibt dabei stehen).
        w.units.iter_mut().find(|u| u.id == 50_001).unwrap().vel = (0.0, 0.0);
        let mut eco = w.economy.clone();
        eco.def.units.iter_mut().find(|u| u.kind == UnitKind::Archer).unwrap().attack = 0;
        w.set_economy(eco);
        run(&mut w, 3.0);
        (shots, w.units.iter().find(|u| u.id == 50_001).unwrap().damage)
    }

    #[test]
    fn pfeil_trifft_erst_beim_aufschlag() {
        let mut w = open_field();
        w.units.push(Unit::new(50_001, 2, UnitKind::Spearman, (32.5, 20.5)));
        w.step_combat(0.05);
        let events = w.take_events();
        let seconds = events
            .iter()
            .find_map(|e| match e {
                ServerMsg::Shots { shots } => shots.first().map(|s| s.seconds),
                _ => None,
            })
            .expect("Schuss fliegt");
        // 12 Felder, Bogen etwas länger, 10 Felder pro Sekunde.
        assert!(seconds > 1.0 && seconds < 2.0, "Flugzeit {seconds}");
        assert_eq!(w.units.iter().find(|u| u.id == 50_001).unwrap().damage, 0, "noch kein Schaden beim Abschuss");
        run(&mut w, seconds - 0.1);
        assert_eq!(w.units.iter().find(|u| u.id == 50_001).unwrap().damage, 0, "kurz vor dem Aufschlag noch nichts");
        run(&mut w, 0.2);
        assert_eq!(w.units.iter().find(|u| u.id == 50_001).unwrap().damage, 6, "beim Aufschlag getroffen");
    }

    #[test]
    fn schuetzen_halten_auf_geradeaus_laufende_vor() {
        // Quer vor dem Schützen vorbei, 2 Felder pro Sekunde.
        let (shots, damage) = shoot_at_runner((10.5, 28.5), (2.0, 0.0), true, 10.0);
        assert!(shots >= 5, "Schüsse: {shots}");
        assert!(damage >= 6 * (shots - 1), "fast alles trifft: {damage} Schaden aus {shots} Schüssen");
    }

    #[test]
    fn wer_anders_laeuft_als_gedacht_wird_verfehlt() {
        // Sieht aus, als liefe er (Tempo gesetzt), bleibt aber stehen: der Pfeil landet vor ihm.
        let (shots, damage) = shoot_at_runner((26.5, 28.5), (3.0, 0.0), false, 6.0);
        assert!(shots >= 3, "Schüsse: {shots}");
        assert_eq!(damage, 0, "alle Pfeile gehen vor ihm nieder");
        // Steht er wirklich still, trifft jeder Pfeil.
        let (shots, damage) = shoot_at_runner((26.5, 28.5), (0.0, 0.0), false, 6.0);
        assert_eq!(damage, 6 * shots);
    }

    #[test]
    fn pfeil_trifft_einen_anderen_feind_am_einschlag() {
        let mut w = open_field();
        // Ziel bleibt stehen, tut aber so, als liefe es; dort, wo der Pfeil landet, steht ein zweiter Feind.
        let mut s = Unit::new(50_001, 2, UnitKind::Spearman, (30.5, 20.5));
        s.vel = (0.0, 3.0);
        w.units.push(s);
        w.step_combat(0.05);
        let to = w
            .take_events()
            .iter()
            .find_map(|e| match e {
                ServerMsg::Shots { shots } => shots.first().map(|s| s.to),
                _ => None,
            })
            .expect("Schuss fliegt");
        assert!(to.1 > 22.0, "vorgehalten: {to:?}");
        w.units.iter_mut().find(|u| u.id == 50_001).unwrap().vel = (0.0, 0.0);
        w.units.push(Unit::new(50_002, 2, UnitKind::Spearman, to));
        let mut eco = w.economy.clone();
        eco.def.units.iter_mut().find(|u| u.kind == UnitKind::Archer).unwrap().attack_seconds = 100.0;
        w.set_economy(eco);
        run(&mut w, 2.5);
        assert_eq!(w.units.iter().find(|u| u.id == 50_001).unwrap().damage, 0);
        assert_eq!(w.units.iter().find(|u| u.id == 50_002).unwrap().damage, 6);
    }

}
