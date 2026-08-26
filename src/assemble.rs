//! The step that used to be "run Blender with a 900-line script".
//!
//! Takes the vehicle layout `sii` worked out, places every model, and writes
//! one FBX plus a folder of textures.

use std::collections::HashMap;
use std::path::Path;

use crate::fbxscene;
use crate::scene::{self, Instance, Loader, Mat4};
use crate::sii::{Accessory, ExtraModel, MainModel, PaintJob, Wheels};

pub struct Layout {
    pub variants_row: bool,
    pub models: Vec<MainModel>,
    pub accessories: Vec<Accessory>,
    pub extras: Vec<ExtraModel>,
    pub wheels: Wheels,
    pub paint: Option<PaintJob>,
}

pub struct Report {
    pub meshes: usize,
    pub tris: usize,
    pub materials: usize,
    pub textures: usize,
    pub size: [f32; 3],
}

fn bounds(v: &[[f32; 3]]) -> Option<([f32; 3], [f32; 3])> {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in v {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    if lo[0] == f32::MAX { None } else { Some((lo, hi)) }
}

/// Every world-space vertex of a model placed at `at`, for fit tests.
fn points(loader: &mut Loader, project: &Path, model: &str, at: Mat4) -> Vec<[f32; 3]> {
    let mut out = Vec::new();
    if let Some((m, _)) = loader.get(project, model) {
        for p in &m.pieces {
            for v in &p.positions {
                out.push(at.point(*v));
            }
        }
    }
    out
}

/// Fraction of box `a` that lies inside box `b`.
fn containment(a: ([f32; 3], [f32; 3]), b: ([f32; 3], [f32; 3])) -> f32 {
    let mut vol = 1.0f32;
    let mut inter = 1.0f32;
    for i in 0..3 {
        let d = (a.1[i] - a.0[i]).max(1e-6);
        let o = (a.1[i].min(b.1[i]) - a.0[i].max(b.0[i])).max(0.0);
        vol *= d;
        inter *= o;
    }
    if vol > 0.0 { inter / vol } else { 0.0 }
}

/// Pick the placement that actually lands the model inside the body.
///
/// Which space a model is authored in is not reliably declared: `ext_interior`
/// is listed on the interior .sii but is drawn in body space, and inheriting the
/// interior's transform slid a whole second bus body out beside the real one.
/// Measuring beats maintaining a table of exceptions.
fn best_fit(
    loader: &mut Loader,
    project: &Path,
    model: &str,
    body: Option<([f32; 3], [f32; 3])>,
    candidates: &[(&str, Mat4)],
) -> (Mat4, &'static str, f32) {
    let Some(body) = body else {
        return (candidates.first().map(|c| c.1).unwrap_or(Mat4::IDENTITY), "declared", 0.0);
    };
    let mut best = (Mat4::IDENTITY, "declared", -1.0f32);
    for (label, m) in candidates {
        let Some(b) = bounds(&points(loader, project, model, *m)) else { continue };
        let s = containment(b, body);
        if s > best.2 {
            let leaked: &'static str = match *label {
                "body" => "body",
                "cabin" => "cabin",
                "interior" => "interior",
                _ => "declared",
            };
            best = (*m, leaked, s);
        }
    }
    if best.2 < 0.0 {
        best = (candidates.first().map(|c| c.1).unwrap_or(Mat4::IDENTITY), "declared", 0.0);
    }
    best
}

fn flat_name(tex: &str) -> String {
    tex.trim_start_matches('/').replace(['/', '\\'], "_")
}

/// Rewrite a DX10-header DDS as the equivalent legacy FourCC one.
///
/// The Hyundai HD 1997 ships its livery as BC3_UNORM_SRGB behind a DX10
/// extended header, and Blender 4.1 refuses it outright -
/// `IMB_ibImageFromMemory: unknown file-format` - which paints the whole truck
/// in the missing-texture magenta. The block data is identical to a plain DXT5
/// file; only the 20-byte header extension and the FourCC differ, so dropping
/// one and setting the other is lossless and costs a memcpy.
///
/// Formats with no legacy equivalent (BC6H, BC7) are left alone.
pub(crate) fn dds_from_dx10(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 148 || &bytes[0..4] != b"DDS " || &bytes[84..88] != b"DX10" {
        return None;
    }
    let dxgi = u32::from_le_bytes([bytes[128], bytes[129], bytes[130], bytes[131]]);
    let fourcc: &[u8; 4] = match dxgi {
        70..=72 => b"DXT1",   // BC1
        73..=75 => b"DXT3",   // BC2
        76..=78 => b"DXT5",   // BC3
        79..=80 => b"ATI1",   // BC4
        82..=83 => b"ATI2",   // BC5
        _ => return None,
    };
    let mut out = Vec::with_capacity(bytes.len() - 20);
    out.extend_from_slice(&bytes[..128]);
    out[84..88].copy_from_slice(fourcc);
    out.extend_from_slice(&bytes[148..]);
    Some(out)
}

/// Force a DDS fully opaque, in place, without touching its colour.
///
/// An importer decides a texture is transparent from the file, not from what
/// the FBX says: Blender wires the image's alpha into Principled Alpha whenever
/// the image has an alpha channel, and `Texture_Alpha_Source: None` does not
/// stop it. For every eut2 shader except the glass/alpha family that channel is
/// a specular or reflection mask, so honouring it turns solid bodywork
/// see-through.
///
/// Rather than decode and re-encode the image - by far the most expensive thing
/// this program could do - overwrite just the alpha half of each block with
/// "opaque everywhere". BC2/BC3 keep their colour bits untouched, so this is
/// lossless for the part that matters.
pub(crate) fn dds_force_opaque(bytes: &mut [u8]) -> bool {
    if bytes.len() < 128 || &bytes[0..4] != b"DDS " {
        return false;
    }
    let fourcc: [u8; 4] = [bytes[84], bytes[85], bytes[86], bytes[87]];
    let rgb_bit_count = u32::from_le_bytes([bytes[88], bytes[89], bytes[90], bytes[91]]);
    let alpha_mask = u32::from_le_bytes([bytes[104], bytes[105], bytes[106], bytes[107]]);
    let data = &mut bytes[128..];

    match &fourcc {
        // BC3: 8 bytes of alpha, then 8 of colour. a0 = a1 = 255 with zeroed
        // indices selects a0 for every texel.
        b"DXT5" | b"DXT4" => {
            for b in data.chunks_mut(16) {
                if b.len() == 16 {
                    b[0] = 0xFF;
                    b[1] = 0xFF;
                    b[2..8].fill(0);
                }
            }
            true
        }
        // BC2: 8 bytes of explicit 4-bit alpha
        b"DXT3" | b"DXT2" => {
            for b in data.chunks_mut(16) {
                if b.len() == 16 {
                    b[0..8].fill(0xFF);
                }
            }
            true
        }
        // BC1 has no alpha channel worth the name
        b"DXT1" => false,
        _ => {
            // uncompressed 32-bit with an alpha mask: set that byte per pixel
            if rgb_bit_count == 32 && alpha_mask != 0 {
                let shift = alpha_mask.trailing_zeros() as usize / 8;
                if shift < 4 {
                    for px in data.chunks_mut(4) {
                        if px.len() == 4 {
                            px[shift] = 0xFF;
                        }
                    }
                    return true;
                }
            }
            false
        }
    }
}

/// Copy each referenced texture next to the .fbx and rewrite the material to
/// point at it. The image files are left in whatever format the mod shipped -
/// usually DDS, which every target reads - because transcoding them would be
/// the slowest thing this program does and would gain nothing.
fn stage_textures(
    project: &Path,
    extracted: &Path,
    outdir: &Path,
    materials: &mut [fbxscene::Material],
    mut on_warn: impl FnMut(String),
) -> usize {
    let tex_dir = outdir.join("textures");
    let _ = std::fs::create_dir_all(&tex_dir);

    // A texture shared between bodywork and a window has to keep its alpha:
    // decide per image, not per material.
    let mut keep_alpha: HashMap<String, bool> = HashMap::new();
    for m in materials.iter() {
        if let Some(t) = &m.texture {
            *keep_alpha.entry(t.clone()).or_insert(false) |= m.wants_alpha;
        }
    }

    let mut done: HashMap<String, Option<String>> = HashMap::new();
    let mut written = 0usize;
    let mut missing = 0usize;
    let mut flattened = 0usize;
    let mut downgraded = 0usize;

    for m in materials.iter_mut() {
        let Some(src_rel) = m.texture.clone() else { continue };
        let alpha_here = *keep_alpha.get(&src_rel).unwrap_or(&false);
        let resolved = done.entry(src_rel.clone()).or_insert_with(|| {
            for root in [project, extracted] {
                for ext in ["dds", "tga", "png", "jpg"] {
                    let mut p = root.to_path_buf();
                    for part in src_rel.trim_start_matches('/').split('/') {
                        p.push(part);
                    }
                    p.set_extension(ext);
                    if !p.is_file() {
                        continue;
                    }
                    let name = format!("{}.{}", flat_name(&src_rel), ext);
                    let dst = tex_dir.join(&name);
                    if dst.exists() {
                        written += 1;
                        return Some(format!("textures/{name}"));
                    }
                    let Ok(mut bytes) = std::fs::read(&p) else { continue };
                    if ext == "dds" {
                        if let Some(legacy) = dds_from_dx10(&bytes) {
                            bytes = legacy;
                            downgraded += 1;
                        }
                        if !alpha_here && dds_force_opaque(&mut bytes) {
                            flattened += 1;
                        }
                    }
                    if std::fs::write(&dst, &bytes).is_ok() {
                        written += 1;
                        return Some(format!("textures/{name}"));
                    }
                }
            }
            missing += 1;
            None
        });
        m.texture = resolved.clone();
    }
    if missing > 0 {
        on_warn(format!("{missing} texture(s) referenced by materials are not in this archive"));
    }
    if flattened > 0 {
        on_warn(format!(
            "{flattened} texture(s) had their alpha channel forced opaque (it is a specular mask, not transparency)"
        ));
    }
    if downgraded > 0 {
        on_warn(format!(
            "{downgraded} DDS texture(s) rewritten from a DX10 header to legacy FourCC (Blender cannot read DX10)"
        ));
    }
    written
}

pub fn run(
    project: &Path,
    extracted: &Path,
    outdir: &Path,
    out_fbx: &Path,
    layout: &Layout,
    mut info: impl FnMut(String),
    mut warn: impl FnMut(String),
) -> std::io::Result<Report> {
    let mut loader = Loader::default();
    let mut inst: Vec<Instance> = Vec::new();

    let ext_model = layout.models.iter().find(|m| m.role == "exterior");
    let cab_model = layout.models.iter().find(|m| m.role == "cabin");
    let int_model = layout.models.iter().find(|m| m.role == "interior");

    // body first, so its locators and bounds are available to everything else
    let mut body_pts: Vec<[f32; 3]> = Vec::new();
    let mut loc: HashMap<String, Mat4> = HashMap::new();
    for m in [ext_model, cab_model].into_iter().flatten() {
        body_pts.extend(points(&mut loader, project, &m.model, Mat4::IDENTITY));
        for (k, v) in scene::locators(&mut loader, project, &m.model, Mat4::IDENTITY) {
            loc.entry(k).or_insert(v);
        }
        inst.push(Instance {
            name: m.role.to_string(),
            model: m.model.clone(),
            variants: m.variants.clone(),
            xform: Mat4::IDENTITY,
        });
    }
    let body = bounds(&body_pts);

    let int_at = if let Some(m) = int_model {
        let cand: Vec<(&str, Mat4)> = match loc.get("interior") {
            Some(l) => vec![("interior", *l), ("body", Mat4::IDENTITY)],
            None => vec![("body", Mat4::IDENTITY)],
        };
        let (at, label, score) = best_fit(&mut loader, project, &m.model, body, &cand);
        info(format!("interior placed in {label} space ({:.0}% inside the body)", score * 100.0));
        for (k, v) in scene::locators(&mut loader, project, &m.model, at) {
            loc.entry(format!("int:{k}")).or_insert(v);
        }
        inst.push(Instance {
            name: "interior".into(),
            model: m.model.clone(),
            variants: m.variants.clone(),
            xform: at,
        });
        at
    } else {
        Mat4::IDENTITY
    };

    for e in &layout.extras {
        let mut cand: Vec<(&str, Mat4)> = vec![("body", Mat4::IDENTITY)];
        if int_model.is_some() {
            cand.push(("interior", int_at));
        }
        let (at, label, score) = best_fit(&mut loader, project, &e.model, body, &cand);
        if label != "body" || score < 0.999 {
            info(format!("extra {:?} placed in {label} space ({:.0}%)", e.name, score * 100.0));
        }
        inst.push(Instance {
            name: format!("anim_{}", e.name),
            model: e.model.clone(),
            variants: e.variants.clone(),
            xform: at,
        });
    }

    let mut mounted = 0usize;
    for a in &layout.accessories {
        // exterior and cabin share the chassis space; the interior has its own,
        // so an exterior part must never borrow an interior locator of the same
        // name - that threw a bus's front panel metres clear of the body.
        let key = if a.kind == "interior" { format!("int:{}", a.slot) } else { a.slot.clone() };
        let at = match loc.get(&key).or_else(|| {
            if a.kind == "interior" { None } else { loc.get(&a.slot) }
        }) {
            Some(m) => *m,
            None => continue,
        };
        inst.push(Instance {
            name: format!("acc_{}_{}", a.slot, a.kind),
            model: a.model.clone(),
            variants: if a.variant.is_empty() { Vec::new() } else { vec![a.variant.clone()] },
            xform: at,
        });
        mounted += 1;
    }
    info(format!("accessories mounted: {mounted}"));

    let mut wheel_parts = 0usize;
    let wheel_locs: Vec<(String, Mat4)> = {
        let mut v: Vec<(String, Mat4)> = loc
            .iter()
            .filter(|(k, _)| is_wheel_locator(k))
            .map(|(k, m)| (k.clone(), *m))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    for (name, at) in &wheel_locs {
        let front = name.starts_with("wheel_f");
        let rim = if front { layout.wheels.rim_front.as_ref() } else { layout.wheels.rim_rear.as_ref() };
        for (i, model) in [rim, layout.wheels.tyre.as_ref()].into_iter().flatten().enumerate() {
            inst.push(Instance {
                name: format!("wheel{i}_{name}"),
                model: model.clone(),
                variants: Vec::new(),
                xform: *at,
            });
            wheel_parts += 1;
        }
    }
    if !wheel_locs.is_empty() {
        info(format!("wheels: {} locators, {} parts", wheel_locs.len(), wheel_parts));
    }

    let mask = layout.paint.as_ref().map(|p| format!("/{}", p.mask.trim_start_matches('/')));
    let paint_base = layout
        .paint
        .as_ref()
        .map(|p| [p.base_color.0, p.base_color.1, p.base_color.2])
        .unwrap_or([1.0, 1.0, 1.0]);
    if let Some(p) = &layout.paint {
        info(format!("paint job {:?} on the bodywork", p.name));
    }
    // Park unworn variants in a row starting one vehicle-width clear of the
    // body, spaced so nothing touches.
    let row = if layout.variants_row {
        body.map(|(lo, hi)| {
            let w = (hi[0] - lo[0]).max(1.0);
            scene::SpareRow { x0: hi[0] - lo[0] + w * 0.6, pitch: w * 1.6 }
        })
    } else {
        None
    };
    let (meshes, mut materials, st) =
        scene::build(project, &mut loader, &inst, mask.as_deref(), paint_base, row, &mut warn);
    if st.spare_slots > 0 {
        info(format!(
            "{} unworn variant(s) parked beside the vehicle ({} meshes)",
            st.spare_slots, st.spare_meshes
        ));
    }
    for m in &st.missing_models {
        warn(format!("model not converted: {m}"));
    }
    info(format!(
        "{} meshes, {} tris, {} duplicates dropped ({} tris), {} shadow proxies dropped",
        st.meshes, st.tris, st.duplicates_dropped, st.duplicate_tris, st.shadow_dropped
    ));

    let textures = stage_textures(project, extracted, outdir, &mut materials, &mut warn);

    let all: Vec<[f32; 3]> = meshes.iter().flat_map(|m| m.positions.iter().copied()).collect();
    let size = match bounds(&all) {
        Some((lo, hi)) => [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]],
        None => [0.0; 3],
    };

    if let Some(p) = out_fbx.parent() {
        std::fs::create_dir_all(p)?;
    }
    fbxscene::write_file(out_fbx, &meshes, &materials)?;

    Ok(Report {
        meshes: st.meshes,
        tris: st.tris,
        materials: materials.len(),
        textures,
        size,
    })
}

/// `wheel_f_0_0`, and also `wheel_r_0` - the trailing index is optional, and
/// demanding it left a TRACOMECO with front wheels only.
fn is_wheel_locator(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("wheel_") else { return false };
    let mut it = rest.split('_');
    match it.next() {
        Some("f") | Some("r") => {}
        _ => return false,
    }
    let mut nums = 0;
    for part in it {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        nums += 1;
    }
    (1..=2).contains(&nums)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_locator_names() {
        assert!(is_wheel_locator("wheel_f_0_0"));
        assert!(is_wheel_locator("wheel_r_3_2"));
        assert!(is_wheel_locator("wheel_r_0"));
        assert!(!is_wheel_locator("swheel"));
        assert!(!is_wheel_locator("wheel_x_0"));
        assert!(!is_wheel_locator("wheel_f"));
        assert!(!is_wheel_locator("wheel_f_0_0_0"));
    }

    #[test]
    fn containment_is_a_volume_fraction() {
        let unit = ([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        assert!((containment(unit, unit) - 1.0).abs() < 1e-6);
        let half = ([0.5, 0.0, 0.0], [1.5, 1.0, 1.0]);
        assert!((containment(half, unit) - 0.5).abs() < 1e-6);
        let outside = ([5.0, 5.0, 5.0], [6.0, 6.0, 6.0]);
        assert_eq!(containment(outside, unit), 0.0);
    }
}





