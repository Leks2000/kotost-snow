fn kotost_snow_tick(game: &mut crate::game::Game<'_>) {
    use horizon_client_render::kotost_snow as ks;
    use std::sync::atomic::{AtomicU32, Ordering};
    static TICKS: AtomicU32 = AtomicU32::new(0);
    if TICKS.fetch_add(1, Ordering::Relaxed) % 20 == 0 {
        ks::reload_tune();
    }
    // Which block states are snow, once (the registry lives as long as the game).
    if !ks::kinds_ready() {
        let registry = game.pack.assets.registry;
        ks::init_kinds(registry.len(), |i| {
            match registry.state(horizon_world::registry::BlockStateId(i)) {
                Some(s) if s.name == "minecraft:snow" => s
                    .properties
                    .get("layers")
                    .and_then(|v| v.parse::<u8>().ok())
                    .unwrap_or(1)
                    .clamp(1, 8),
                Some(s) if s.name == "minecraft:snow_block" => 9,
                // Grass and bushes stand in the snow (the lower half of tall ones).
                Some(s)
                    if matches!(
                        s.name.as_str(),
                        "minecraft:short_grass"
                            | "minecraft:fern"
                            | "minecraft:dead_bush"
                            | "minecraft:bush"
                            | "minecraft:short_dry_grass"
                            | "minecraft:tall_dry_grass"
                            | "minecraft:sweet_berry_bush"
                    ) || (matches!(s.name.as_str(), "minecraft:tall_grass" | "minecraft:large_fern")
                        && s.properties.get("half").map(|h| h.as_str()) == Some("lower")) =>
                {
                    ks::PLANT_KIND
                }
                _ => 0,
            }
        });
    }
    let tune = ks::tune();
    if !tune.enabled {
        return;
    }
    let Some(p) = game.player.as_ref() else {
        ks::clear();
        return;
    };
    let me = [p.position.x, p.position.y, p.position.z];
    // A block edit rebuilds only the sections within one block of it, but a snow surface looks two cells
    // around (its neighbours' neighbours): rebuild the snowy sections beside an edited one too, or snow
    // stays drawn where it is gone (hanging in the air) or missing where it lies (invisible).
    {
        use std::sync::Mutex;
        static ADDED: Mutex<Vec<(i32, i32, i32)>> = Mutex::new(Vec::new());
        let mut added = ADDED.lock().unwrap_or_else(|e| e.into_inner());
        let mut src: Vec<(i32, i32, i32)> = Vec::new();
        'cols: for ((x, z), mask) in game.world.dirty_sections.columns() {
            for sy in mask.iter() {
                let k = (x, sy as i32, z);
                if !added.contains(&k) {
                    src.push(k);
                    if src.len() > 24 {
                        break 'cols;
                    }
                }
            }
        }
        added.clear();
        // Many at once: chunks arriving (rebuilt whole anyway), not an edit.
        if !src.is_empty() && src.len() <= 24 {
            let layout = game.world.layout();
            let min_y = layout.min_y();
            for &(sx, sy, sz) in &src {
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        let k = (sx + dx, sy, sz + dz);
                        if (dx, dz) == (0, 0) || src.contains(&k) || added.contains(&k) {
                            continue;
                        }
                        let lo = [k.0 * 16, min_y + sy * 16, k.2 * 16];
                        let hi = [lo[0] + 15, lo[1] + 15, lo[2] + 15];
                        if game.world.may_hold(lo, hi, |id| ks::is_snow(ks::kind(id.0))) {
                            added.push(k);
                        }
                    }
                }
            }
            for &k in added.iter() {
                game.world.dirty_sections.insert(k);
            }
        }
    }
    // Snow near the viewer is drawn finely: rebuild the snowy sections that enter or leave that ring.
    let (vx, vz) = (me[0].floor() as i32, me[2].floor() as i32);
    let old = ks::set_viewer(vx, vz);
    if old != Some((vx, vz)) {
        let layout = game.world.layout();
        let (min_y, sections) = (layout.min_y(), layout.sections() as i32);
        let psy = (me[1].floor() as i32 - min_y).div_euclid(16);
        let (cx, cz) = (vx >> 4, vz >> 4);
        for sx in cx - 3..=cx + 3 {
            for sz in cz - 3..=cz + 3 {
                if ks::detailed(sx, sz, old) == ks::detailed(sx, sz, Some((vx, vz))) {
                    continue;
                }
                for sy in (psy - 1).max(0)..=(psy + 1).min(sections - 1) {
                    let lo = [sx * 16, min_y + sy * 16, sz * 16];
                    let hi = [lo[0] + 15, lo[1] + 15, lo[2] + 15];
                    if game.world.may_hold(lo, hi, |id| ks::is_snow(ks::kind(id.0))) {
                        game.world.dirty_sections.insert((sx, sy, sz));
                    }
                }
            }
        }
    }
    let mut walkers = vec![ks::Walker {
        id: ks::LOCAL,
        pos: me,
        yaw: p.yaw,
        width: 0.6,
        on_ground: p.ground.on_ground,
    }];
    if tune.mobs {
        for m in game.mobs.iter() {
            if m.removed || !m.alive() {
                continue;
            }
            let (dx, dz) = (m.position.x - me[0], m.position.z - me[2]);
            if dx * dx + dz * dz > 64.0 * 64.0 || (m.position.y - me[1]).abs() > 32.0 {
                continue;
            }
            walkers.push(ks::Walker {
                id: m.id,
                pos: [m.position.x, m.position.y, m.position.z],
                yaw: m.body_yaw,
                width: m.kind.width as f32,
                on_ground: m.on_ground,
            });
        }
    }
    let (changed, puffs) = {
        let world = &game.world;
        ks::tick(&walkers, |x, y, z| world.block(x, y, z).0)
    };
    // Snowfall around the camera: snow (not rain) where the player is.
    let snowing = {
        let (bx, by, bz) = (me[0].floor() as i32, me[1].floor() as i32, me[2].floor() as i32);
        let biomes = match &game.world_biomes {
            Some(b) => b,
            None => game.res.biomes(),
        };
        game.world
            .chunks
            .get(&(bx >> 4, bz >> 4))
            .is_some_and(|c| biomes.get(c.biome(bx, by, bz)).snows_at(bx, by, bz) == Some(true))
    };
    let (rain, thunder) = (game.world.weather.rain_level.1, game.world.weather.thunder_level.1);
    let eye = [me[0], me[1] + 1.6, me[2]];
    let (flakes, rebuild) = {
        let world = &game.world;
        ks::snowfall(eye, snowing, rain, thunder, |x, y, z| world.motion_blocking(x, z, |id| id.0 != 0) <= y)
    };
    // The fresh powder grew or settled: rebuild the snow around.
    if rebuild {
        let layout = game.world.layout();
        let (min_y, sections) = (layout.min_y(), layout.sections() as i32);
        let psy = (me[1].floor() as i32 - min_y).div_euclid(16);
        let (cx, cz) = ((me[0].floor() as i32) >> 4, (me[2].floor() as i32) >> 4);
        for sx in cx - 4..=cx + 4 {
            for sz in cz - 4..=cz + 4 {
                for sy in (psy - 2).max(0)..=(psy + 1).min(sections - 1) {
                    let lo = [sx * 16, min_y + sy * 16, sz * 16];
                    let hi = [lo[0] + 15, lo[1] + 15, lo[2] + 15];
                    if game.world.may_hold(lo, hi, |id| ks::is_snow(ks::kind(id.0))) {
                        game.world.dirty_sections.insert((sx, sy, sz));
                    }
                }
            }
        }
    }
    {
        use horizon_client::particles::{ParticleOptions, ParticleType};
        let flake = ParticleOptions::simple(ParticleType::Snowflake);
        let powder = ParticleOptions::simple(ParticleType::WhiteAsh);
        for f in flakes {
            game.native_add_particle(if f.powder { &powder } else { &flake }, f.at, f.velocity);
        }
        for p in puffs {
            for k in 0..3 {
                let side = (f64::from(k) - 1.0) * 0.035;
                game.native_add_particle(&flake, p, [side, 0.05 + 0.02 * f64::from(k), -side]);
            }
        }
    }
    if changed.is_empty() {
        return;
    }
    let layout = game.world.layout();
    let (min_y, sections) = (layout.min_y(), layout.sections() as i32);
    for (bx, by, bz) in changed {
        let sy = (by - min_y).div_euclid(16);
        if !(0..sections).contains(&sy) {
            continue;
        }
        // A block's surface also reads its neighbours' prints: their sections too.
        for dx in [-1, 0, 1] {
            for dz in [-1, 0, 1] {
                game.world.dirty_sections.insert(((bx + dx) >> 4, sy, (bz + dz) >> 4));
            }
        }
    }
}
