// Kotost snow: a soft, smooth snow surface and footprints pressed into it (client only).
// Shared state between the mesher hook (worker threads) and the client tick hook.
// Numbers can be tuned while playing: `config/kotost-snow.txt` in the game's folder (key = value).
// The imprint idea follows Soft Imprints by 0Templ (MIT); the code here is written for Horizon.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock, RwLockReadGuard};
use std::time::{Instant, SystemTime};

/// Imprint cells per block side (one per texel).
pub const CELLS: i32 = 16;
/// Depth unit of a stored cell, in blocks (a quarter texel).
pub const UNIT: f32 = 1.0 / 64.0;
/// The local player's id in the walker tracks.
pub const LOCAL: u32 = u32::MAX;

// --- settings -------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Tune {
    pub enabled: bool,
    /// How much higher the snow is drawn than its collision (blocks): feet sink into it.
    pub lift: f32,
    /// A player's footprint depth (blocks); mobs scale it by their width.
    pub depth: f32,
    /// Distance between two footprints of a walking player (blocks).
    pub stride: f32,
    /// Seconds a footprint stays whole, then seconds it takes to fill in.
    pub fade_start: f32,
    pub fade_len: f32,
    pub mobs: bool,
    /// Strength of the soft shading of slopes and footprints (0 = none).
    pub shade: f32,
    /// Blue in the shade (0 = grey shading, 1 = strong blue).
    pub blue: f32,
    /// Snowfall effects: flakes in the wind, gusts, the blizzard's white haze.
    pub weather: bool,
    /// How many flakes (1 = as designed).
    pub flakes: f32,
    /// Snow dust kicked up by footsteps.
    pub dust: bool,
    /// Grass and bushes next to snow are white with frost.
    pub frost: bool,
    pub debug: bool,
}

impl Default for Tune {
    fn default() -> Self {
        Self {
            enabled: true,
            lift: 3.0 / 16.0,
            depth: 5.0 / 16.0,
            stride: 0.72,
            fade_start: 150.0,
            fade_len: 150.0,
            mobs: true,
            shade: 0.7,
            blue: 0.6,
            weather: true,
            flakes: 1.0,
            dust: true,
            frost: true,
            debug: false,
        }
    }
}

static TUNE: RwLock<Option<Tune>> = RwLock::new(None);
static TUNE_STAMP: Mutex<Option<SystemTime>> = Mutex::new(None);

pub fn tune() -> Tune {
    TUNE.read().ok().and_then(|t| *t).unwrap_or_default()
}

const DEFAULT_FILE: &str = "# Kotost snow (native mod). Saved changes apply within a second; footprints and the\n\
# snow surface are rebuilt as you walk. px = texels (1/16 block).\n\
enabled = 1\n\
# snow drawn this much above its collision: feet sink in (0..6)\n\
lift_px = 3\n\
# a player's footprint depth (mobs scale it by size)\n\
depth_px = 5\n\
# blocks between two footprints of a walking player\n\
stride = 0.72\n\
# seconds a footprint stays, then seconds it takes to fill in\n\
fade_start = 150\n\
fade_len = 150\n\
# mobs leave footprints too\n\
mobs = 1\n\
# soft shading of slopes and footprints, and how blue the shade is (0..2)\n\
shade = 0.7\n\
blue = 0.6\n\
# snowfall: flakes in the wind, gusts; a blizzard (thunder) brings a white haze\n\
weather = 1\n\
# how many flakes (0..3)\n\
flakes = 1\n\
# snow dust kicked up by footsteps\n\
dust = 1\n\
# grass and bushes next to snow white with frost\n\
frost = 1\n\
# 1 = write kotost-snow-debug.log\n\
debug = 0\n";

fn settings_path() -> PathBuf {
    crate::logging::game_home().join("config").join("kotost-snow.txt")
}

/// Re-reads the settings file when it changed (call it about once a second).
pub fn reload_tune() {
    let path = settings_path();
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if modified.is_none() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, DEFAULT_FILE);
    }
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    {
        let mut stamp = TUNE_STAMP.lock().unwrap_or_else(|e| e.into_inner());
        if *stamp == modified && TUNE.read().map(|t| t.is_some()).unwrap_or(false) {
            return;
        }
        *stamp = modified;
    }
    let mut t = Tune::default();
    if let Ok(text) = std::fs::read_to_string(&path) {
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let num = v.parse::<f32>().ok();
            let on = matches!(v, "1" | "on" | "true" | "yes");
            match (k, num) {
                ("enabled", _) => t.enabled = on,
                ("lift_px", Some(n)) => t.lift = n.clamp(0.0, 8.0) / 16.0,
                ("depth_px", Some(n)) => t.depth = n.clamp(0.0, 12.0) / 16.0,
                ("stride", Some(n)) => t.stride = n.clamp(0.2, 3.0),
                ("fade_start", Some(n)) => t.fade_start = n.max(0.0),
                ("fade_len", Some(n)) => t.fade_len = n.max(1.0),
                ("mobs", _) => t.mobs = on,
                ("shade", Some(n)) => t.shade = n.clamp(0.0, 2.0),
                ("blue", Some(n)) => t.blue = n.clamp(0.0, 2.0),
                ("weather", _) => t.weather = on,
                ("flakes", Some(n)) => t.flakes = n.clamp(0.0, 3.0),
                ("dust", _) => t.dust = on,
                ("frost", _) => t.frost = on,
                ("debug", _) => t.debug = on,
                _ => {}
            }
        }
    }
    if let Ok(mut slot) = TUNE.write() {
        *slot = Some(t);
    }
    log(&format!("settings {t:?}"));
}

pub fn log(text: &str) {
    if !tune().debug {
        return;
    }
    use std::io::Write;
    let path = crate::logging::game_home().join("kotost-snow-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{:9.2}] {text}", now());
    }
}

// --- clock, re-entrancy ---------------------------------------------------------------------------

static START: OnceLock<Instant> = OnceLock::new();

/// Seconds since the game started.
pub fn now() -> f32 {
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

thread_local! {
    static NESTED: Cell<bool> = const { Cell::new(false) };
}
pub fn nested() -> bool {
    NESTED.with(Cell::get)
}
pub fn set_nested(on: bool) {
    NESTED.with(|n| n.set(on));
}

// --- snow blocks ----------------------------------------------------------------------------------

/// By block state id: 0 not snow, 1..=8 snow layers, 9 a snow block, PLANT_KIND grass or a bush that snow
/// fills around.
static KINDS: OnceLock<Vec<u8>> = OnceLock::new();

pub fn init_kinds(len: usize, kind_of: impl Fn(u32) -> u8) {
    KINDS.get_or_init(|| {
        let kinds: Vec<u8> = (0..len as u32).map(kind_of).collect();
        let snow = kinds.iter().filter(|&&k| k > 0).count();
        log(&format!("{snow} snow states of {len}"));
        kinds
    });
}
pub fn kinds_ready() -> bool {
    KINDS.get().is_some()
}
#[inline]
pub fn kind(id: u32) -> u8 {
    KINDS.get().and_then(|k| k.get(id as usize).copied()).unwrap_or(0)
}
#[inline]
pub fn is_layer(kind: u8) -> bool {
    (1..=8).contains(&kind)
}
/// A grass or a bush: snow lies around it, it stands in the snow.
pub const PLANT_KIND: u8 = 12;
#[inline]
pub fn is_snow(kind: u8) -> bool {
    is_layer(kind) || kind == 9
}

// --- footprints -----------------------------------------------------------------------------------

pub struct Imprint {
    /// Depth per cell (16 x 16, z rows), in `UNIT`s; negative = the raised rim.
    pub d: [i8; 256],
    /// When the block was last stamped (seconds).
    pub t: f32,
}

#[derive(Default)]
pub struct Store {
    pub map: HashMap<(i32, i32, i32), Imprint>,
}

static STORE: OnceLock<RwLock<Store>> = OnceLock::new();
fn store() -> &'static RwLock<Store> {
    STORE.get_or_init(|| RwLock::new(Store::default()))
}

fn fade(t: f32, now: f32, tune: &Tune) -> f32 {
    let age = now - t;
    if age <= tune.fade_start {
        1.0
    } else {
        (1.0 - (age - tune.fade_start) / tune.fade_len).clamp(0.0, 1.0)
    }
}

/// What the mesher reads: footprint depths with their fading.
pub struct View<'a> {
    store: RwLockReadGuard<'a, Store>,
    now: f32,
    tune: Tune,
}

pub fn view() -> View<'static> {
    let store = store().read().unwrap_or_else(|e| e.into_inner());
    View { store, now: now(), tune: tune() }
}

impl View<'_> {
    pub fn is_empty(&self) -> bool {
        self.store.map.is_empty()
    }
    /// Whether this block or one around it holds footprints (its surface is then drawn finely).
    pub fn near(&self, x: i32, y: i32, z: i32) -> bool {
        if self.store.map.is_empty() {
            return false;
        }
        (-1..=1).any(|dx| (-1..=1).any(|dz| self.store.map.contains_key(&(x + dx, y, z + dz))))
    }
    /// The depth (blocks, faded; negative = rim) of texel cell `gx, gz` (world texel coordinates) at height `y`.
    pub fn cell(&self, gx: i32, y: i32, gz: i32) -> f32 {
        let key = (gx.div_euclid(CELLS), y, gz.div_euclid(CELLS));
        let Some(imp) = self.store.map.get(&key) else {
            return 0.0;
        };
        let i = (gz.rem_euclid(CELLS) * CELLS + gx.rem_euclid(CELLS)) as usize;
        f32::from(imp.d[i]) * UNIT * fade(imp.t, self.now, &self.tune)
    }
    /// The depth at grid point `gx, gz` (a texel corner): the mean of the four cells around it.
    pub fn at(&self, gx: i32, y: i32, gz: i32) -> f32 {
        (self.cell(gx - 1, y, gz - 1) + self.cell(gx, y, gz - 1) + self.cell(gx - 1, y, gz) + self.cell(gx, y, gz))
            * 0.25
    }
}

/// Something walking: the local player, another player or a mob.
#[derive(Clone, Copy, Debug)]
pub struct Walker {
    pub id: u32,
    pub pos: [f64; 3],
    pub yaw: f32,
    pub width: f32,
    pub on_ground: bool,
}

struct Track {
    last: [f64; 3],
    dir: [f32; 2],
    dist: f32,
    left: bool,
    air: bool,
    still: bool,
    seen: u64,
}

#[derive(Default)]
struct Walk {
    tracks: HashMap<u32, Track>,
    tick: u64,
    stamps: u64,
}

static WALK: Mutex<Option<Walk>> = Mutex::new(None);

/// The snow block an entity stands in or on: (its y, its kind).
fn snow_under(pos: [f64; 3], block: &impl Fn(i32, i32, i32) -> u32) -> Option<(i32, u8)> {
    let (bx, bz) = (pos[0].floor() as i32, pos[2].floor() as i32);
    let fy = (pos[1] + 0.01).floor() as i32;
    let k = kind(block(bx, fy, bz));
    if is_layer(k) {
        return Some((fy, k));
    }
    let k = kind(block(bx, fy - 1, bz));
    // Standing on top of a full stack of layers or a snow block.
    if k == 8 || k == 9 {
        return Some((fy - 1, k));
    }
    None
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Presses one foot into the snow at `c` (x, z), the foot pointing along `dir`.
fn stamp(
    map: &mut HashMap<(i32, i32, i32), Imprint>,
    dirty: &mut HashSet<(i32, i32, i32)>,
    block: &impl Fn(i32, i32, i32) -> u32,
    c: [f32; 2],
    y: i32,
    dir: [f32; 2],
    scale: f32,
    depth: f32,
    now: f32,
    tune: &Tune,
) {
    let perp = [-dir[1], dir[0]];
    let a = 3.6 / 16.0 * scale; // half length
    let b = 1.6 / 16.0 * scale; // half width
    let reach = a * 1.6;
    let cells = CELLS as f32;
    let (x0, x1) = (((c[0] - reach) * cells).floor() as i32, ((c[0] + reach) * cells).ceil() as i32);
    let (z0, z1) = (((c[1] - reach) * cells).floor() as i32, ((c[1] + reach) * cells).ceil() as i32);
    let mut is_snow: HashMap<(i32, i32), bool> = HashMap::new();
    for gz in z0..=z1 {
        for gx in x0..=x1 {
            let w = [(gx as f32 + 0.5) / cells - c[0], (gz as f32 + 0.5) / cells - c[1]];
            let along = w[0] * dir[0] + w[1] * dir[1];
            let across = w[0] * perp[0] + w[1] * perp[1];
            // A boot: the toe a little wider than the heel.
            let bw = b * (1.0 + 0.18 * (along / a).clamp(-1.0, 1.0));
            let r = ((along / a).powi(2) + (across / bw).powi(2)).sqrt();
            let value = if r < 1.0 {
                depth * (1.0 - smoothstep(0.7, 1.0, r))
            } else if r < 1.45 {
                -depth * 0.15 * (1.0 - (r - 1.0) / 0.45)
            } else {
                continue;
            };
            let (bx, bz) = (gx.div_euclid(CELLS), gz.div_euclid(CELLS));
            let snow = *is_snow.entry((bx, bz)).or_insert_with(|| self::is_snow(kind(block(bx, y, bz))));
            if !snow {
                continue;
            }
            let units = (value / UNIT).round().clamp(-127.0, 127.0) as i8;
            if units == 0 {
                continue;
            }
            let imp = map.entry((bx, y, bz)).or_insert_with(|| Imprint { d: [0; 256], t: now });
            // Re-stamping a fading block: keep its older prints as faded as they are now.
            let f = fade(imp.t, now, tune);
            if f < 1.0 {
                for d in imp.d.iter_mut() {
                    *d = (f32::from(*d) * f).round() as i8;
                }
            }
            imp.t = now;
            let i = (gz.rem_euclid(CELLS) * CELLS + gx.rem_euclid(CELLS)) as usize;
            let d = &mut imp.d[i];
            if units > 0 {
                *d = (*d).max(units);
            } else if *d <= 0 {
                *d = (*d).min(units);
            }
            dirty.insert((bx, y, bz));
        }
    }
}

/// One client tick: follows every walker and presses footprints where they step on snow.
/// Returns the snow blocks whose surface changed (their sections need meshing again) and where snow dust
/// flies up from a step.
pub fn tick(walkers: &[Walker], block: impl Fn(i32, i32, i32) -> u32) -> (Vec<(i32, i32, i32)>, Vec<[f64; 3]>) {
    let mut puffs = Vec::new();
    let tune = tune();
    let now = now();
    let mut dirty = HashSet::new();
    let mut guard = WALK.lock().unwrap_or_else(|e| e.into_inner());
    let walk = guard.get_or_insert_with(Walk::default);
    walk.tick += 1;
    let tick = walk.tick;
    {
        let mut store = store().write().unwrap_or_else(|e| e.into_inner());
        for w in walkers {
            let tr = walk.tracks.entry(w.id).or_insert(Track {
                last: w.pos,
                dir: [0.0, 1.0],
                dist: 0.0,
                left: false,
                air: false,
                still: false,
                seen: tick,
            });
            let (dx, dz) = ((w.pos[0] - tr.last[0]) as f32, (w.pos[2] - tr.last[2]) as f32);
            let moved = (dx * dx + dz * dz).sqrt();
            tr.seen = tick;
            tr.last = w.pos;
            if moved > 2.5 {
                // A teleport.
                tr.dist = 0.0;
                continue;
            }
            if moved > 0.003 {
                tr.dir = [dx / moved, dz / moved];
            } else if tr.dist == 0.0 {
                let yaw = w.yaw.to_radians();
                tr.dir = [-yaw.sin(), yaw.cos()];
            }
            let under = snow_under(w.pos, &block);
            let grounded = match under {
                Some((y, k)) => {
                    let top = y as f64 + if k == 9 { 1.0 } else { f64::from(k - 1) / 8.0 };
                    w.on_ground || (w.pos[1] - top).abs() < 0.2
                }
                None => w.on_ground,
            };
            let Some((y, _)) = under.filter(|_| grounded) else {
                tr.air = !grounded;
                tr.dist = 0.0;
                tr.still = false;
                continue;
            };
            // Bigger mobs step longer and deeper, but their feet grow slower than their bodies.
            let body = (w.width / 0.6).clamp(0.35, 2.5);
            let scale = body.sqrt().clamp(0.6, 1.3);
            // Faster = longer steps.
            let speed = moved;
            let stride = tune.stride * body.clamp(0.5, 1.8) * (1.0 + 0.45 * ((speed - 0.22) / 0.07).clamp(0.0, 1.0));
            let depth = tune.depth * body.sqrt().clamp(0.55, 1.5);
            let side = 2.1 / 16.0 * body.clamp(0.6, 2.0);
            let dir = tr.dir;
            let at = [w.pos[0] as f32, w.pos[2] as f32];
            let foot = move |left: bool| {
                let s = if left { side } else { -side };
                [at[0] - dir[1] * s, at[1] + dir[0] * s]
            };
            let mut feet: Vec<[f32; 2]> = Vec::new();
            if tr.air {
                // Landing: both feet.
                feet.push(foot(true));
                feet.push(foot(false));
                tr.dist = 0.0;
            } else if !tr.still && moved < 0.003 {
                feet.push(foot(true));
                feet.push(foot(false));
            } else {
                tr.dist += moved;
                while tr.dist >= stride {
                    tr.dist -= stride;
                    feet.push(foot(tr.left));
                    tr.left = !tr.left;
                }
            }
            tr.air = false;
            tr.still = true;
            for c in feet {
                stamp(&mut store.map, &mut dirty, &block, c, y, dir, scale, depth, now, &tune);
                walk.stamps += 1;
                if tune.dust && speed > 0.12 {
                    puffs.push([f64::from(c[0]), w.pos[1] + 0.15, f64::from(c[1])]);
                }
            }
        }
        // Fading prints: rebuilt every 5 s; the filled-in ones are dropped.
        if tick % 100 == 0 {
            let end = tune.fade_start + tune.fade_len;
            store.map.retain(|&pos, imp| {
                let age = now - imp.t;
                if age > tune.fade_start {
                    dirty.insert(pos);
                }
                age <= end
            });
            if tune.debug {
                log(&format!(
                    "tick {tick}: {} blocks with prints, {} walkers, {} stamps so far",
                    store.map.len(),
                    walkers.len(),
                    walk.stamps
                ));
            }
        }
    }
    walk.tracks.retain(|_, t| tick - t.seen < 200);
    (dirty.into_iter().collect(), puffs)
}

/// Leaving a world: forget every footprint.
pub fn clear() {
    if let Ok(mut s) = store().write() {
        if !s.map.is_empty() {
            s.map.clear();
        }
    }
    if let Ok(mut w) = WALK.lock() {
        if let Some(w) = w.as_mut() {
            w.tracks.clear();
        }
    }
}

// --- the surface's shading ------------------------------------------------------------------------

/// The brightness of a surface with normal `n`, darker at the bottom of a print (`dent` 0..1),
/// as per-channel factors (r, g, b): the shade turns blue.
pub fn shade(n: [f32; 3], dent: f32, tune: &Tune) -> [f32; 3] {
    let l = [0.32f32, 1.0, 0.5];
    let ll = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
    let dot = (n[0] * l[0] + n[1] * l[1] + n[2] * l[2]) / ll;
    let flat = l[1] / ll;
    let mut f = (dot / flat).clamp(0.4, 1.12);
    f *= 1.0 - 0.2 * dent.clamp(0.0, 1.0);
    let f = 1.0 + (f - 1.0) * tune.shade;
    if f >= 1.0 {
        return [f; 3];
    }
    let k = 1.0 - f;
    let b = tune.blue;
    [
        (1.0 - k * (1.0 + 0.15 * b)).max(0.0),
        (1.0 - k * (1.0 + 0.02 * b)).max(0.0),
        (1.0 - k * (1.0 - 0.45 * b)).max(0.0),
    ]
}

// --- the section being meshed (one per mesher thread) ------------------------------------------------

/// Cells of the snapshot: a section plus two blocks around it (a cell beside the section, drawn by it,
/// needs its own neighbours).
pub const SECTION_CELLS: usize = 20 * 20 * 20;
/// A snapshot cell: 0 open, 1..=8 snow layers, 9 snow block, WALL a full solid block, PLANT grass or a bush.
pub const WALL: u8 = 10;
pub const PLANT: u8 = 11;

#[inline]
pub fn cell_index(x: i32, y: i32, z: i32) -> usize {
    (((y + 2) * 20 + (z + 2)) * 20 + (x + 2)) as usize
}

type Vertex = crate::mesher::TerrainVertex;

struct Section {
    /// The section's lowest corner in the world.
    origin: [i32; 3],
    /// Its snow and walls; None: no snow in it.
    classes: Option<Box<[u8; SECTION_CELLS]>>,
    /// What each sprite layer is (0 other, 1 snow, 2 a plant), for the render assets at this address.
    sprites: Option<(usize, Vec<u8>, Option<u16>)>,
}

thread_local! {
    static SECTION: std::cell::RefCell<Section> =
        const { std::cell::RefCell::new(Section { origin: [0; 3], classes: None, sprites: None }) };
}

/// A section's blocks are about to be read (`Padded::fill`): where it is.
pub fn section_origin(origin: [i32; 3]) {
    SECTION.with(|s| {
        let mut s = s.borrow_mut();
        s.origin = origin;
        s.classes = None;
    });
}
/// Its blocks are read (`solid_rows`): its snow and walls.
pub fn section_classes(classes: Option<Box<[u8; SECTION_CELLS]>>) {
    SECTION.with(|s| s.borrow_mut().classes = classes);
}
pub fn section_active() -> bool {
    SECTION.with(|s| s.borrow().classes.is_some())
}
/// Snow's sprite and the plants that frost: their names and kinds (1 snow, 2 a tinted plant, 3 a dead bush:
/// drawn as a frosted fern in snow).
pub const SPRITES: &[(&str, u8)] = &[
    ("minecraft:block/snow", 1),
    ("minecraft:block/short_grass", 2),
    ("minecraft:block/tall_grass_top", 2),
    ("minecraft:block/tall_grass_bottom", 2),
    ("minecraft:block/fern", 2),
    ("minecraft:block/large_fern_top", 2),
    ("minecraft:block/large_fern_bottom", 2),
    ("minecraft:block/dead_bush", 3),
    ("minecraft:block/bush", 2),
    ("minecraft:block/short_dry_grass", 2),
    ("minecraft:block/tall_dry_grass", 2),
];

/// What sprite `layer` is: 0 other, 1 snow, 2 a tinted plant, 3 a dead bush (looked up once per render assets).
/// The fern's sprite layer, which a dead bush wears in snow (frosted).
pub fn frost_sprite() -> Option<u16> {
    SECTION.with(|s| s.borrow().sprites.as_ref().and_then(|(_, _, f)| *f))
}

pub fn sprite_kind(assets: usize, layer: u16, lookup: impl Fn(&str) -> Option<u16>) -> u8 {
    SECTION.with(|s| {
        let mut s = s.borrow_mut();
        if !matches!(&s.sprites, Some((at, _, _)) if *at == assets) {
            let mut kinds = Vec::new();
            for &(name, kind) in SPRITES {
                if let Some(l) = lookup(name) {
                    let l = usize::from(l);
                    if kinds.len() <= l {
                        kinds.resize(l + 1, 0);
                    }
                    kinds[l] = kind;
                }
            }
            let fern = lookup("minecraft:block/fern");
            s.sprites = Some((assets, kinds, fern));
        }
        s.sprites.as_ref().and_then(|(_, k, _)| k.get(usize::from(layer)).copied()).unwrap_or(0)
    })
}

fn mix(v: &[Vertex; 4], s: f32, t: f32) -> Vertex {
    let w = [(1.0 - s) * (1.0 - t), s * (1.0 - t), s * t, (1.0 - s) * t];
    let f = |get: &dyn Fn(&Vertex) -> f32| w[0] * get(&v[0]) + w[1] * get(&v[1]) + w[2] * get(&v[2]) + w[3] * get(&v[3]);
    let byte = |get: &dyn Fn(&Vertex) -> u32, shift: u32| {
        (f(&|x| ((get(x) >> shift) & 0xFF) as f32).round().clamp(0.0, 255.0) as u32) << shift
    };
    let mut out = v[0];
    out.pos = std::array::from_fn(|k| f(&|x| x.pos[k]));
    out.uv = std::array::from_fn(|k| f(&|x| f32::from(x.uv[k])).round().clamp(0.0, 65535.0) as u16);
    out.color = [0, 8, 16, 24].into_iter().map(|sh| byte(&|x| x.color, sh)).fold(0, |a, b| a | b);
    out.light = ([0, 8].into_iter().map(|sh| byte(&|x| u32::from(x.light), sh)).fold(0, |a, b| a | b)) as u16;
    out
}

fn tint(v: &mut Vertex, f: [f32; 3]) {
    let c = v.color;
    let ch = |shift: u32, f: f32| (((c >> shift) & 0xFF) as f32 * f).round().clamp(0.0, 255.0) as u32;
    // TerrainVertex colour: r in the low byte, then g, then b.
    v.color = (c & 0xFF00_0000) | ch(16, f[2]) << 16 | ch(8, f[1]) << 8 | ch(0, f[0]);
}

// --- detail around the viewer ---------------------------------------------------------------------

/// Blocks around the viewer whose snow is drawn finely (bumps, ragged edges).
pub const DETAIL_RADIUS: i32 = 20;

static VIEWER: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MIN);

fn pack_xz(x: i32, z: i32) -> i64 {
    (i64::from(x) << 32) | i64::from(z as u32)
}
fn unpack_xz(v: i64) -> (i32, i32) {
    ((v >> 32) as i32, v as i32)
}
/// The viewer's block (x, z); returns the previous one.
pub fn set_viewer(x: i32, z: i32) -> Option<(i32, i32)> {
    let old = VIEWER.swap(pack_xz(x, z), std::sync::atomic::Ordering::Relaxed);
    (old != i64::MIN).then(|| unpack_xz(old))
}
fn viewer() -> Option<(i32, i32)> {
    let v = VIEWER.load(std::sync::atomic::Ordering::Relaxed);
    (v != i64::MIN).then(|| unpack_xz(v))
}
/// Whether the section column at section coordinates (sx, sz) is drawn finely for a viewer at (vx, vz).
pub fn detailed(sx: i32, sz: i32, viewer: Option<(i32, i32)>) -> bool {
    let Some((vx, vz)) = viewer else { return false };
    let dx = (sx * 16 - vx).max(vx - (sx * 16 + 15)).max(0);
    let dz = (sz * 16 - vz).max(vz - (sz * 16 + 15)).max(0);
    dx * dx + dz * dz <= DETAIL_RADIUS * DETAIL_RADIUS
}

// --- noise ----------------------------------------------------------------------------------------

fn hash(x: i32, z: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (z as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xFF_FFFF) as f32 / 16_777_216.0
}
/// Smooth value noise in 0..1.
fn noise(x: f32, z: f32, seed: u32) -> f32 {
    let (x0, z0) = (x.floor(), z.floor());
    let (fx, fz) = (x - x0, z - z0);
    let (sx, sz) = (fx * fx * (3.0 - 2.0 * fx), fz * fz * (3.0 - 2.0 * fz));
    let (ix, iz) = (x0 as i32, z0 as i32);
    let a = hash(ix, iz, seed) + (hash(ix + 1, iz, seed) - hash(ix, iz, seed)) * sx;
    let b = hash(ix, iz + 1, seed) + (hash(ix + 1, iz + 1, seed) - hash(ix, iz + 1, seed)) * sx;
    a + (b - a) * sz
}
/// Small bumps of the surface at texel corner (gx, gz), about -0.5..0.5.
fn bumps(gx: i32, gz: i32) -> f32 {
    let (x, z) = (gx as f32, gz as f32);
    noise(x / 5.0, z / 5.0, 1) * 0.65 + noise(x / 2.2, z / 2.2, 2) * 0.35 - 0.5
}
/// How thin a snow edge may get before it ends (blocks): ragged edges.
fn edge_cut(gx: i32, gz: i32) -> f32 {
    let (x, z) = (gx as f32, gz as f32);
    0.015 + 0.11 * (noise(x / 3.3 + 17.0, z / 3.3 + 5.0, 3) * 0.7 + noise(x / 1.5, z / 1.5, 4) * 0.3)
}
/// A slight change of brightness over the snow.
fn sparkle(gx: i32, gz: i32) -> f32 {
    1.0 + (noise(gx as f32 / 4.0, gz as f32 / 4.0, 5) - 0.5) * 0.07
}

// --- the surface ----------------------------------------------------------------------------------

/// Quadratic B-spline weights of the column before, this one and the one after, at u (0..1 across the
/// block): the surface is smooth from block to block (no creases on the block grid).
fn spline(u: f32) -> [f32; 3] {
    [(1.0 - u) * (1.0 - u) * 0.5, (-2.0 * u * u + 2.0 * u + 1.0) * 0.5, u * u * 0.5]
}

/// How high snow piles against a wall (its control height, blocks).
const DRIFT: f32 = 0.45;
/// How far from open ground the snow runs out (blocks).
const TAPER: f32 = 0.5;

/// A snow surface over one block: heights and print depths on a g x g grid (one point around it too).
struct Surface {
    g: i32,
    grid: Vec<(f32, f32)>,
    /// The block's texel origin (x, z) in the world.
    texel: [i32; 2],
}
impl Surface {
    fn at(&self, iu: i32, iv: i32) -> (f32, f32) {
        let w = self.g + 3;
        self.grid[((iv.clamp(-1, self.g + 1) + 1) * w + iu.clamp(-1, self.g + 1) + 1) as usize]
    }
    fn index(&self, u: f32) -> i32 {
        (u * self.g as f32).round() as i32
    }
    /// The world texel corner of grid point (iu, iv).
    fn texel(&self, iu: i32, iv: i32) -> (i32, i32) {
        (self.texel[0] + iu * 16 / self.g, self.texel[1] + iv * 16 / self.g)
    }
    fn normal(&self, iu: i32, iv: i32) -> [f32; 3] {
        let gf = self.g as f32;
        let nx = (self.at(iu - 1, iv).0 - self.at(iu + 1, iv).0) * gf * 0.5;
        let nz = (self.at(iu, iv - 1).0 - self.at(iu, iv + 1).0) * gf * 0.5;
        let l = (nx * nx + 1.0 + nz * nz).sqrt();
        [nx / l, 1.0 / l, nz / l]
    }
}

/// The distance from (u, v) to the square [x, x + 1] x [z, z + 1].
fn box_distance(u: f32, v: f32, x: i32, z: i32) -> f32 {
    let dx = (x as f32 - u).max(u - (x + 1) as f32).max(0.0);
    let dz = (z as f32 - v).max(v - (z + 1) as f32).max(0.0);
    (dx * dx + dz * dz).sqrt()
}

fn surface(classes: &[u8; SECTION_CELLS], origin: [i32; 3], b: [i32; 3], kind: u8, tune: &Tune) -> Option<Surface> {
    let cls = |dx: i32, dy: i32, dz: i32| classes[cell_index(b[0] + dx, b[1] + dy, b[2] + dz)];
    let (x, y, z) = (origin[0] + b[0], origin[1] + b[1], origin[2] + b[2]);
    let view = view();
    let viewer = viewer();
    let grid_of = |bx: i32, bz: i32| -> i32 {
        if view.near(bx, y, bz) {
            16
        } else if detailed(bx.div_euclid(16), bz.div_euclid(16), viewer) {
            8
        } else {
            1
        }
    };
    let g = grid_of(x, z);
    if kind == 9 && g < 16 {
        return None;
    }
    let lift = tune.lift + fresh();
    // The 3 x 3 columns around: their control heights, and which of them are open ground.
    let mut control = [[0.0f32; 3]; 3];
    let mut open: Vec<(i32, i32, f32)> = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let c = cls(dx, 0, dz);
            let open_above = matches!(cls(dx, 1, dz), 0 | PLANT);
            control[(dz + 1) as usize][(dx + 1) as usize] = if is_layer(c) {
                f32::from(c) / 8.0 + if open_above { lift } else { 0.0 }
            } else if c == PLANT {
                // Snow lies around grass and bushes as one layer.
                // a little mound around the stem
                1.0 / 8.0 + if open_above { lift + 1.5 / 16.0 } else { 0.0 }
            } else if c == 9 || c == WALL {
                DRIFT
            } else {
                // Open ground at the same level: near the player the surface runs on into it and ends
                // raggedly there (drawn with that cell); far off it runs out over half a block. A drop-off
                // (air below): it rounds off at the very edge, like a cap.
                if cls(dx, -1, dz) != WALL {
                    open.push((dx, dz, 0.18));
                } else if g == 1 {
                    open.push((dx, dz, TAPER));
                }
                0.0
            };
        }
    }
    let floor = if kind == 9 { 1.0 - 0.45 } else { 0.012 };
    let height = |u: f32, v: f32| -> f32 {
        if kind == 9 {
            return 1.0;
        }
        let (wu, wv) = (spline(u), spline(v));
        let mut h = 0.0;
        for j in 0..3 {
            for i in 0..3 {
                h += wu[i] * wv[j] * control[j][i];
            }
        }
        // Big soft waves where the snow is thick enough (world noise: the same from every block).
        if h > 0.0 {
            let (wx, wz) = (x as f32 + u, z as f32 + v);
            let dune = (noise(wx / 5.5, wz / 5.5, 21) - 0.5) * 3.5 + (noise(wx / 2.3, wz / 2.3, 22) - 0.5) * 1.5;
            h += dune / 16.0 * (h / 0.3).clamp(0.0, 1.5);
        }
        // Next to open ground the snow runs out to nothing (the same from either side of an edge).
        let t = open
            .iter()
            .map(|&(ox, oz, r)| box_distance(u, v, ox, oz) / r)
            .fold(1.0f32, f32::min)
            .clamp(0.0, 1.0);
        h * t * t * (3.0 - 2.0 * t)
    };
    let gf = g as f32;
    let w = g + 3;
    let texel = [x * 16, z * 16];
    let mut grid = vec![(0.0f32, 0.0f32); (w * w) as usize];
    for iv in -1..=g + 1 {
        for iu in -1..=g + 1 {
            let base = height(iu as f32 / gf, iv as f32 / gf);
            let (gx, gz) = (texel[0] + iu * 16 / g, texel[1] + iv * 16 / g);
            // Bumps fade out where the snow is thin.
            let bump = if kind == 9 { 0.0 } else { bumps(gx, gz) * (1.2 / 16.0) * (base / 0.15).clamp(0.0, 1.0) };
            let d = if g == 16 { view.at(gx, y, gz) } else { 0.0 };
            grid[((iv + 1) * w + iu + 1) as usize] = ((base + bump - d).max(floor), d);
        }
    }
    // Where a neighbour is drawn coarser, this block's edge follows its points (no cracks between them).
    for (edge, (nx, nz)) in [(0, (0, -1)), (1, (0, 1)), (2, (-1, 0)), (3, (1, 0))] {
        if !is_layer(cls(nx, 0, nz)) && cls(nx, 0, nz) != 9 {
            continue;
        }
        let gn = grid_of(x + nx, z + nz);
        if gn >= g {
            continue;
        }
        let step = g / gn;
        for k in 0..=g {
            if k % step == 0 {
                continue;
            }
            let (k0, k1) = (k - k % step, k - k % step + step);
            let t = (k % step) as f32 / step as f32;
            let pick = |kk: i32| match edge {
                0 => (kk, 0),
                1 => (kk, g),
                2 => (0, kk),
                _ => (g, kk),
            };
            let (a, bb) = (pick(k0), pick(k1));
            let ha = grid[((a.1 + 1) * w + a.0 + 1) as usize];
            let hb = grid[((bb.1 + 1) * w + bb.0 + 1) as usize];
            let p = pick(k);
            grid[((p.1 + 1) * w + p.0 + 1) as usize] = (ha.0 + (hb.0 - ha.0) * t, ha.1 + (hb.1 - ha.1) * t);
        }
    }
    Some(Surface { g, grid, texel })
}

/// A quad of a snow block (the snow sprite) as the mesher emits it, in section coordinates: the pieces
/// to emit instead (none: drop it), or None to keep it as it is.
pub fn reshape(v: &[Vertex; 4], sprite: u8) -> Option<Vec<[Vertex; 4]>> {
    let tune = tune();
    if !tune.enabled {
        return None;
    }
    SECTION.with(|s| {
        let s = s.borrow();
        let classes = s.classes.as_ref()?;
        if sprite == 2 || sprite == 3 {
            return if tune.frost { frost(classes, v) } else { None };
        }
        reshape_in(classes, s.origin, v, &tune)
    })
}

/// A plant next to snow: white with frost (its light and shade kept).
fn frost(classes: &[u8; SECTION_CELLS], v: &[Vertex; 4]) -> Option<Vec<[Vertex; 4]>> {
    let c: [f32; 3] = std::array::from_fn(|k| v.iter().map(|q| q.pos[k]).sum::<f32>() * 0.25);
    let b = [c[0].floor() as i32, c[1].floor() as i32, c[2].floor() as i32];
    if !(0..16).contains(&b[0]) || !(0..16).contains(&b[1]) || !(0..16).contains(&b[2]) {
        return None;
    }
    let snowy = (-1..=0).any(|dy| {
        (-1..=1).any(|dz| {
            (-1..=1).any(|dx| {
                let k = classes[cell_index(b[0] + dx, b[1] + dy, b[2] + dz)];
                is_layer(k) || k == 9
            })
        })
    });
    if !snowy {
        return None;
    }
    let mut out = *v;
    for q in &mut out {
        let col = q.color;
        let ch = |shift: u32| ((col >> shift) & 0xFF) as f32;
        let old = [ch(0), ch(8), ch(16)];
        let l = old[0].max(old[1]).max(old[2]) / 255.0;
        let frosted = [232.0 * l, 240.0 * l, 255.0 * l];
        let m: [u32; 3] = std::array::from_fn(|k| (old[k] * 0.25 + frosted[k] * 0.75).round().clamp(0.0, 255.0) as u32);
        q.color = (col & 0xFF00_0000) | m[2] << 16 | m[1] << 8 | m[0];
    }
    Some(vec![out])
}

/// The cells next to snow block `b` (its top quad `v`) that the snow runs into: grass and bushes stand in it,
/// open ground near the player gets the surface running on and ending raggedly. Each cell is drawn by one
/// snow block beside it in the same section (the first in the world's order).
fn fill_cells(classes: &[u8; SECTION_CELLS], origin: [i32; 3], b: [i32; 3], v: &[Vertex; 4], tune: &Tune, out: &mut Vec<[Vertex; 4]>) {
    let cls = |dx: i32, dy: i32, dz: i32| classes[cell_index(b[0] + dx, b[1] + dy, b[2] + dz)];
    let p = v.map(|x| x.pos);
    let es = [p[1][0] - p[0][0], p[1][2] - p[0][2]];
    let et = [p[3][0] - p[0][0], p[3][2] - p[0][2]];
    let to_param = |lx: f32, lz: f32| -> (f32, f32) {
        let (rx, rz) = (lx - (p[0][0] - b[0] as f32), lz - (p[0][2] - b[2] as f32));
        if es[0].abs() > 0.5 { (rx / es[0], rz / et[1]) } else { (rz / es[1], rx / et[0]) }
    };
    let depth = tune.depth.max(0.01);
    for oz in -1..=1 {
        for ox in -1..=1 {
            let class = cls(ox, 0, oz);
            if (ox, oz) == (0, 0) || !matches!(class, 0 | PLANT) || cls(ox, -1, oz) != WALL {
                continue;
            }
            let o = [b[0] + ox, b[1], b[2] + oz];
            // Who draws the cell: the first snow layer beside it (world order: z, then x) in the cell's own
            // section, or if there is none there, the first beside it anywhere. Every section that sees the
            // cell (it or its border) picks the same one.
            let section_of = |x: i32, z: i32| (x.div_euclid(16), z.div_euclid(16));
            let home = section_of(o[0], o[2]);
            let mut own = None;
            let mut any = None;
            for az in -1..=1 {
                for ax in -1..=1 {
                    let (cx, cz) = (o[0] + ax, o[2] + az);
                    if (ax, az) == (0, 0) || !is_layer(classes[cell_index(cx, o[1], cz)]) {
                        continue;
                    }
                    if any.is_none() {
                        any = Some((cx, cz));
                    }
                    if own.is_none() && section_of(cx, cz) == home {
                        own = Some((cx, cz));
                    }
                }
            }
            if own.or(any) != Some((b[0], b[2])) {
                continue;
            }
            let Some(surf) = surface(classes, origin, o, 1, tune) else { continue };
            let g = surf.g;
            // Open ground only near the player (far off the snow just runs out at its edge).
            let ragged = class == 0;
            if ragged && g == 1 {
                continue;
            }
            let gf = g as f32;
            let (sc, tc) = to_param(ox as f32 + 0.5, oz as f32 + 0.5);
            let (fs, ft) = (sc.floor(), tc.floor());
            for j in 0..g {
                for i in 0..g {
                    let (s0, s1) = (sc - 0.5 + i as f32 / gf, sc - 0.5 + (i + 1) as f32 / gf);
                    let (t0, t1) = (tc - 0.5 + j as f32 / gf, tc - 0.5 + (j + 1) as f32 / gf);
                    let mut thin = true;
                    let piece = [(s0, t0), (s1, t0), (s1, t1), (s0, t1)].map(|(s, t)| {
                        let mut q = mix(v, s - fs, t - ft);
                        let (x, z) = (p[0][0] + es[0] * s + et[0] * t, p[0][2] + es[1] * s + et[1] * t);
                        let (iu, iv) = (surf.index(x - o[0] as f32), surf.index(z - o[2] as f32));
                        let (h, d) = surf.at(iu, iv);
                        let (gx, gz) = surf.texel(iu, iv);
                        if h >= edge_cut(gx, gz) {
                            thin = false;
                        }
                        q.pos = [x, o[1] as f32 + h, z];
                        let mut f = shade(surf.normal(iu, iv), d / depth, tune);
                        let k = sparkle(gx, gz);
                        for c in &mut f {
                            *c *= k;
                        }
                        tint(&mut q, f);
                        q
                    });
                    // On open ground the snow ends where it gets too thin (a ragged edge over the grass).
                    if ragged && thin {
                        continue;
                    }
                    out.push(piece);
                }
            }
        }
    }
}

fn reshape_in(classes: &[u8; SECTION_CELLS], origin: [i32; 3], v: &[Vertex; 4], tune: &Tune) -> Option<Vec<[Vertex; 4]>> {
    let tune = *tune;
    let p = v.map(|x| x.pos);
    let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
    let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
    let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
    let c: [f32; 3] = std::array::from_fn(|k| (p[0][k] + p[1][k] + p[2][k] + p[3][k]) * 0.25);
    let a = n.map(f32::abs);
    let lo: [f32; 3] = std::array::from_fn(|k| p.iter().map(|q| q[k]).fold(f32::MAX, f32::min));
    let hi: [f32; 3] = std::array::from_fn(|k| p.iter().map(|q| q[k]).fold(f32::MIN, f32::max));
    let inside = |b: [i32; 3]| (0..16).contains(&b[0]) && (0..16).contains(&b[1]) && (0..16).contains(&b[2]);
    if a[1] > a[0] && a[1] > a[2] {
        if n[1] <= 0.0 {
            return None;
        }
        // The top: the block under the plane.
        let b = [c[0].floor() as i32, (c[1] - 0.001).floor() as i32, c[2].floor() as i32];
        if !inside(b) {
            return None;
        }
        let kind = classes[cell_index(b[0], b[1], b[2])];
        if kind == 0 || kind == WALL {
            return None;
        }
        // Only a whole top (a pack's partial faces stay as they are).
        if hi[0] - lo[0] < 0.999 || hi[2] - lo[2] < 0.999 {
            return None;
        }
        let surf = surface(classes, origin, b, kind, &tune)?;
        let g = surf.g;
        let gf = g as f32;
        let depth = tune.depth.max(0.01);
        let mut out = Vec::with_capacity((g * g) as usize);
        for j in 0..g {
            for i in 0..g {
                let (s0, s1) = (i as f32 / gf, (i + 1) as f32 / gf);
                let (t0, t1) = (j as f32 / gf, (j + 1) as f32 / gf);
                let piece = [(s0, t0), (s1, t0), (s1, t1), (s0, t1)].map(|(s, t)| {
                    let mut q = mix(v, s, t);
                    let (iu, iv) = (surf.index(q.pos[0] - b[0] as f32), surf.index(q.pos[2] - b[2] as f32));
                    let (h, d) = surf.at(iu, iv);
                    let (gx, gz) = surf.texel(iu, iv);
                    q.pos[1] = b[1] as f32 + h;
                    let mut f = shade(surf.normal(iu, iv), d / depth, &tune);
                    let s = sparkle(gx, gz);
                    for c in &mut f {
                        *c *= s;
                    }
                    tint(&mut q, f);
                    q
                });
                out.push(piece);
            }
        }
        if kind != 9 {
            fill_cells(classes, origin, b, v, &tune, &mut out);
        }
        return Some(out);
    }
    if a[1] >= a[0].max(a[2]) {
        return None;
    }
    // A side: the block behind the plane.
    let (dx, dz) = if a[0] > a[2] { (n[0].signum() as i32, 0) } else { (0, n[2].signum() as i32) };
    let b = [
        (c[0] - dx as f32 * 0.001).floor() as i32,
        c[1].floor() as i32,
        (c[2] - dz as f32 * 0.001).floor() as i32,
    ];
    if !inside(b) {
        return None;
    }
    let kind = classes[cell_index(b[0], b[1], b[2])];
    if !is_layer(kind) {
        return None;
    }
    // Between two snow columns (or snow and a plant standing in it) the surface is one: no wall there.
    let next = classes[cell_index(b[0] + dx, b[1], b[2] + dz)];
    if is_layer(next) {
        return Some(Vec::new());
    }
    if lo[1] - b[1] as f32 > 0.01 {
        return None;
    }
    let surf = surface(classes, origin, b, kind, &tune)?;
    let g = surf.g;
    let gf = g as f32;
    // Toward open ground the surface runs out to nothing; what is left of a side follows its edge.
    let horizontal_s = (p[0][1] - p[1][1]).abs() < 1e-4;
    let mut out = Vec::with_capacity(g as usize);
    for i in 0..g {
        let (a0, a1) = (i as f32 / gf, (i + 1) as f32 / gf);
        let params = if horizontal_s {
            [(a0, 0.0), (a1, 0.0), (a1, 1.0), (a0, 1.0)]
        } else {
            [(0.0, a0), (1.0, a0), (1.0, a1), (0.0, a1)]
        };
        let mut top = 0.0f32;
        let piece = params.map(|(s, t)| {
            let mut q = mix(v, s, t);
            if q.pos[1] - b[1] as f32 > 0.01 {
                let (iu, iv) = (surf.index(q.pos[0] - b[0] as f32), surf.index(q.pos[2] - b[2] as f32));
                let h = surf.at(iu, iv).0;
                q.pos[1] = b[1] as f32 + h;
                top = top.max(h);
            }
            q
        });
        if top > 0.02 {
            out.push(piece);
        }
    }
    Some(out)
}

// --- snowfall -------------------------------------------------------------------------------------

/// A flake to spawn around the camera: where, its velocity, fine powder (white ash) or a flake.
pub struct Flake {
    pub at: [f64; 3],
    pub velocity: [f64; 3],
    pub powder: bool,
}

/// The kinds of snowfall; one is picked each time snow starts to fall. Thunder makes any of them a blizzard.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Style {
    /// Big slow flakes, few, no wind.
    Flurries,
    /// Steady snowfall with a breeze.
    Steady,
    /// Thick snow driven by gusts.
    Windy,
}

struct Fall {
    style: Style,
    strength: f32,
    haze: f32,
    rng: u64,
    ticks: u64,
    falling: bool,
}

static FALL: Mutex<Option<Fall>> = Mutex::new(None);
static HAZE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// Fresh powder on top of the snow (blocks), grown by snowfall and settling after it.
static FRESH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The fresh powder on the snow now (blocks).
pub fn fresh() -> f32 {
    f32::from_bits(FRESH.load(std::sync::atomic::Ordering::Relaxed))
}

fn rand(state: &mut u64) -> f32 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    ((state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32) / 16_777_216.0
}

/// The white haze of a blizzard (and of windy snow), 0..1: the fog hook reads it.
pub fn haze() -> f32 {
    f32::from_bits(HAZE.load(std::sync::atomic::Ordering::Relaxed))
}

/// One client tick of snowfall around the camera. `snowing`: snow (not rain) falls at the camera;
/// `rain`, `thunder`: the weather's levels (0..1); `open(x, y, z)`: nothing above that spot.
pub fn snowfall(
    camera: [f64; 3],
    snowing: bool,
    rain: f32,
    thunder: f32,
    open: impl Fn(i32, i32, i32) -> bool,
) -> (Vec<Flake>, bool) {
    use std::f32::consts::TAU;
    let tune = tune();
    let mut guard = FALL.lock().unwrap_or_else(|e| e.into_inner());
    let f = guard.get_or_insert_with(|| Fall {
        style: Style::Steady,
        strength: 0.0,
        haze: 0.0,
        rng: 0x9E37_79B9_7F4A_7C15 ^ (now().to_bits() as u64) << 17,
        ticks: 0,
        falling: false,
    });
    f.ticks += 1;
    let on = tune.weather && snowing;
    let target = if on { rain.clamp(0.0, 1.0) } else { 0.0 };
    if target > 0.05 && !f.falling {
        f.falling = true;
        f.style = match (rand(&mut f.rng) * 3.0) as u32 {
            0 => Style::Flurries,
            1 => Style::Steady,
            _ => Style::Windy,
        };
        log(&format!("snow starts: {:?}", f.style));
    }
    if target <= 0.0 && f.strength < 0.01 {
        f.falling = false;
    }
    f.strength += (target - f.strength) * 0.02;
    let blizzard = on && thunder > 0.3;
    let haze_target = if !on {
        0.0
    } else if blizzard {
        f.strength
    } else if f.style == Style::Windy {
        0.3 * f.strength
    } else {
        0.0
    };
    f.haze += (haze_target - f.haze) * 0.02;
    HAZE.store(f.haze.to_bits(), std::sync::atomic::Ordering::Relaxed);
    // Fresh powder: up to 3 px over 4 minutes of heavy snow, settling over 10 minutes after it.
    let max_fresh = 3.0 / 16.0;
    let old = fresh();
    let grown = if on && f.strength > 0.05 {
        old + max_fresh / 4800.0 * f.strength * if blizzard { 1.6 } else { 1.0 }
    } else {
        old - max_fresh / 12000.0
    }
    .clamp(0.0, max_fresh);
    FRESH.store(grown.to_bits(), std::sync::atomic::Ordering::Relaxed);
    // The snow is rebuilt each half texel the powder grows or settles.
    let rebuild = (grown * 32.0).floor() != (old * 32.0).floor();
    // Falling snow covers the prints: they age faster (4x in heavy snow).
    if on && f.strength > 0.05 {
        if let Ok(mut store) = store().write() {
            let shift = 0.05 * 3.0 * f.strength;
            for imp in store.map.values_mut() {
                imp.t -= shift;
            }
        }
    }
    if f.strength < 0.02 || !tune.weather {
        return (Vec::new(), rebuild);
    }
    // density, wind (blocks a tick), falling speed range
    let (density, wind, fall_lo, fall_hi) = if blizzard {
        (2.4, 0.42, 0.10, 0.22)
    } else {
        match f.style {
            Style::Flurries => (0.35, 0.025, 0.025, 0.05),
            Style::Steady => (0.7, 0.07, 0.05, 0.10),
            Style::Windy => (1.0, 0.18, 0.07, 0.14),
        }
    };
    let t = f.ticks as f32;
    let angle = noise(t / 1800.0, 0.5, 11) * TAU * 2.0;
    let gust = if blizzard || f.style == Style::Windy { 0.55 + 0.9 * noise(t / 45.0, 3.5, 12) } else { 1.0 };
    let w = [angle.cos() * wind * gust, angle.sin() * wind * gust];
    let count = (density * f.strength * 55.0 * tune.flakes) as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let r = if blizzard { 9.0 } else { 13.0 } * rand(&mut f.rng).sqrt();
        let a = rand(&mut f.rng) * TAU;
        // Upwind, so the flakes cross the view.
        let at = [
            camera[0] + f64::from(r * a.cos() - w[0] * 12.0),
            camera[1] - 3.0 + f64::from(rand(&mut f.rng) * 12.0),
            camera[2] + f64::from(r * a.sin() - w[1] * 12.0),
        ];
        if !open(at[0].floor() as i32, at[1].floor() as i32, at[2].floor() as i32) {
            continue;
        }
        let down = fall_lo + (fall_hi - fall_lo) * rand(&mut f.rng);
        let sway = (rand(&mut f.rng) - 0.5) * if f.style == Style::Flurries { 0.05 } else { 0.03 };
        out.push(Flake {
            at,
            velocity: [f64::from(w[0] + sway), -f64::from(down), f64::from(w[1] - sway)],
            powder: blizzard && i % 2 == 0,
        });
    }
    (out, rebuild)
}
