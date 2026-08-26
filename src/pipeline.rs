//! Orchestration: archive -> mid-format -> FBX, all in this process.
//!
//! Steps 3 and 4 exist because the mod references base-game files it does not
//! ship, and because ConverterPIX does not mark normal maps.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

use crate::sii;

#[derive(Debug, Clone)]
pub enum Msg {
    Step(String),
    Info(String),
    Warn(String),
    Error(String),
    Progress(f32),
    Done(Vec<PathBuf>),
}

#[derive(Debug, Clone)]
pub struct Options {
    pub archive: PathBuf,
    pub outdir: PathBuf,
    pub converter_pix: PathBuf,
    pub vehicle: String,
    pub cabin: String,
    pub chassis: String,
    pub interior_variant: String,
    /// Park the variants the vehicle is not wearing in a row beside it. They are
    /// half of what a mod ships, but they multiply the file size several times
    /// over, so this is a choice rather than a default.
    pub variants_row: bool,
    /// Pull the mod's audio out next to the model, as MP3 when ffmpeg is around.
    pub sounds: bool,
    /// Leave an archive alone when its .fbx is already there.
    pub skip_existing: bool,
    /// The SCS-native .blend is always written (Blender needs a save point), but
    /// most people only want the clean one, so it is removed unless asked for.
    /// Delete the working folder afterwards. It holds the ~5000 extracted archive
    /// files and the mid-format conversion - useful for debugging, noise for
    /// everyone else.
    pub cleanup: bool,
}

/// A filesystem- and human-friendly name for the outputs, taken from the archive.
/// Mod filenames carry version brackets that make for miserable file names.
fn output_stem(archive: &Path) -> String {
    let raw = archive
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "vehicle".into());
    let mut s = String::new();
    let mut depth = 0i32;
    for c in raw.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = (depth - 1).max(0),
            _ if depth == 0 => s.push(c),
            _ => {}
        }
    }
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' || c == ' ' { c } else { ' ' })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim().trim_end_matches(['-', '_']).trim();
    if cleaned.is_empty() {
        "vehicle".to_string()
    } else {
        cleaned.chars().take(60).collect()
    }
}

/// Textures every ETS2 mod references but none of them ship: they live in the
/// base game. An empty ShaderNodeTexEnvironment renders as pure magenta and the
/// AddEnv group adds that on top of every shaded pixel, so a neutral stand-in is
/// far better than nothing.
const PLACEHOLDERS: &[(&str, f32)] = &[
    ("material/environment/vehicle_reflection", 0.25),
    ("material/environment/soft_reflection/soft_reflection", 0.25),
    ("material/environment/fuzzy_reflection/fuzzy_reflection", 0.20),
    ("material/environment/interior_reflection", 0.18),
    ("material/environment/brushed_reflection/brushed_reflection", 0.22),
    ("material/environment/generic_reflection", 0.25),
    ("material/environment/close_mirror_reflection", 0.45),
    ("material/environment/hood_mirror_reflection", 0.45),
    ("material/environment/far_mirror_reflection", 0.45),
    ("material/environment/far_s_mirror_reflection", 0.45),
    ("vehicle/truck/share/glass_ex", 0.72),
    ("vehicle/truck/share/glass_int", 0.72),
    ("vehicle/truck/share/dashboard", 0.35),
    ("vehicle/truck/share/gps", 0.35),
];

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        table[i as usize] = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// Minimal flat-colour PNG. The extension matters: SCS Blender Tools only ever
/// probes for .tobj/.tga/.png, so an extension-less file is never found - and it
/// must NOT be .tobj, which would route reflections into the cubemap loader.
fn flat_png(level: f32, size: u32) -> Vec<u8> {
    let v = (level.powf(1.0 / 2.2) * 255.0).round().clamp(0.0, 255.0) as u8;
    let mut raw = Vec::with_capacity((size * (size * 4 + 1)) as usize);
    for _ in 0..size {
        raw.push(0u8);
        for _ in 0..size {
            raw.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let mut idat = Vec::new();
    {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut enc = ZlibEncoder::new(&mut idat, Compression::best());
        enc.write_all(&raw).ok();
        enc.finish().ok();
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    for (tag, body) in [(&b"IHDR"[..], ihdr), (&b"IDAT"[..], idat), (&b"IEND"[..], Vec::new())] {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut chunk = tag.to_vec();
        chunk.extend_from_slice(&body);
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc32(&chunk).to_be_bytes());
    }
    out
}

/// Neutral grey for a texture we have to invent, guessed from its name.
fn placeholder_level(path: &str) -> f32 {
    let p = path.to_ascii_lowercase();
    if p.contains("mirror") {
        0.45
    } else if p.contains("glass") || p.contains("window") {
        0.72
    } else if p.contains("reflection") || p.contains("environment") {
        0.25
    } else if p.contains("light") || p.contains("lamp") || p.contains("lum") {
        0.60
    } else {
        0.50
    }
}

fn write_one_placeholder(project: &Path, rel: &str, level: f32) -> std::io::Result<()> {
    let mut p = project.to_path_buf();
    for part in rel.trim_start_matches('/').split('/') {
        p.push(part);
    }
    p.set_extension("png");
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, flat_png(level, 64))
}

/// Every texture the mod's materials reference but does not ship.
///
/// Hardcoding the names does not scale: each mod leans on a different set of
/// base-game files, and a missing one is not a cosmetic problem - SCS Tools
/// returns None, and an empty image node evaluates to pure black for a BaseTex
/// or pure magenta for a reflection, so whole panels come out wrong.
fn missing_textures(extracted: &Path, project: &Path) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    let mut stack = vec![extracted.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("mat") {
                let Ok(txt) = std::fs::read_to_string(&p) else { continue };
                for line in txt.lines() {
                    let l = line.trim();
                    if !l.starts_with("texture[") {
                        continue;
                    }
                    if let Some(v) = l.split('"').nth(1) {
                        let v = v.trim();
                        // guard against blank or directory-only entries, which
                        // otherwise produce hundreds of bogus placeholder files
                        if v.len() > 3 && v.contains('/') && !v.ends_with('/') {
                            refs.push(v.to_string());
                        }
                    }
                }
            }
        }
    }
    refs.sort();
    refs.dedup();

    refs.into_iter()
        .filter_map(|r| {
            let stem = r
                .trim_start_matches('/')
                .trim_end_matches(".tobj")
                .to_string();
            let base = {
                let mut p = project.to_path_buf();
                for part in stem.split('/') {
                    p.push(part);
                }
                p
            };
            // any of the forms SCS Tools probes for
            for ext in ["tobj", "tga", "png", "dds"] {
                if base.with_extension(ext).is_file() {
                    return None;
                }
            }
            Some(stem)
        })
        .collect()
}

fn write_placeholders(project: &Path) -> std::io::Result<usize> {
    let mut n = 0;
    for (rel, level) in PLACEHOLDERS {
        write_one_placeholder(project, rel, *level)?;
        n += 1;
    }
    std::fs::write(
        project.join("material").join("PLACEHOLDERS.txt"),
        b"Neutral stand-ins for base-game textures this mod references but does not ship.\n\
          Replace with the real files from base.scs if you have ETS2 installed.\n" as &[u8],
    )
    .ok();
    Ok(n)
}

/// ConverterPIX writes no `usage tsnormal` line, so SCS Blender Tools stamps
/// every normal map as sRGB.
fn mark_normal_maps(extracted: &Path, project: &Path) -> usize {
    let mut refs: Vec<String> = Vec::new();
    let mut stack = vec![extracted.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("mat") {
                let Ok(txt) = std::fs::read_to_string(&p) else { continue };
                let mut tex: Vec<(usize, String)> = Vec::new();
                let mut names: Vec<(usize, String)> = Vec::new();
                for line in txt.lines() {
                    let l = line.trim();
                    if let Some(rest) = l.strip_prefix("texture[") {
                        if let Some((idx, val)) = rest.split_once(']') {
                            if let (Ok(i), Some(v)) = (idx.parse::<usize>(), val.split('"').nth(1)) {
                                tex.push((i, v.to_string()));
                            }
                        }
                    } else if let Some(rest) = l.strip_prefix("texture_name[") {
                        if let Some((idx, val)) = rest.split_once(']') {
                            if let (Ok(i), Some(v)) = (idx.parse::<usize>(), val.split('"').nth(1)) {
                                names.push((i, v.to_string()));
                            }
                        }
                    }
                }
                for (i, n) in &names {
                    if n == "texture_nmap" {
                        if let Some((_, t)) = tex.iter().find(|(j, _)| j == i) {
                            refs.push(t.trim_start_matches('/').to_string());
                        }
                    }
                }
            }
        }
    }
    let mut patched = 0;
    for r in refs {
        let mut p = project.to_path_buf();
        for part in r.split('/') {
            p.push(part);
        }
        let Ok(txt) = std::fs::read_to_string(&p) else { continue };
        if txt.contains("usage") {
            continue;
        }
        let mut new = txt;
        if !new.ends_with('\n') {
            new.push('\n');
        }
        new.push_str("usage\ttsnormal\n");
        if std::fs::write(&p, new).is_ok() {
            patched += 1;
        }
    }
    patched
}

// Both helper tools are compiled into the executable, so copying a single
// scs2fbx.exe to another machine is enough. They are unpacked next to the
// binary on first use; a copy sitting beside the exe always wins, which is how
// you swap in a different ConverterPIX build.
const EMBEDDED_CONVERTER_PIX: &[u8] = include_bytes!("../vendor/converter_pix.exe");

fn embedded_bytes(name: &str) -> Option<&'static [u8]> {
    match name {
        "converter_pix.exe" => Some(EMBEDDED_CONVERTER_PIX),
        _ => None,
    }
}

fn cache_dir() -> PathBuf {
    // %LOCALAPPDATA% first: the folder holding the exe may well be read-only.
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(base).join("scs2fbx").join("tools");
    }
    std::env::temp_dir().join("scs2fbx").join("tools")
}

/// Find a helper tool: an explicit copy on disk if there is one, otherwise the
/// embedded copy written out to a cache directory.
///
/// `current_exe()` can hand back a relative path and a shortcut can start the
/// process in an unrelated working directory, so guessing one path is not enough
/// - which is exactly how a lone scs2fbx.exe on another machine used to fail
/// with a bare "cannot find the path specified", once per model.
pub fn resolve_tool(name: &str) -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(d) = exe.parent() {
            roots.push(d.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    for r in &roots {
        for cand in [r.join("tools").join(name), r.join(name)] {
            if cand.is_file() {
                return Some(cand);
            }
        }
    }

    let bytes = embedded_bytes(name)?;
    let dir = cache_dir();
    let out = dir.join(name);
    if let Ok(meta) = std::fs::metadata(&out) {
        if meta.len() == bytes.len() as u64 {
            return Some(out);
        }
    }
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(&out, bytes).ok()?;
    Some(out)
}

/// Windows pops a console window for every child process. ConverterPIX runs once
/// per model - around forty times for one vehicle - so without this the screen
/// flickers with black windows and steals focus from whatever else is open.
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

enum PixError {
    /// The executable itself could not be started - nothing else will work either.
    CannotRun(String),
    /// ConverterPIX ran but refused this model.
    Failed(String),
}

fn converter_pix(
    exe: &Path,
    base: &Path,
    model: &str,
    anims: &[String],
    export: &Path,
) -> Result<(), PixError> {
    let mut cmd = Command::new(exe);
    no_window(&mut cmd);
    cmd.arg("-b").arg(base).arg("-m");
    // `-m <model> [anim...]`: the animations must come right after the model,
    // before -e. ConverterPIX builds the skeleton from them.
    cmd.arg(format!("/{}", model.trim_start_matches('/')));
    for a in anims {
        cmd.arg(format!("/{}", a.trim_start_matches('/')));
    }
    let out = cmd
        .arg("-e")
        .arg(export)
        .output()
        .map_err(|e| PixError::CannotRun(e.to_string()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(PixError::Failed(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .last()
                .unwrap_or("failed")
                .to_string(),
        ))
    }
}

fn already_extracted(dir: &Path) -> bool {
    dir.join("def").is_dir() || dir.join("vehicle").is_dir()
}

/// Everything the user did not ask for lives here, so the output folder shows
/// only the model, its textures and nothing else.
pub fn work_dir(outdir: &Path) -> PathBuf {
    outdir.join("_work")
}

pub fn extracted_dir(outdir: &Path) -> PathBuf {
    work_dir(outdir).join("extracted")
}

/// Extract only, so the GUI can populate the cabin/chassis lists before the user
/// commits to a full conversion.
pub fn scan(archive: PathBuf, outdir: PathBuf, tx: Sender<Msg>) {
    let extracted = extracted_dir(&outdir);
    let _ = std::fs::create_dir_all(&outdir);
    let _ = tx.send(Msg::Step("Extracting archive".into()));
    let mut fs = match crate::archive::Archive::open(&archive) {
        Ok(f) => f,
        Err(e) => {
            let _ = tx.send(Msg::Error(e.to_string()));
            return;
        }
    };
    let _ = tx.send(Msg::Info(format!("{} - {} entries", fs.kind(), fs.entry_count())));
    if already_extracted(&extracted) {
        let _ = tx.send(Msg::Info("already extracted, reusing".into()));
    } else if let Err(e) = fs.extract_all(&extracted, true, |m, p| {
        if m.starts_with("deep scan") {
            let _ = tx.send(Msg::Info(m.to_string()));
        }
        let _ = tx.send(Msg::Progress(p * 2.0));
    }) {
        let _ = tx.send(Msg::Error(e.to_string()));
        return;
    }
    let _ = tx.send(Msg::Progress(1.0));
    // the GUI reads the vehicle list itself; this just says the files are there
    let _ = std::fs::write(outdir.join("_scan_done"), b"1");
}

pub fn run(opt: Options, tx: Sender<Msg>) {
    let step = |s: &str| { let _ = tx.send(Msg::Step(s.to_string())); };
    let info = |s: String| { let _ = tx.send(Msg::Info(s)); };
    let warn = |s: String| { let _ = tx.send(Msg::Warn(s)); };
    let prog = |p: f32| { let _ = tx.send(Msg::Progress(p)); };

    let work = work_dir(&opt.outdir);
    let extracted = extracted_dir(&opt.outdir);
    let project = work.join("converted");
    let _ = std::fs::create_dir_all(&project);

    // 0. nothing to do if this one has already been through
    if opt.skip_existing {
        let done = opt.outdir.join(format!("{}.fbx", output_stem(&opt.archive)));
        if done.is_file() {
            info(format!("already converted, skipping ({})", done.display()));
            let _ = tx.send(Msg::Done(vec![done]));
            return;
        }
    }

    // 1. extract
    step("Extracting archive");
    let mut fs = match crate::archive::Archive::open(&opt.archive) {
        Ok(f) => f,
        Err(e) => { let _ = tx.send(Msg::Error(e.to_string())); return; }
    };
    info(format!("{} - {} entries", fs.kind(), fs.entry_count()));
    if already_extracted(&extracted) {
        info("already extracted, reusing".to_string());
        prog(0.5);
    } else {
        match fs.extract_all(&extracted, true, |m, p| {
            if m.starts_with("deep scan") { let _ = tx.send(Msg::Info(m.to_string())); }
            let _ = tx.send(Msg::Progress(p * 0.5));
        }) {
            Ok((w, r, d)) => info(format!("{w} named + {r} recovered by deep scan, {d} written under their hash")),
            Err(e) => { let _ = tx.send(Msg::Error(e.to_string())); return; }
        }
    }

    if opt.sounds {
        step("Extracting sound");
        match crate::sounds::extract(&extracted, &opt.outdir, |m| info(m)) {
            Ok(r) if r.converted + r.copied + r.banks == 0 => {
                info("this mod ships no sound".to_string())
            }
            Ok(r) => {
                if r.encoder.is_none() && r.copied > 0 {
                    warn(
                        "ffmpeg not found - sound copied in its original format instead of MP3"
                            .to_string(),
                    );
                }
                info(format!(
                    "sound: {} converted to MP3, {} copied as-is, {} FMOD bank(s) \
                     (a bank needs FMOD's own tools to open)",
                    r.converted, r.copied, r.banks
                ));
            }
            Err(e) => warn(format!("could not extract sound: {e}")),
        }
    }

    // 2. resolve how the vehicle is assembled
    step("Resolving vehicle definition");
    let vehicles = sii::find_vehicles(&extracted);
    let Some(v) = vehicles.iter().find(|v| v.name == opt.vehicle).or(vehicles.first()) else {
        let _ = tx.send(Msg::Error("no truck definition found in this archive".into()));
        return;
    };
    let (accessories, warns) = sii::resolve_accessories(v, &opt.cabin, &opt.chassis);
    for w in warns { warn(w); }

    // The interior .sii names both its model and its variant, which beats
    // guessing the path by convention.
    let interiors = sii::interiors(v);
    let chosen_int = interiors
        .iter()
        .find(|i| !opt.interior_variant.is_empty()
            && (i.variant == opt.interior_variant || i.stem == opt.interior_variant))
        .or_else(|| interiors.first());
    if let Some(i) = chosen_int {
        info(format!("interior {:?} (variant {:?})", i.stem, i.variant));
    }

    let main = sii::main_models(v, &opt.cabin, &opt.chassis, chosen_int.map(|i| i.sii.as_path()));
    let Some(ext_model) = main.iter().find(|m| m.role == "exterior").map(|m| m.model.clone()) else {
        let _ = tx.send(Msg::Error(format!(
            "chassis {:?} names no model - it has neither a `model:` nor a `detail_model:` line",
            opt.chassis
        )));
        return;
    };
    for m in &main {
        info(format!("{} model: {} {:?}", m.role, m.model, m.variants));
    }

    let has_cabin_root = main.iter().any(|m| m.role == "cabin");
    let mut extras = sii::resolve_extras(v, &opt.cabin, &opt.chassis, chosen_int.map(|i| i.sii.as_path()));
    if !has_cabin_root {
        // one model plays both roles, so there is nothing named "cabin" to mount on
        for e in &mut extras {
            if e.kind == "cabin" {
                e.kind = "exterior";
            }
        }
    }
    for e in &extras {
        info(format!(
            "animated model {:?}: {} ({} animation{})",
            e.name,
            e.model,
            e.anims.len(),
            if e.anims.len() == 1 { "" } else { "s" }
        ));
    }
    info(format!("{} accessories to mount", accessories.len()));

    let wheels = sii::find_wheels(&extracted);
    match (&wheels.rim_front, &wheels.tyre) {
        (Some(r), Some(t)) => info(format!("wheels: rim {r}, tyre {t}")),
        (Some(r), None) => warn(format!("found rim {r} but no tyre model in this mod")),
        _ => warn("no wheel models in this archive - they are base-game assets".to_string()),
    }

    let paint = sii::find_paint_job(&v.def_dir);
    match &paint {
        Some(p) => info(format!(
            "paint job {:?}{}: {}",
            p.name,
            if p.airbrush { " (airbrush)" } else { "" },
            p.mask
        )),
        None => info("no paint job in this mod - the body keeps its plain colour".to_string()),
    }

    // 3. mid-format conversion
    step("Converting models to mid-format");
    let pix = if opt.converter_pix.is_file() {
        opt.converter_pix.clone()
    } else {
        match resolve_tool("converter_pix.exe") {
            Some(p) => p,
            None => {
                let _ = tx.send(Msg::Error(format!(
                    "could not provide converter_pix.exe (failed to write it to {})",
                    cache_dir().display()
                )));
                return;
            }
        }
    };

    // (model, animations) - only the animated ones carry any
    let mut models: Vec<(String, Vec<String>)> =
        main.iter().map(|m| (m.model.clone(), vec![])).collect();
    for a in &accessories {
        models.push((a.model.trim_start_matches('/').to_string(), vec![]));
    }
    for e in &extras {
        models.push((e.model.trim_start_matches('/').to_string(), e.anims.clone()));
    }
    for w in [&wheels.rim_front, &wheels.rim_rear, &wheels.tyre].into_iter().flatten() {
        models.push((w.trim_start_matches('/').to_string(), vec![]));
    }
    models.sort();
    models.dedup_by(|a, b| a.0 == b.0);
    let total = models.len().max(1);
    let mut converted_main = false;
    for (i, (m, anims)) in models.iter().enumerate() {
        match converter_pix(&pix, &extracted, m, anims, &project) {
            Ok(()) => {
                if m == &ext_model {
                    converted_main = true;
                }
            }
            // No point trying the rest: if the process will not start once it
            // will not start 40 times either.
            Err(PixError::CannotRun(e)) => {
                let _ = tx.send(Msg::Error(format!(
                    "cannot run {}: {e}",
                    pix.display()
                )));
                return;
            }
            Err(PixError::Failed(e)) => warn(format!("ConverterPIX refused {m}: {e}")),
        }
        prog(0.5 + 0.2 * (i as f32 / total as f32));
    }
    // SCS Blender Tools finds animations by walking the *model's own* directory
    // (imp/pix.py:505). ConverterPIX writes each .pia to the animation's original
    // location, which for these models is a parent directory - so put a copy
    // where the importer will actually look.
    let mut staged = 0usize;
    for e in &extras {
        let Some((model_dir, _)) = e.model.trim_start_matches('/').rsplit_once('/') else { continue };
        for a in &e.anims {
            let src = {
                let mut p = project.clone();
                for part in a.trim_start_matches('/').split('/') {
                    p.push(part);
                }
                p.set_extension("pia");
                p
            };
            let Some(file) = src.file_name() else { continue };
            let mut dst = project.clone();
            for part in model_dir.split('/') {
                dst.push(part);
            }
            dst.push(file);
            if src.is_file() && !dst.exists() && std::fs::copy(&src, &dst).is_ok() {
                staged += 1;
            }
        }
    }
    if staged > 0 {
        info(format!("{staged} animation files staged next to their model"));
    }

    if !converted_main {
        let _ = tx.send(Msg::Error(format!(
            "the body model {ext_model:?} could not be converted - there is nothing to import"
        )));
        return;
    }

    // 4. data-level repairs (must happen before Blender reads anything)
    step("Patching missing textures and normal maps");
    match write_placeholders(&project) {
        Ok(n) => info(format!("{n} placeholder textures for the usual base-game files")),
        Err(e) => warn(format!("placeholders: {e}")),
    }
    // ...and then whatever else this particular mod refers to but does not ship
    let missing = missing_textures(&extracted, &project);
    let mut made = 0usize;
    for m in &missing {
        if write_one_placeholder(&project, m, placeholder_level(m)).is_ok() {
            made += 1;
        }
    }
    if made > 0 {
        info(format!(
            "{made} more textures referenced but not shipped - filled in ({}...)",
            missing
                .iter()
                .take(3)
                .map(|s| s.rsplit('/').next().unwrap_or(s))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    info(format!("{} normal-map .tobj marked tsnormal", mark_normal_maps(&extracted, &project)));

    // The livery texture is referenced by a def, not by any material, so
    // ConverterPIX never copies it into the project the importer reads. Stage it
    // by hand.
    let paint = paint.filter(|p| {
        let mut staged = false;
        for ext in ["tobj", "dds", "png", "tga"] {
            let mut src = extracted.clone();
            let mut dst = project.clone();
            for part in p.mask.split('/') {
                src.push(part);
                dst.push(part);
            }
            src.set_extension(ext);
            dst.set_extension(ext);
            if src.is_file() && !dst.exists() {
                if let Some(parent) = dst.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                staged |= std::fs::copy(&src, &dst).is_ok();
            } else if dst.exists() {
                staged = true;
            }
        }
        if !staged {
            warn(format!("paint job texture {} is not in this archive", p.mask));
        }
        staged
    });

    // 5. assemble and write the FBX, in this process
    step("Assembling the model");
    prog(0.75);

    // Name the results after the mod and keep them - and only them - at the top
    // level, so the folder a non-technical user opens holds the model, its
    // textures and nothing else.
    let stem = output_stem(&opt.archive);
    let out_fbx = opt.outdir.join(format!("{stem}.fbx"));
    let layout = crate::assemble::Layout {
        variants_row: opt.variants_row,
        models: main.clone(),
        accessories: accessories.clone(),
        extras: extras.clone(),
        wheels: wheels.clone(),
        paint: paint.clone(),
    };

    match crate::assemble::run(
        &project,
        &extracted,
        &opt.outdir,
        &out_fbx,
        &layout,
        |m| { let _ = tx.send(Msg::Info(m)); },
        |m| { let _ = tx.send(Msg::Warn(m)); },
    ) {
        Ok(r) => {
            prog(1.0);
            info(format!(
                "{} meshes, {} tris, {} materials, {} textures, {:.2} x {:.2} x {:.2} m",
                r.meshes, r.tris, r.materials, r.textures, r.size[0], r.size[1], r.size[2]
            ));
            match std::fs::metadata(&out_fbx) {
                Ok(md) => info(format!("fbx: {:.1} MB", md.len() as f64 / 1_048_576.0)),
                Err(e) => warn(format!("could not stat the .fbx: {e}")),
            }

            if opt.cleanup {
                step("Cleaning up");
                match std::fs::remove_dir_all(&work) {
                    Ok(()) => info("removed the working folder".to_string()),
                    Err(e) => warn(format!("could not remove {}: {e}", work.display())),
                }
            } else {
                info(format!("working files kept in {}", work.display()));
            }
            let _ = tx.send(Msg::Done(vec![out_fbx]));
        }
        Err(e) => { let _ = tx.send(Msg::Error(format!("could not write the FBX: {e}"))); }
    }
}








