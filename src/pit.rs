//! Reader for ConverterPIX's `.pit` trait format: the material table and the
//! variant/part visibility table that goes with a `.pim`.
//!
//! Materials are keyed by the same alias the `.pim` uses, so the two files join
//! on that string.

use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Material {
    pub alias: String,
    pub effect: String,
    /// `texture[0]` first; the base colour is index 0 for every eut2 shader
    pub textures: Vec<String>,
    pub diffuse: [f32; 3],
}

impl Material {
    /// Whether this shader treats the base texture's alpha as opacity.
    ///
    /// For the whole `eut2.dif.spec*` family it is a specular or reflection
    /// mask, and reading it as transparency turns solid bodywork see-through.
    /// Only the shaders that say so in their name mean opacity.
    pub fn wants_alpha(&self) -> bool {
        let e = self.effect.to_ascii_lowercase();
        if e.contains("glass") || e.contains("alpha") {
            return true;
        }
        e.split('.').any(|t| matches!(t, "a" | "atest" | "alphatest"))
    }

    pub fn is_shadow_only(&self) -> bool {
        let e = self.effect.to_ascii_lowercase();
        e.contains("shadowonly") || e.contains("fakeshadow")
    }
}

#[derive(Debug, Default)]
pub struct Trait {
    pub materials: HashMap<String, Material>,
    /// variant name -> the parts it makes visible
    pub variants: Vec<(String, HashSet<String>)>,
}

impl Trait {
    pub fn variant(&self, name: &str) -> Option<&HashSet<String>> {
        self.variants.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }
}

fn quoted(line: &str) -> String {
    let mut it = line.split('"');
    it.next();
    it.next().unwrap_or("").to_string()
}

fn values(line: &str) -> Vec<f32> {
    let Some(open) = line.find('(') else { return Vec::new() };
    let Some(close) = line[open..].find(')') else { return Vec::new() };
    line[open + 1..open + close]
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect()
}

/// Which block a line sits in. `Material` and `Part` both contain nested
/// `Attribute`/`Texture` blocks, so a flat "any `}` closes the current thing"
/// reader shuts them at the first attribute and every texture path is lost -
/// which is exactly how a whole vehicle came out untextured.
#[derive(PartialEq, Clone, Copy)]
enum Block {
    Look,
    Material,
    Variant,
    Part,
    Other,
}

pub fn parse(path: &Path) -> std::io::Result<Trait> {
    let text = std::fs::read_to_string(path)?;
    let mut t = Trait::default();
    let mut stack: Vec<Block> = Vec::new();

    // Only the first Look is read. Looks are alternative paint schemes for the
    // same geometry and the vehicle .sii picks one by name; taking the first
    // matches what the game shows by default.
    let mut look_index = 0usize;
    let mut mat: Option<Material> = None;
    let mut attr_tag = String::new();
    let mut in_variant = false;
    let mut variant_name = String::new();
    let mut variant_parts: HashSet<String> = HashSet::new();
    let mut part_name = String::new();
    let mut part_visible = true;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        if line.ends_with('{') {
            let head = line.trim_end_matches('{').trim();
            let b = match head {
                "Look" => {
                    look_index += 1;
                    Block::Look
                }
                "Variant" => {
                    in_variant = true;
                    variant_name.clear();
                    variant_parts.clear();
                    Block::Variant
                }
                "Material" if look_index <= 1 && stack.last() == Some(&Block::Look) => {
                    mat = Some(Material { diffuse: [1.0, 1.0, 1.0], ..Default::default() });
                    Block::Material
                }
                "Part" if stack.last() == Some(&Block::Variant) => {
                    part_name.clear();
                    part_visible = true;
                    Block::Part
                }
                _ => Block::Other,
            };
            stack.push(b);
            continue;
        }

        if line == "}" {
            match stack.pop() {
                Some(Block::Material) => {
                    if let Some(m) = mat.take() {
                        if !m.alias.is_empty() {
                            t.materials.entry(m.alias.clone()).or_insert(m);
                        }
                    }
                }
                Some(Block::Part) => {
                    if part_visible && !part_name.is_empty() {
                        variant_parts.insert(std::mem::take(&mut part_name));
                    }
                    part_name.clear();
                }
                Some(Block::Variant) => {
                    t.variants.push((
                        std::mem::take(&mut variant_name),
                        std::mem::take(&mut variant_parts),
                    ));
                    in_variant = false;
                }
                _ => {}
            }
            continue;
        }

        if in_variant {
            if line.starts_with("Name:") {
                match stack.last() {
                    Some(Block::Part) => part_name = quoted(line),
                    _ => variant_name = quoted(line),
                }
            } else if line.starts_with("Tag:") {
                attr_tag = quoted(line);
            } else if line.starts_with("Value:") && attr_tag == "visible" {
                part_visible = values(line).first().copied().unwrap_or(1.0) != 0.0;
            }
            continue;
        }

        let Some(m) = mat.as_mut() else { continue };
        if line.starts_with("Alias:") {
            m.alias = quoted(line);
        } else if line.starts_with("Effect:") {
            m.effect = quoted(line);
        } else if line.starts_with("Tag:") {
            attr_tag = quoted(line);
            // Texture blocks tag themselves `texture[0]:texture_base`
            if let Some(rest) = attr_tag.strip_prefix("texture[") {
                if let Some((n, _)) = rest.split_once(']') {
                    let idx: usize = n.parse().unwrap_or(0);
                    if m.textures.len() <= idx {
                        m.textures.resize(idx + 1, String::new());
                    }
                }
            }
        } else if line.starts_with("Value:") {
            if attr_tag == "diffuse" {
                let v = values(line);
                if v.len() >= 3 {
                    m.diffuse = [v[0], v[1], v[2]];
                }
            } else if let Some(rest) = attr_tag.strip_prefix("texture[") {
                if let Some((n, _)) = rest.split_once(']') {
                    let idx: usize = n.parse().unwrap_or(0);
                    let path = quoted(line);
                    if m.textures.len() <= idx {
                        m.textures.resize(idx + 1, String::new());
                    }
                    m.textures[idx] = path;
                }
            }
        }
    }

    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_is_read_from_the_shader_name() {
        let a = |e: &str| Material { effect: e.into(), ..Default::default() }.wants_alpha();
        assert!(a("eut2.glass"));
        assert!(a("eut2.dif.spec.a.decal"));
        assert!(a("eut2.dif.spec.weight.mult2.weight2.dif.spec.alpha.test"));
        // the specular-mask family must stay opaque
        assert!(!a("eut2.dif.spec"));
        assert!(!a("eut2.dif.spec.add.env"));
        assert!(!a("eut2.truckpaint.rfx"));
        assert!(!a("eut2.dif.spec.mult.dif.spec.add.env.shadow.rfx"));
    }
}

