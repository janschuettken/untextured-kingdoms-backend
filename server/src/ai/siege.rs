//! Belagerung durch Computergegner.
//!
//! Ist die Burg eines Gegners zu (kein Weg zum Bergfried), baut er eine
//! Belagerungswerkstatt, bildet ein paar Baumeister aus und lässt sie nahe der eigenen
//! Burg Katapulte bauen (ab Werkstatt-Stufe 2 auch einen Tribok, wenn genug da ist). Fertige,
//! bediente Geräte ziehen mit der nächsten Welle los und werfen auf das Mauerstück, an dem
//! der Weg zum Bergfried endet (siehe `attack::bombard`).

use protocol::{BuildingKind, ClientMsg, StockState, UnitKind};

use super::{rally_point, Computer, Layout};
use crate::world::World;

/// So viele Baumeister hält er.
const BUILDERS: u32 = 4;
/// So viele Geräte (fertig oder im Bau) hält er.
const DEVICES: usize = 2;
/// Alle so viele Sekunden prüft er, ob ein Gegner zu ist (Wegsuche je Gegner).
const CHECK_SECONDS: f32 = 20.0;

impl Computer {
    pub(super) fn siege_works(&mut self, world: &World, stock: &StockState, layout: &Layout, cmds: &mut Vec<ClientMsg>) {
        if self.profile.attack.is_none() {
            return;
        }
        let p = self.player;
        if self.clock >= self.next_siege_check {
            self.next_siege_check = self.clock + CHECK_SECONDS;
            let foes: Vec<u32> = world
                .buildings
                .iter()
                .filter(|b| b.kind == BuildingKind::Keep && b.owner != p && b.owner != 0 && world.can_attack(p, b.owner))
                .map(|b| b.owner)
                .collect();
            self.siege_wanted = foes.into_iter().any(|f| crate::world::castle_closed(world, f));
        }
        let workshops: Vec<_> = world.buildings.iter().filter(|b| b.owner == p && b.kind == BuildingKind::SiegeWorkshop).collect();
        // Erst die eigene Burg zu: vorher braucht er Holz und Bretter für Mauer und Tore.
        if workshops.is_empty() && (!self.siege_wanted || (self.profile.walls && !self.closed)) {
            return;
        }
        if workshops.is_empty() {
            self.extra.insert(BuildingKind::SiegeWorkshop, 1);
            return;
        }
        // Baumeister ausbilden.
        let builders: Vec<&crate::world::Unit> = world.units.iter().filter(|u| u.owner == p && u.kind == UnitKind::Builder).collect();
        let queued: u32 = workshops.iter().map(|b| b.training.iter().filter(|&&k| k == UnitKind::Builder).count() as u32).sum();
        let have = builders.len() as u32 + queued;
        let taler = world.economy.unit(UnitKind::Builder).map_or(0, |d| d.taler);
        if have < BUILDERS && stock.taler >= taler * (BUILDERS - have) {
            cmds.push(ClientMsg::TrainUnits { building_id: workshops[0].id, kind: UnitKind::Builder, count: BUILDERS - have, spread: false });
        }
        // Geräte: fertige und Baustellen.
        let devices = world.units.iter().filter(|u| u.owner == p && matches!(u.kind, UnitKind::Catapult | UnitKind::Trebuchet)).count();
        let sites = world.buildings.iter().filter(|b| b.owner == p && b.site.is_some()).count();
        if devices + sites >= DEVICES || !self.siege_wanted {
            return;
        }
        let free: Vec<u32> = builders.iter().filter(|u| u.crew_of().is_none() && !u.is_moving()).map(|u| u.id).collect();
        if free.len() < 2 || sites > 0 {
            return;
        }
        // Tribok, wenn freigeschaltet und genug Material da ist, sonst Katapult.
        let enough = |k: UnitKind| world.economy.unit(k).is_some_and(|d| d.cost.iter().all(|c| stock.goods.iter().any(|g| g.good == c.good && g.amount >= c.amount)));
        let kind = if world.siege_unlocked(p, UnitKind::Trebuchet) && enough(UnitKind::Trebuchet) {
            UnitKind::Trebuchet
        } else if world.siege_unlocked(p, UnitKind::Catapult) && enough(UnitKind::Catapult) {
            UnitKind::Catapult
        } else {
            return;
        };
        let Some(home) = rally_point(world, p, layout) else { return };
        // Etwas neben dem Sammelpunkt, an einer freien Stelle (Ringe nach außen).
        let spot = (0..24u32).find_map(|i| {
            self.siege_try = self.siege_try.wrapping_add(1);
            let a = self.siege_try as f32 * 2.4;
            let r = 4.0 + (i / 6) as f32 * 3.0;
            let (x, y) = ((home.0 + a.cos() * r).max(0.0) as u32, (home.1 + a.sin() * r).max(0.0) as u32);
            world.siege_site_free(p, kind, (x, y)).then_some((x, y))
        });
        let Some((x, y)) = spot else { return };
        cmds.push(ClientMsg::BuildSiege { unit_ids: free.into_iter().take(3).collect(), kind, x, y, backpack: true });
    }
}
