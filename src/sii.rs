//! Enough of the SCS `.sii` definition format to work out how a vehicle is
//! assembled.
//!
//! An ETS2 vehicle is not one model. `data.sii` lists `require[]` accessory
//! slots; the chosen cabin and chassis then name a specific shape per slot via
//! `defaults[]`, and each shape .sii points at the actual model through
//! `exterior_model` / `interior_model`. Resolving that graph is the difference
//! between a bare body shell and a complete bus.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SiiUnit {
    pub path: PathBuf,
    pub keys: HashMap<String, String>,
    pub lists: HashMap<String, Vec<String>>,
}

impl SiiUnit {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.keys.get(k).map(|s| s.as_str())
    }
    pub fn list(&self, k: &str) -> &[String] {
        self.lists.get(k).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

pub fn parse(path: &Path) -> Option<SiiUnit> {
    let mut keys = HashMap::new();
    let mut lists: HashMap<String, Vec<String>> = HashMap::new();
    read_into(path, &mut keys, &mut lists, 0)?;
    Some(SiiUnit { path: path.to_path_buf(), keys, lists })
}

/// `.sii` files pull in `.sui` fragments with `@include`, relative to the
/// including file's own directory. The interior's whole animation list lives in
/// one of those, so not following includes loses it entirely.
fn read_into(
    path: &Path,
    keys: &mut HashMap<String, String>,
    lists: &mut HashMap<String, Vec<String>>,
    depth: u32,
) -> Option<()> {
    if depth > 8 {
        return Some(());
    }
    let raw = std::fs::read(path).ok()?;
    // Some mods ship their definitions obfuscated; undo that before reading.
    let raw = crate::sii3nk::decode(&raw).unwrap_or(raw);
    let text = String::from_utf8_lossy(&raw);
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@include") {
            let inc = rest.trim().trim_matches('"');
            if !inc.is_empty() {
                let mut p = dir.clone();
                for part in inc.trim_start_matches('/').split('/') {
                    p.push(part);
                }
                read_into(&p, keys, lists, depth + 1);
            }
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let key = k.trim();
        let val = v.trim().trim_end_matches(',').trim().trim_matches('"').to_string();
        if let Some(base) = key.strip_suffix("[]") {
            lists.entry(base.trim().to_string()).or_default().push(val);
        } else if !key.contains(' ') {
            keys.entry(key.to_string()).or_insert(val);
        }
    }
    Some(())
}

#[derive(Debug, Clone)]
pub struct Accessory {
    pub slot: String,
    pub kind: &'static str, // "exterior" | "interior"
    pub model: String,      // archive path, no extension
    #[allow(dead_code)] // which .sii chose this shape, for tracing a bad mount
    pub sii: String,
    /// Accessory models carry variants of their own (one .pmg holding the 32,
    /// 34 and 36 bunk layouts, for instance). Ignoring this leaves every layout
    /// stacked on top of the others.
    pub variant: String,
}

#[derive(Debug, Clone)]
pub struct VehicleDef {
    pub def_dir: PathBuf,
    pub name: String,
    pub cabins: Vec<String>,
    pub chassis: Vec<String>,
    pub required: Vec<String>,
}

fn stem_list(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("sii") {
                p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    v.sort();
    v
}

/// Find every truck definition in an extracted archive.
pub fn find_vehicles(root: &Path) -> Vec<VehicleDef> {
    let mut out = Vec::new();
    let trucks = root.join("def").join("vehicle").join("truck");
    let Ok(rd) = std::fs::read_dir(&trucks) else { return out };
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let data = dir.join("data.sii");
        if !data.is_file() {
            continue;
        }
        let unit = parse(&data);
        let required = unit.map(|u| u.list("require").to_vec()).unwrap_or_default();
        out.push(VehicleDef {
            name: dir.file_name().unwrap().to_string_lossy().to_string(),
            cabins: stem_list(&dir.join("cabin")),
            chassis: stem_list(&dir.join("chassis")),
            required,
            def_dir: dir,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn rel_to_def(def_dir: &Path, reference: &str) -> PathBuf {
    // defaults[] entries are absolute in-archive paths like
    // "/def/vehicle/truck/<name>/accessory/roof/shape2.sii"
    let cleaned = reference.trim_start_matches('/');
    let marker = def_dir
        .file_name()
        .map(|s| format!("{}/", s.to_string_lossy()))
        .unwrap_or_default();
    let tail = cleaned
        .split_once(&marker)
        .map(|(_, t)| t.to_string())
        .unwrap_or_else(|| cleaned.to_string());
    let mut p = def_dir.to_path_buf();
    for part in tail.split('/') {
        p.push(part);
    }
    p
}

/// ConverterPIX wants archive paths without an extension. Passing an animation
/// as `.../win_open_l.pma` still yields a skeleton but silently produces no
/// .pia at all, so stripping this is not cosmetic.
fn strip_ext(model: &str) -> String {
    let m = model.trim();
    for e in [".pmd", ".pmg", ".pmc", ".pma", ".pms"] {
        if let Some(s) = m.strip_suffix(e) {
            return s.to_string();
        }
    }
    m.to_string()
}

/// Resolve the accessory set the game would fit by default for one cabin +
/// chassis combination.
pub fn resolve_accessories(
    v: &VehicleDef,
    cabin: &str,
    chassis: &str,
) -> (Vec<Accessory>, Vec<String>) {
    let mut chosen: HashMap<String, SiiUnit> = HashMap::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut queue: Vec<PathBuf> = vec![
        v.def_dir.join("cabin").join(format!("{cabin}.sii")),
        v.def_dir.join("chassis").join(format!("{chassis}.sii")),
    ];

    while let Some(p) = queue.pop() {
        if !seen.insert(p.clone()) || !p.is_file() {
            continue;
        }
        let Some(unit) = parse(&p) else { continue };
        let slot = p
            .parent()
            .and_then(|d| d.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let has_model = unit.get("exterior_model").is_some() || unit.get("interior_model").is_some();
        if has_model && slot != v.name && slot != "cabin" && slot != "chassis" {
            chosen.entry(slot).or_insert_with(|| unit.clone());
        }
        for d in unit.list("defaults") {
            queue.push(rel_to_def(&v.def_dir, d));
        }
    }

    // Every accessory slot the mod ships, not just the ones data.sii marks
    // required.
    //
    // `require[]` on a real mod lists only a handful - the HOWO TS7 names three -
    // while the mod carries some sixty slots that the player buys in the shop:
    // the front mask, the bumper, mudflaps, lights. Fitting one shape from each
    // is what makes the vehicle look like the vehicle instead of a bare cab.
    let mut all_slots: Vec<String> = v.required.clone();
    if let Ok(rd) = std::fs::read_dir(v.def_dir.join("accessory")) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                let n = e.file_name().to_string_lossy().to_string();
                if !all_slots.contains(&n) {
                    all_slots.push(n);
                }
            }
        }
    }
    all_slots.sort();

    let mut warnings = Vec::new();
    for slot in &all_slots {
        if chosen.contains_key(slot) {
            continue;
        }
        let dir = v.def_dir.join("accessory").join(slot);
        let mut picked = None;
        let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("sii"))
            .collect();
        names.sort();
        for p in names {
            if let Some(u) = parse(&p) {
                if u.get("exterior_model").is_some() || u.get("interior_model").is_some() {
                    picked = Some(u);
                    break;
                }
            }
        }
        match picked {
            Some(u) => {
                chosen.insert(slot.clone(), u);
            }
            None => {
                if v.required.contains(slot) {
                    warnings.push(format!("required slot {slot:?} has no model"));
                }
            }
        }
    }

    let mut out = Vec::new();
    let mut slots: Vec<&String> = chosen.keys().collect();
    slots.sort();
    for slot in slots {
        let u = &chosen[slot];
        let ext = u.get("exterior_model").map(strip_ext);
        let int = u.get("interior_model").map(strip_ext);
        let sii = u.path.file_name().unwrap().to_string_lossy().to_string();
        let variant = u.get("variant").unwrap_or_default().to_string();
        if let Some(m) = ext.clone() {
            out.push(Accessory {
                slot: slot.clone(),
                kind: "exterior",
                model: m,
                sii: sii.clone(),
                variant: variant.clone(),
            });
        }
        if let Some(m) = int {
            // Several .sii files name the SAME file for both, and the matching
            // interior locator sits at the same world position - importing both
            // drops two identical copies on top of each other.
            if Some(&m) != ext.as_ref() {
                out.push(Accessory { slot: slot.clone(), kind: "interior", model: m, sii, variant });
            } else {
                warnings.push(format!(
                    "slot {slot:?}: interior_model is the same file as exterior_model, imported once"
                ));
            }
        }
    }
    (out, warnings)
}

/// A model that is neither the body nor an accessory: the openable windows, the
/// wipers, the animated dashboard. These are declared under their own key names
/// (`windows_model`, `wiper_model`, `animated_model`) and are invisible to any
/// code that only looks for `exterior_model` / `interior_model` - which is why
/// they used to be missing from the .blend entirely.
#[derive(Debug, Clone)]
pub struct ExtraModel {
    pub name: String,
    pub kind: &'static str, // which root it belongs to
    pub model: String,
    /// `.pma` animation files. ConverterPIX derives the skeleton from these, so
    /// without them the model converts with no bones at all
    /// (`--find-model-animations` cannot help: it looks for a .pis that a
    /// .pmg-only mod does not ship).
    pub anims: Vec<String>,
    /// These carry variants of their own just like accessories do.
    pub variants: Vec<String>,
}

fn anim_values(u: &SiiUnit, filter: impl Fn(&str) -> bool) -> Vec<String> {
    let mut v: Vec<String> = u
        .keys
        .iter()
        // match on the value's extension, not the key: animations also hide
        // behind names like `button_left_blinker`
        .filter(|(k, val)| val.ends_with(".pma") && filter(k))
        .map(|(_, val)| strip_ext(val))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Collect the animated models declared by the chosen cabin, chassis and interior.
pub fn resolve_extras(
    v: &VehicleDef,
    cabin: &str,
    chassis: &str,
    interior_sii: Option<&Path>,
) -> Vec<ExtraModel> {
    let mut out = Vec::new();

    // wipers and openable windows belong to the cab, so they mount on the cabin
    // model when there is a separate one
    for (kind, path) in [
        ("cabin", v.def_dir.join("cabin").join(format!("{cabin}.sii"))),
        ("exterior", v.def_dir.join("chassis").join(format!("{chassis}.sii"))),
    ] {
        let Some(u) = parse(&path) else { continue };
        let variant = u.get("variant").unwrap_or_default().to_string();
        for (key, name, want) in [
            ("windows_model", "windows", "window"),
            ("wiper_model", "wipers", "wiper"),
        ] {
            if let Some(m) = u.get(key) {
                out.push(ExtraModel {
                    name: name.to_string(),
                    kind,
                    model: strip_ext(m),
                    anims: anim_values(&u, |k| k.contains(want)),
                    variants: [variant.clone()].into_iter().filter(|s| !s.is_empty()).collect(),
                });
            }
        }
    }

    if let Some(p) = interior_sii {
        if let Some(u) = parse(p) {
            if let Some(m) = u.get("animated_model") {
                out.push(ExtraModel {
                    name: "dashboard".to_string(),
                    kind: "interior",
                    model: strip_ext(m),
                    anims: anim_values(&u, |_| true),
                    variants: u
                        .get("variant")
                        .filter(|s| !s.is_empty())
                        .map(|s| vec![s.to_string()])
                        .unwrap_or_default(),
                });
            }
            // What the cabin looks like from outside - seats and curtains seen
            // through the glass. 37 of the 39 interiors surveyed declare one.
            if let Some(m) = u.get("ext_model") {
                out.push(ExtraModel {
                    name: "ext_interior".to_string(),
                    kind: "exterior",
                    model: strip_ext(m),
                    anims: Vec::new(),
                    variants: u
                        .get("ext_variant")
                        .filter(|s| !s.is_empty())
                        .map(|s| vec![s.to_string()])
                        .unwrap_or_default(),
                });
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct InteriorDef {
    pub stem: String,
    pub variant: String,
    #[allow(dead_code)] // the interior model named by the .sii; main_models resolves it
    pub model: String,
    pub sii: PathBuf,
}

/// Interior definitions, which name both the model and the variant. Reading them
/// means the interior variant list is known before the first run instead of
/// after it.
pub fn interiors(v: &VehicleDef) -> Vec<InteriorDef> {
    let dir = v.def_dir.join("interior");
    let mut out: Vec<InteriorDef> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("sii"))
        .filter_map(|p| {
            let u = parse(&p)?;
            let model = model_of(&u)?;
            Some(InteriorDef {
                stem: p.file_stem()?.to_string_lossy().to_string(),
                variant: u.get("variant").unwrap_or_default().to_string(),
                model,
                sii: p,
            })
        })
        .collect();
    out.sort_by(|a, b| a.stem.cmp(&b.stem));
    out
}

/// One of the vehicle's own models, as opposed to a bolt-on accessory.
#[derive(Debug, Clone)]
pub struct MainModel {
    /// "exterior" (the chassis), "cabin" or "interior"
    pub role: &'static str,
    pub model: String,
    /// Variants declared for this specific model by its own .sii.
    pub variants: Vec<String>,
}

/// A .sii may name its mesh under either key. `model` is the normal one, but
/// plenty of mods only fill in `detail_model`, and reading just `model` makes
/// them look like they have no geometry at all.
fn model_of(u: &SiiUnit) -> Option<String> {
    u.get("model")
        .or_else(|| u.get("detail_model"))
        .map(strip_ext)
        .map(|m| m.trim_start_matches('/').to_string())
}

/// The vehicle's own models.
///
/// A conventional truck keeps the chassis frame and the cab in two separate
/// .pmg files; a bus usually ships one model that is both, referenced by the
/// chassis and the cabin alike. Handle both: import each distinct file once, and
/// when the two roles share a file, give that single model both their variants.
pub fn main_models(
    v: &VehicleDef,
    cabin: &str,
    chassis: &str,
    interior_sii: Option<&Path>,
) -> Vec<MainModel> {
    let chassis_u = parse(&v.def_dir.join("chassis").join(format!("{chassis}.sii")));
    let cabin_u = parse(&v.def_dir.join("cabin").join(format!("{cabin}.sii")));

    let chassis_m = chassis_u.as_ref().and_then(model_of);
    let cabin_m = cabin_u.as_ref().and_then(model_of);
    let chassis_var = chassis_u.as_ref().and_then(|u| u.get("variant")).unwrap_or_default().to_string();
    let cabin_var = cabin_u.as_ref().and_then(|u| u.get("variant")).unwrap_or_default().to_string();

    // The interior also dictates a variant of the *exterior*: the body's window
    // and door layout has to match the seating plan inside. Every mod looked at
    // uses this. Miss it and most of the body stays hidden - one coach came out
    // with 23 meshes instead of a few hundred.
    let interior_u = interior_sii.and_then(parse);
    let ext_var = interior_u
        .as_ref()
        .and_then(|u| u.get("ext_variant"))
        .unwrap_or_default()
        .to_string();

    let mut out = Vec::new();
    match (&chassis_m, &cabin_m) {
        (Some(c), Some(k)) if c == k => {
            // one model plays both roles - it needs the union of every variant
            out.push(MainModel {
                role: "exterior",
                model: c.clone(),
                variants: [chassis_var, cabin_var, ext_var.clone()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect(),
            });
        }
        _ => {
            if let Some(c) = chassis_m {
                out.push(MainModel {
                    role: "exterior",
                    model: c,
                    variants: [chassis_var, ext_var.clone()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect(),
                });
            }
            if let Some(k) = cabin_m {
                out.push(MainModel {
                    role: "cabin",
                    model: k,
                    variants: [cabin_var, ext_var.clone()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect(),
                });
            }
        }
    }

    if let Some(u) = interior_u {
        if let Some(m) = model_of(&u) {
            out.push(MainModel {
                role: "interior",
                model: m,
                variants: u
                    .get("variant")
                    .filter(|s| !s.is_empty())
                    .map(|s| vec![s.to_string()])
                    .unwrap_or_default(),
            });
        }
    }
    out
}

/// The wheel models a mod ships, if any.
///
/// Wheels are not accessories: the chassis carries `wheel_f_N_0` / `wheel_r_N_0`
/// locators and the game fits a rim and a tyre at each. Most mods define the
/// wheel units in the base game's `/def/vehicle/truck_wheel`, which is not in the
/// archive - but the models themselves usually are, so they can be mounted
/// directly off the locators.
#[derive(Debug, Clone, Default)]
pub struct Wheels {
    pub rim_front: Option<String>,
    pub rim_rear: Option<String>,
    pub tyre: Option<String>,
}

/// The livery a truck wears.
///
/// `eut2.truckpaint` renders the body as a flat `base_color` until a paint job
/// supplies its mask texture, which is why a mod that ships six liveries still
/// converted to a plain white cab. The mask is the artwork; for `airbrush`
/// paint jobs it covers the paintable panels outright.
#[derive(Debug, Clone, Default)]
pub struct PaintJob {
    pub name: String,
    /// texture path inside the archive, without extension
    pub mask: String,
    pub base_color: (f32, f32, f32),
    pub airbrush: bool,
}

fn parse_color(s: &str) -> Option<(f32, f32, f32)> {
    let inner = s.trim().trim_start_matches('(').trim_end_matches(')');
    let mut it = inner.split(',').map(|p| p.trim().parse::<f32>());
    match (it.next(), it.next(), it.next()) {
        (Some(Ok(r)), Some(Ok(g)), Some(Ok(b))) => Some((r, g, b)),
        _ => None,
    }
}

/// Pick the livery to apply. Prefer an airbrush paint job, since its mask is a
/// finished design rather than a channel mask that needs colours choosing for
/// it; ties break on the file name so the same archive always converts the same
/// way.
pub fn find_paint_job(def_dir: &Path) -> Option<PaintJob> {
    let dir = def_dir.join("paint_job");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("sii"))
        .collect();
    names.sort();

    let mut best: Option<PaintJob> = None;
    for p in names {
        let Some(u) = parse(&p) else { continue };
        let Some(mask) = u.get("paint_job_mask") else { continue };
        let mask = mask.trim().trim_matches('"').trim_start_matches('/');
        if mask.is_empty() {
            continue;
        }
        let mask = mask
            .trim_end_matches(".tobj")
            .trim_end_matches(".dds")
            .to_string();
        let job = PaintJob {
            name: p.file_stem().unwrap_or_default().to_string_lossy().to_string(),
            mask,
            base_color: u
                .get("base_color")
                .and_then(parse_color)
                .unwrap_or((1.0, 1.0, 1.0)),
            airbrush: u.get("airbrush").map(|v| v.trim() == "true").unwrap_or(false),
        };
        let better = match &best {
            None => true,
            Some(b) => job.airbrush && !b.airbrush,
        };
        if better {
            best = Some(job);
        }
    }
    best
}

/// Path fragments that mark a model as part of a wheel. Mod authors are not
/// consistent about naming these in English.
const WHEEL_WORDS: [&str; 6] = ["wheel", "roda", "disc", "/rim", "tire", "tyre"];

pub fn find_wheels(extracted: &Path) -> Wheels {
    let mut candidates: Vec<String> = Vec::new();
    let mut stack = vec![extracted.join("vehicle")];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("pmg") {
                if let Ok(rel) = p.strip_prefix(extracted) {
                    let s = rel.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
                    // "wheel" alone is not enough: the Hyundai Aero Space keeps
                    // its rims under upgrade/rodas/ (Portuguese for wheels), so
                    // match the part names too.
                    if WHEEL_WORDS.iter().any(|k| s.contains(k)) {
                        candidates.push(s.trim_end_matches(".pmg").to_string());
                    }
                }
            }
        }
    }
    candidates.sort();

    let is_rim = |s: &str| s.contains("disc") || s.contains("rim") || s.contains("roda");
    let mut w = Wheels::default();
    for c in &candidates {
        if is_rim(c) && c.contains("front") && w.rim_front.is_none() {
            w.rim_front = Some(c.clone());
        } else if is_rim(c) && c.contains("rear") && w.rim_rear.is_none() {
            w.rim_rear = Some(c.clone());
        }
    }
    // the tyre is whatever wheel model is not a rim - commonly /vehicle/wheel/<x>
    for c in &candidates {
        if !is_rim(c) && !c.contains("null") && w.tyre.is_none() {
            w.tyre = Some(c.clone());
        }
    }
    // a mod with only one rim model uses it front and rear
    if w.rim_front.is_none() {
        w.rim_front = w.rim_rear.clone();
    }
    if w.rim_rear.is_none() {
        w.rim_rear = w.rim_front.clone();
    }
    w
}




