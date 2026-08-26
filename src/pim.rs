//! Reader for ConverterPIX's `.pim` mid-format.
//!
//! It is a plain-text block format. Floats are written as `&` followed by the
//! eight hex digits of their IEEE-754 bits, which is exact - no decimal
//! round-tripping - and cheap to parse.
//!
//! ```text
//! Piece {
//!      Index: 0
//!      Material: 0
//!      Stream { Format: FLOAT3   Tag: "_POSITION"   0 ( &3f92bcc8 &403d27ae &bfdc47fc ) ... }
//!      Triangles { 0 ( 0 1 2 ) ... }
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Piece {
    pub material: usize,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uv0: Vec<[f32; 2]>,
    pub uv1: Vec<[f32; 2]>,
    pub rgba: Vec<[f32; 4]>,
    pub tris: Vec<[u32; 3]>,
}

#[derive(Debug, Default, Clone)]
pub struct Part {
    pub name: String,
    pub pieces: Vec<usize>,
    pub locators: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Locator {
    pub name: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

#[derive(Debug, Default)]
pub struct Model {
    /// material alias per index, as referenced by `Piece.Material`
    pub materials: Vec<String>,
    pub pieces: Vec<Piece>,
    pub parts: Vec<Part>,
    pub locators: Vec<Locator>,
}

impl Model {
    /// Which part a piece belongs to, by piece index.
    pub fn part_of_piece(&self) -> HashMap<usize, String> {
        let mut m = HashMap::new();
        for p in &self.parts {
            for &i in &p.pieces {
                m.insert(i, p.name.clone());
            }
        }
        m
    }
}

/// `&3f800000` -> 1.0. Anything else parses as a plain decimal, which is what
/// the `.pit` side uses.
fn num(tok: &str) -> f32 {
    let t = tok.trim();
    if let Some(hex) = t.strip_prefix('&') {
        u32::from_str_radix(hex, 16).map(f32::from_bits).unwrap_or(0.0)
    } else {
        t.parse().unwrap_or(0.0)
    }
}

/// Values inside `( ... )` on a line, ignoring any leading index column.
fn tuple_on(line: &str) -> Vec<f32> {
    let Some(open) = line.find('(') else { return Vec::new() };
    let Some(close) = line[open..].find(')') else { return Vec::new() };
    line[open + 1..open + close]
        .split_whitespace()
        .map(num)
        .collect()
}

fn quoted(line: &str) -> String {
    let mut it = line.split('"');
    it.next();
    it.next().unwrap_or("").to_string()
}

fn after_colon(line: &str) -> &str {
    line.split_once(':').map(|(_, r)| r.trim()).unwrap_or("")
}

pub fn parse(path: &Path) -> std::io::Result<Model> {
    let text = std::fs::read_to_string(path)?;
    let mut m = Model::default();

    // Block state. The format nests only two deep (Piece > Stream), so a couple
    // of flags beat a general parser here.
    let mut piece: Option<Piece> = None;
    let mut stream_tag = String::new();
    let mut in_stream = false;
    let mut in_tris = false;
    let mut part: Option<Part> = None;
    let mut loc: Option<Locator> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with("Material {") || line == "Material" {
            // alias comes on the next line; handled by the Alias arm below
            continue;
        }
        if line.starts_with("Alias:") && piece.is_none() && part.is_none() {
            m.materials.push(quoted(line));
            continue;
        }

        if line.starts_with("Piece {") {
            piece = Some(Piece::default());
            continue;
        }
        if line.starts_with("Part {") {
            part = Some(Part::default());
            continue;
        }
        if line.starts_with("Locator {") {
            loc = Some(Locator {
                name: String::new(),
                position: [0.0; 3],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0; 3],
            });
            continue;
        }
        if line.starts_with("Stream {") {
            in_stream = true;
            stream_tag.clear();
            continue;
        }
        if line.starts_with("Triangles") {
            in_tris = true;
            continue;
        }

        if line == "}" {
            if in_stream {
                in_stream = false;
                stream_tag.clear();
            } else if in_tris {
                in_tris = false;
            } else if let Some(p) = piece.take() {
                m.pieces.push(p);
            } else if let Some(p) = part.take() {
                m.parts.push(p);
            } else if let Some(l) = loc.take() {
                m.locators.push(l);
            }
            continue;
        }

        if let Some(l) = loc.as_mut() {
            if line.starts_with("Name:") {
                l.name = quoted(line);
            } else if line.starts_with("Position:") {
                let v = tuple_on(line);
                if v.len() >= 3 {
                    l.position = [v[0], v[1], v[2]];
                }
            } else if line.starts_with("Rotation:") {
                let v = tuple_on(line);
                if v.len() >= 4 {
                    l.rotation = [v[0], v[1], v[2], v[3]];
                }
            } else if line.starts_with("Scale:") {
                let v = tuple_on(line);
                if v.len() >= 3 {
                    l.scale = [v[0], v[1], v[2]];
                }
            }
            continue;
        }

        if let Some(p) = part.as_mut() {
            if line.starts_with("Name:") {
                p.name = quoted(line);
            } else if line.starts_with("Pieces:") {
                p.pieces = after_colon(line)
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
            } else if line.starts_with("Locators:") {
                p.locators = after_colon(line)
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
            }
            continue;
        }

        let Some(p) = piece.as_mut() else { continue };

        if in_tris {
            let v = tuple_on(line);
            if v.len() >= 3 {
                p.tris.push([v[0] as u32, v[1] as u32, v[2] as u32]);
            }
            continue;
        }

        if in_stream {
            if line.starts_with("Tag:") {
                stream_tag = quoted(line);
                continue;
            }
            if line.starts_with("Format:") || line.starts_with("Alias") {
                continue;
            }
            let v = tuple_on(line);
            if v.is_empty() {
                continue;
            }
            match stream_tag.as_str() {
                "_POSITION" if v.len() >= 3 => p.positions.push([v[0], v[1], v[2]]),
                "_NORMAL" if v.len() >= 3 => p.normals.push([v[0], v[1], v[2]]),
                "_UV0" if v.len() >= 2 => p.uv0.push([v[0], v[1]]),
                "_UV1" if v.len() >= 2 => p.uv1.push([v[0], v[1]]),
                "_RGBA" if v.len() >= 4 => p.rgba.push([v[0], v[1], v[2], v[3]]),
                "_RGB" if v.len() >= 3 => p.rgba.push([v[0], v[1], v[2], 1.0]),
                _ => {}
            }
            continue;
        }

        if line.starts_with("Material:") {
            p.material = after_colon(line).parse().unwrap_or(0);
        }
    }

    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_floats() {
        assert_eq!(num("&3f800000"), 1.0);
        assert_eq!(num("&00000000"), 0.0);
        assert_eq!(num("&bf800000"), -1.0);
        assert_eq!(num("2.5"), 2.5);
    }

    #[test]
    fn tuples_ignore_the_index_column() {
        assert_eq!(tuple_on("   0    ( &3f800000  &00000000  &bf800000 )"), vec![1.0, 0.0, -1.0]);
        assert_eq!(tuple_on("Position: ( &00000000 &00000000 &00000000 )"), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn quoted_values() {
        assert_eq!(quoted("     Name: \"defaultpart\""), "defaultpart");
        assert_eq!(quoted("Tag: \"_POSITION\""), "_POSITION");
    }
}

