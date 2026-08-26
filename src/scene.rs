//! Assembling a vehicle out of `.pim`/`.pit` pairs and handing it to the FBX
//! writer.
//!
//! This replaces what a headless Blender used to do. Blender was never needed
//! for the geometry - the mid-format is already triangles, streams and a part
//! table - only for its SCS addon, and that addon spent most of its effort
//! building shader node graphs that were then thrown away on export.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::fbxscene;
use crate::pim;
use crate::pit;

// ------------------------------------------------------------------ maths ---

/// Column-vector 4x4, stored row-major.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]);

    /// Position, quaternion and scale, the way a `.pim` locator stores its
    /// placement.
    ///
    /// The quaternion is **(w, x, y, z)**, not (x, y, z, w). The addon settles
    /// it: it builds `Quaternion((rot[0], rot[1], rot[2], rot[3]))`, and
    /// Blender's constructor takes w first. Reading it the other way put the
    /// HOWO's cab-back panel three metres above the roof and its grille nearly
    /// two metres past the bumper.
    pub fn from_trs(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> Mat4 {
        let (w, x, y, z) = (q[0], q[1], q[2], q[3]);
        let n = (x * x + y * y + z * z + w * w).sqrt();
        let (x, y, z, w) = if n > 1e-8 { (x / n, y / n, z / n, w / n) } else { (0.0, 0.0, 0.0, 1.0) };
        let (xx, yy, zz) = (x * x, y * y, z * z);
        let (xy, xz, yz) = (x * y, x * z, y * z);
        let (wx, wy, wz) = (w * x, w * y, w * z);
        Mat4([
            (1.0 - 2.0 * (yy + zz)) * s[0], (2.0 * (xy - wz)) * s[1], (2.0 * (xz + wy)) * s[2], t[0],
            (2.0 * (xy + wz)) * s[0], (1.0 - 2.0 * (xx + zz)) * s[1], (2.0 * (yz - wx)) * s[2], t[1],
            (2.0 * (xz - wy)) * s[0], (2.0 * (yz + wx)) * s[1], (1.0 - 2.0 * (xx + yy)) * s[2], t[2],
            0.0, 0.0, 0.0, 1.0,
        ])
    }

    pub fn point(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        [
            m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + m[3],
            m[4] * p[0] + m[5] * p[1] + m[6] * p[2] + m[7],
            m[8] * p[0] + m[9] * p[1] + m[10] * p[2] + m[11],
        ]
    }

    /// `self * other`, used to take a locator's local placement into world space.
    pub fn mul(&self, other: &Mat4) -> Mat4 {
        let mut r = [0.0f32; 16];
        for i in 0..4 {
            for j in 0..4 {
                let mut s = 0.0;
                for k in 0..4 {
                    s += self.0[i * 4 + k] * other.0[k * 4 + j];
                }
                r[i * 4 + j] = s;
            }
        }
        Mat4(r)
    }
}

// --------------------------------------------------------------- instances ---

/// Where to park the variants the vehicle is not currently wearing.
///
/// Throwing them away loses half of what a mod ships - the alternative bumpers,
/// roof bars and bunk layouts - but leaving them in place stacks them inside the
/// bodywork. Parking them in a row beside the vehicle keeps both.
#[derive(Clone, Copy)]
pub struct SpareRow {
    /// where the row starts, on the lateral axis
    pub x0: f32,
    /// centre-to-centre spacing between slots
    pub pitch: f32,
}

pub struct Instance {
    /// object name prefix in the output
    pub name: String,
    /// path under the project, without extension
    pub model: String,
    /// variants to show; empty means pick one
    pub variants: Vec<String>,
    pub xform: Mat4,
}

#[derive(Default)]
pub struct Stats {
    pub instances: usize,
    pub meshes: usize,
    pub tris: usize,
    pub duplicates_dropped: usize,
    pub duplicate_tris: usize,
    pub shadow_dropped: usize,
    /// meshes belonging to variants parked beside the vehicle
    pub spare_meshes: usize,
    pub spare_slots: usize,
    pub missing_models: Vec<String>,
}

/// Cache: a wheel model gets asked for once per locator, an accessory once per
/// slot, and reading the same 40 MB `.pim` six times is the whole cost.
#[derive(Default)]
pub struct Loader {
    models: HashMap<String, Option<(pim::Model, pit::Trait)>>,
}

impl Loader {
    pub fn get(&mut self, project: &Path, model: &str) -> Option<&(pim::Model, pit::Trait)> {
        if !self.models.contains_key(model) {
            let base = model_path(project, model);
            // Prefer the mid-format: it comes with a .pit, so materials keep
            // their names and texture paths. Fall back to reading the .pmg
            // directly for archives ConverterPIX cannot touch - the ones whose
            // .pmd descriptor was never recoverable.
            let loaded = pim::parse(&base.with_extension("pim"))
                .or_else(|_| crate::pmg::parse(&base.with_extension("pmg")))
                .ok()
                .map(|m| {
                    let t = pit::parse(&base.with_extension("pit")).unwrap_or_default();
                    (m, t)
                });
            self.models.insert(model.to_string(), loaded);
        }
        self.models.get(model).and_then(|o| o.as_ref())
    }
}

fn model_path(project: &Path, model: &str) -> PathBuf {
    let mut p = project.to_path_buf();
    for part in model.trim_start_matches('/').split('/') {
        p.push(part);
    }
    p
}

/// World-space placement of every locator in a model, keyed by locator name.
pub fn locators(loader: &mut Loader, project: &Path, model: &str, at: Mat4) -> HashMap<String, Mat4> {
    let mut out = HashMap::new();
    let Some((m, _)) = loader.get(project, model) else { return out };
    for l in &m.locators {
        let local = Mat4::from_trs(l.position, l.rotation, l.scale);
        out.entry(l.name.clone()).or_insert(at.mul(&local));
    }
    out
}

// ------------------------------------------------------------------ build ---

fn quantise(p: [f32; 3]) -> [i32; 3] {
    [
        (p[0] * 1000.0).round() as i32,
        (p[1] * 1000.0).round() as i32,
        (p[2] * 1000.0).round() as i32,
    ]
}

/// Fingerprint a mesh by its world-space vertex set.
///
/// Sorted, not in vertex order: the same panel reaching the scene through two
/// different code paths comes out with its vertices laid out differently, and
/// hashing them in order let both copies survive - TRACOMECO ships one 2254
/// triangle panel twice, in exactly the same place.
fn geometry_key(positions: &[[f32; 3]]) -> u64 {
    let mut pts: Vec<[i32; 3]> = positions.iter().map(|p| quantise(*p)).collect();
    pts.sort_unstable();
    let mut h: u64 = 0xcbf29ce484222325;
    for p in &pts {
        for v in p {
            h ^= *v as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

pub fn build(
    project: &Path,
    loader: &mut Loader,
    instances: &[Instance],
    // livery texture path and the colour unpainted areas take, applied to
    // `eut2.truckpaint` panels
    paint_mask: Option<&str>,
    paint_base: [f32; 3],
    spare_row: Option<SpareRow>,
    mut on_warn: impl FnMut(String),
) -> (Vec<fbxscene::Mesh>, Vec<fbxscene::Material>, Stats) {
    let mut meshes = Vec::new();
    let mut materials: Vec<fbxscene::Material> = Vec::new();
    let mut mat_index: HashMap<String, usize> = HashMap::new();
    let mut seen_geo: HashSet<u64> = HashSet::new();
    let mut st = Stats::default();
    let mut spare_slots: HashMap<String, usize> = HashMap::new();

    for inst in instances {
        let Some((model, trait_)) = loader.get(project, &inst.model) else {
            st.missing_models.push(inst.model.clone());
            continue;
        };
        st.instances += 1;

        let mut visible: Option<HashSet<String>> = None;
        for v in &inst.variants {
            if let Some(parts) = trait_.variant(v) {
                visible.get_or_insert_with(HashSet::new).extend(parts.iter().cloned());
            }
        }
        // A model that offers variants must show exactly one of them. Showing
        // all - which is what happens with no variant named - stacks every
        // alternative roof bar, exhaust and bumper in place at once; on the
        // HOWO that pushed the bounding box from 7.2 x 4.0 m out to 9.5 x 7.2 m.
        // Pick alphabetically so the same archive always converts the same way.
        if visible.is_none() && !trait_.variants.is_empty() {
            let mut order: Vec<usize> = (0..trait_.variants.len()).collect();
            order.sort_by(|&a, &b| trait_.variants[a].0.cmp(&trait_.variants[b].0));
            let pick = order[0];
            if !inst.variants.is_empty() {
                on_warn(format!(
                    "{}: variant {:?} not in this model, using {:?} of {}",
                    inst.name,
                    inst.variants,
                    trait_.variants[pick].0,
                    trait_.variants.len()
                ));
            }
            visible = Some(trait_.variants[pick].1.clone());
        }

        // One pass in place for the variant the vehicle wears, then one per
        // variant it does not - each parked further along the row, and carrying
        // only the parts that differ, because anything shared is caught by the
        // duplicate check below and dropped.
        let worn = visible.clone();
        let mut passes: Vec<(String, Option<HashSet<String>>, f32)> =
            vec![(String::new(), visible, 0.0)];
        if let Some(row) = spare_row {
            for (vname, parts) in &trait_.variants {
                if worn.as_ref().is_some_and(|w| w == parts) {
                    continue;
                }
                let slot = {
                    let n = spare_slots.len();
                    *spare_slots.entry(format!("{}|{}", inst.model, vname)).or_insert(n)
                };
                passes.push((
                    format!("_alt_{vname}"),
                    Some(parts.clone()),
                    row.x0 + row.pitch * slot as f32,
                ));
            }
        }

        let piece_part = model.part_of_piece();
        for (suffix, vis_set, dx) in &passes {
            for (pi, piece) in model.pieces.iter().enumerate() {
                if piece.positions.is_empty() || piece.tris.is_empty() {
                    continue;
                }
                if let Some(vis) = vis_set {
                    let part = piece_part.get(&pi).map(|s| s.as_str()).unwrap_or("defaultpart");
                    if part != "defaultpart" && !vis.contains(part) {
                        continue;
                    }
                }

                let alias = model.materials.get(piece.material).cloned().unwrap_or_default();
                let mat = trait_.materials.get(&alias).cloned().unwrap_or_default();
                if mat.is_shadow_only() {
                    st.shadow_dropped += 1;
                    continue;
                }

                let positions: Vec<[f32; 3]> = piece
                    .positions
                    .iter()
                    .map(|p| {
                        let w = inst.xform.point(*p);
                        [w[0] + dx, w[1], w[2]]
                    })
                    .collect();
                let key = geometry_key(&positions);
                if !seen_geo.insert(key) {
                    st.duplicates_dropped += 1;
                    st.duplicate_tris += piece.tris.len();
                    continue;
                }

                // `eut2.truckpaint` draws a flat colour until a paint job
                // supplies its mask, which is why a mod shipping six liveries
                // converted to a blank white cab. The game multiplies mask over
                // base texture on a separate UV channel; FBX cannot express that,
                // so the livery - the part anyone looking at the model expects to
                // see - becomes the panel's texture.
                let mut tex = mat.textures.first().cloned().unwrap_or_default();
                let mut uv_set = "UVMap".to_string();
                let mut use_uv1 = false;
                if let Some(mask) = paint_mask {
                    if mat.effect.contains("truckpaint") {
                        tex = mask.to_string();
                        // Sample it on the paint channel when the panel has one.
                        if !piece.uv1.is_empty() {
                            uv_set = fbxscene::PAINT_UV.to_string();
                            use_uv1 = true;
                        }
                    }
                }
                let mkey = format!("{}|{}|{}", alias, tex, uv_set);
                let mi = *mat_index.entry(mkey).or_insert_with(|| {
                    materials.push(fbxscene::Material {
                        name: if alias.is_empty() { "material".into() } else { alias.clone() },
                        diffuse: if mat.effect.contains("truckpaint") {
                            // what the shader shows on panels the livery does not cover
                            paint_base
                        } else if mat.diffuse == [0.0, 0.0, 0.0] {
                            [1.0, 1.0, 1.0]
                        } else {
                            mat.diffuse
                        },
                        texture: if tex.is_empty() { None } else { Some(tex.clone()) },
                        normal_map: None,
                        wants_alpha: mat.wants_alpha(),
                        uv_set: uv_set.clone(),
                    });
                    materials.len() - 1
                });

                st.tris += piece.tris.len();
                if !suffix.is_empty() {
                    st.spare_meshes += 1;
                }
                meshes.push(fbxscene::Mesh {
                    name: format!("{}{}_{}", inst.name, suffix, pi),
                    positions,
                    uvs: piece.uv0.clone(),
                    uvs2: if use_uv1 { piece.uv1.clone() } else { Vec::new() },
                    colors: piece.rgba.clone(),
                    tris: piece.tris.clone(),
                    material: mi,
                });
            }
        }
    }

    st.meshes = meshes.len();
    st.spare_slots = spare_slots.len();
    (meshes, materials, st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_neutral() {
        let m = Mat4::from_trs([1.0, 2.0, 3.0], [1.0, 0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let p = m.point([0.0, 0.0, 0.0]);
        assert!((p[0] - 1.0).abs() < 1e-6 && (p[1] - 2.0).abs() < 1e-6 && (p[2] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn quarter_turn_about_y_sends_x_to_minus_z() {
        // (w, x, y, z) with w = cos(45), y = sin(45)
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let m = Mat4::from_trs([0.0; 3], [s, 0.0, s, 0.0], [1.0; 3]);
        let p = m.point([1.0, 0.0, 0.0]);
        assert!(p[0].abs() < 1e-5, "x = {}", p[0]);
        assert!((p[2] + 1.0).abs() < 1e-5, "z = {}", p[2]);
    }

    #[test]
    fn duplicate_key_ignores_vertex_order() {
        let a = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let b = [[0.0, 1.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        assert_eq!(geometry_key(&a), geometry_key(&b));
        let c = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 2.0, 0.0]];
        assert_ne!(geometry_key(&a), geometry_key(&c));
    }
}


