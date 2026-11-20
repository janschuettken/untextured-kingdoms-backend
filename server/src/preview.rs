//! Kleine Kartenvorschau für die Lobby: Gelände grob, im laufenden Spiel mit Gebäuden
//! und wer ausgeschieden ist (`GET /api/games/{id}/preview`).

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use protocol::{BuildingCategory, BuildingKind, GamePreview, PreviewBuilding, PreviewLook, ResourceKind, TileKind};
use serde::Deserialize;

use crate::{economy::Economy, lobby::Hub, map::GameMap, world::World};

/// Höchstens so viele Zellen je Seite.
pub const MAX_CELLS: u32 = 256;

/// Gelände der Vorschau: (step, cols, rows, Zeichen).
pub fn terrain(map: &GameMap) -> (u32, u32, u32, String) {
    let step = map.width.max(map.height).div_ceil(MAX_CELLS).max(1);
    let cols = map.width.div_ceil(step);
    let rows = map.height.div_ceil(step);
    let mut cells = String::with_capacity((cols * rows) as usize);
    for r in 0..rows {
        for c in 0..cols {
            let x = (c * step + step / 2).min(map.width - 1);
            let y = (r * step + step / 2).min(map.height - 1);
            let i = (y * map.width + x) as usize;
            let ch = if map.tiles[i] == TileKind::Water {
                '~'
            } else {
                match map.resources[i] {
                    ResourceKind::Forest => 'W',
                    ResourceKind::Stone => 'S',
                    ResourceKind::Iron => 'E',
                    ResourceKind::Farmland => 'A',
                    ResourceKind::Fertile => 'F',
                    ResourceKind::None => char::from(b'0' + map.levels[i].min(9)),
                }
            };
            cells.push(ch);
        }
    }
    (step, cols, rows, cells)
}

/// Vorschau einer Karte ohne laufendes Spiel.
pub fn of_map(map: &GameMap) -> GamePreview {
    let (step, cols, rows, cells) = terrain(map);
    GamePreview {
        width: map.width,
        height: map.height,
        step,
        cols,
        rows,
        cells,
        buildings: Vec::new(),
        defeated: Vec::new(),
        winners: None,
    }
}

fn look(economy: &Economy, kind: BuildingKind) -> PreviewLook {
    if kind == BuildingKind::Keep {
        return PreviewLook::Keep;
    }
    match economy.building(kind).map(|d| d.category) {
        Some(BuildingCategory::Road) => PreviewLook::Road,
        Some(BuildingCategory::Fortification) => PreviewLook::Wall,
        _ => PreviewLook::Building,
    }
}

/// Vorschau eines laufenden Spiels; Gebäude nur, wenn `with_buildings`.
pub fn of_world(world: &World, with_buildings: bool) -> GamePreview {
    let mut p = of_map(&world.map);
    if with_buildings {
        p.buildings = world
            .buildings
            .iter()
            .map(|b| PreviewBuilding {
                owner: b.owner,
                x: b.x,
                y: b.y,
                w: b.w,
                h: b.h,
                look: look(&world.economy, b.kind),
            })
            .collect();
    }
    p.defeated = world.eliminated();
    p.winners = world.winners().map(|w| w.to_vec());
    p
}

#[derive(Deserialize)]
pub struct PreviewQuery {
    token: Option<String>,
}

pub async fn handler(State(hub): State<Arc<Hub>>, Path(id): Path<u32>, Query(q): Query<PreviewQuery>) -> Response {
    let Some(game) = hub.game(id) else {
        return (StatusCode::NOT_FOUND, "Spiel nicht gefunden").into_response();
    };
    let token = q.token.unwrap_or_default();
    let preview = match game.running.get() {
        Some(m) => {
            // Gebäude sehen nur Mitspieler (die sehen sie im Spiel ohnehin) und Admins.
            let member = !token.is_empty() && hub.membership(id, &token).is_some();
            let with_buildings = member || (!token.is_empty() && hub.admin.check(&token).await);
            let world = m.world.lock().unwrap();
            of_world(&world, with_buildings)
        }
        None => match game.waiting_preview() {
            Some(p) => (*p).clone(),
            None => return (StatusCode::NOT_FOUND, "Karte ungültig").into_response(),
        },
    };
    axum::Json(preview).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grosse_karte_wird_verkleinert() {
        let mut text = String::from("[terrain]\n");
        for y in 0..600 {
            let row: String = (0..520).map(|x| if x < 10 { '~' } else if y > 500 { '3' } else { '1' }).collect();
            text.push_str(&row);
            text.push('\n');
        }
        text.push_str("spawn 100 100\n");
        let map = GameMap::parse(&text).unwrap();
        let p = of_map(&map);
        assert_eq!(p.step, 3);
        assert_eq!((p.cols, p.rows), (174, 200));
        assert_eq!(p.cells.len(), (p.cols * p.rows) as usize);
        assert!(p.cells.starts_with("~~~1"));
        assert!(p.cells.ends_with('3'));
    }
}
