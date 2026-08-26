//! Reading SCS `.pmg` geometry directly.
//!
//! ConverterPIX refuses a `.pmg` without its `.pmd` descriptor, and some
//! archives ship no directory listing at all - one trailer pack carries 221
//! models and 278 descriptors, every one of them named only by a hash, so
//! nothing can be paired back up. Reading the geometry here sidesteps the
//! pairing entirely.
//!
//! Three container versions are in circulation. 0x14 and 0x15 differ only in
//! header length; 0x13 keeps its vertex attributes in two pools rather than
//! one. Layouts follow ConverterPIX's `structs/pmg_0x1{3,4,5}.h` and its
//! loaders, which are the reference for this format.
//!
//! What is *not* here is material names: a piece stores an index, and the names
//! live in the `.pmd`. Pieces come out with placeholder aliases, so a model read
//! this way has geometry and UVs but no texture paths.

use std::path::Path;

use crate::pim::{Locator, Model, Part, Piece};

fn i32_at(b: &[u8], o: usize) -> i32 {
    if o + 4 > b.len() {
        return -1;
    }
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    if o + 8 > b.len() {
        return 0;
    }
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(v)
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    if o + 4 > b.len() {
        return 0.0;
    }
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn f3_at(b: &[u8], o: usize) -> [f32; 3] {
    [f32_at(b, o), f32_at(b, o + 4), f32_at(b, o + 8)]
}

/// SCS packs short names into a 64-bit "token": base 38, least significant
/// digit first. Nothing longer than 12 characters fits, which is why locator
/// names are always short.
///
/// The digit order is 0-9, then a-z, then underscore - solved from real pairs
/// rather than assumed. Token 0x422e5ac75 has base-38 digits
/// 13, 18, 29, 37, 7, 34, 5 and names the part "chs_6x4"; putting the letters
/// first instead yields "mr19g6e".
fn token_to_string(mut t: u64) -> String {
    const ALPHABET: &[u8] = b"\x000123456789abcdefghijklmnopqrstuvwxyz_";
    let mut s = String::new();
    while t != 0 {
        let d = (t % 38) as usize;
        t /= 38;
        if d == 0 || d >= ALPHABET.len() {
            break;
        }
        s.push(ALPHABET[d] as char);
    }
    s
}

struct Layout {
    piece_count: usize,
    part_count: usize,
    locator_count: usize,
    parts_offset: usize,
    locators_offset: usize,
    pieces_offset: usize,
    /// 0x13 splits its vertex attributes across two strides
    old: bool,
}

pub fn parse(path: &Path) -> std::io::Result<Model> {
    parse_bytes(&std::fs::read(path)?)
}

pub fn parse_bytes(b: &[u8]) -> std::io::Result<Model> {
    let bad = |m: String| std::io::Error::new(std::io::ErrorKind::InvalidData, m);
    if b.len() < 116 || &b[1..4] != b"gmP" {
        return Err(bad("not a .pmg".into()));
    }

    let lay = match b[0] {
        0x15 => Layout {
            piece_count: i32_at(b, 4).max(0) as usize,
            part_count: i32_at(b, 8).max(0) as usize,
            locator_count: i32_at(b, 20).max(0) as usize,
            parts_offset: i32_at(b, 76).max(0) as usize,
            locators_offset: i32_at(b, 80).max(0) as usize,
            pieces_offset: i32_at(b, 84).max(0) as usize,
            old: false,
        },
        // identical to 0x15 with the 8-byte skeleton hash removed
        0x14 => Layout {
            piece_count: i32_at(b, 4).max(0) as usize,
            part_count: i32_at(b, 8).max(0) as usize,
            locator_count: i32_at(b, 20).max(0) as usize,
            parts_offset: i32_at(b, 68).max(0) as usize,
            locators_offset: i32_at(b, 72).max(0) as usize,
            pieces_offset: i32_at(b, 76).max(0) as usize,
            old: false,
        },
        0x13 => Layout {
            piece_count: i32_at(b, 4).max(0) as usize,
            part_count: i32_at(b, 8).max(0) as usize,
            locator_count: i32_at(b, 16).max(0) as usize,
            parts_offset: i32_at(b, 64).max(0) as usize,
            locators_offset: i32_at(b, 68).max(0) as usize,
            pieces_offset: i32_at(b, 72).max(0) as usize,
            old: true,
        },
        v => return Err(bad(format!("unsupported .pmg version 0x{v:02x}"))),
    };

    let mut m = Model::default();

    for i in 0..lay.part_count {
        let o = lay.parts_offset + i * 24;
        let n = i32_at(b, o + 8).max(0) as usize;
        let first = i32_at(b, o + 12).max(0) as usize;
        let ln = i32_at(b, o + 16).max(0) as usize;
        let lfirst = i32_at(b, o + 20).max(0) as usize;
        m.parts.push(Part {
            name: token_to_string(u64_at(b, o)),
            pieces: (first..first + n).collect(),
            locators: (lfirst..lfirst + ln).collect(),
        });
    }

    for i in 0..lay.locator_count {
        let o = lay.locators_offset + i * 44;
        let s = f32_at(b, o + 20);
        let s = if s.abs() < 1e-9 { 1.0 } else { s };
        m.locators.push(Locator {
            name: token_to_string(u64_at(b, o)),
            position: f3_at(b, o + 8),
            // one float of uniform scale here, not three
            scale: [s, s, s],
            rotation: [
                f32_at(b, o + 24),
                f32_at(b, o + 28),
                f32_at(b, o + 32),
                f32_at(b, o + 36),
            ],
        });
    }

    let pstride = if lay.old { 104 } else { 100 };
    for i in 0..lay.piece_count {
        let o = lay.pieces_offset + i * pstride;
        let edges = i32_at(b, o).max(0) as usize;
        let verts = i32_at(b, o + 4).max(0) as usize;
        let uv_channels = i32_at(b, o + 12).max(0) as usize;
        let material = if lay.old { i32_at(b, o + 20) } else { i32_at(b, o + 16) };
        let pos_o = i32_at(b, o + 64);
        let nrm_o = i32_at(b, o + 68);
        let uv_o = i32_at(b, o + 72);
        let rgba_o = i32_at(b, o + 76);
        let fac_o = i32_at(b, o + 80);
        let tan_o = i32_at(b, o + 84);
        // 0x13 lists its triangle offset next; 0x14/0x15 put two bone streams
        // first. ConverterPIX's header comments disagree with its own field
        // order here - the declaration order is what the file actually uses,
        // confirmed by +96 holding this model's index pool offset exactly.
        let idx_o = if lay.old { i32_at(b, o + 88) } else { i32_at(b, o + 96) };

        // Stride is the sum of the attributes actually present. 0x13 counts the
        // position-side attributes separately from the colour/UV side, unless
        // the piece has no bones, in which case the two pools are one.
        let mut stat = 0usize;
        let mut dynm = 0usize;
        if pos_o != -1 {
            stat += 12;
        }
        if nrm_o != -1 {
            stat += 12;
        }
        if tan_o != -1 {
            stat += 16;
        }
        if uv_o != -1 {
            dynm += 8 * uv_channels;
        }
        if rgba_o != -1 {
            dynm += 4;
        }
        if fac_o != -1 {
            dynm += 4;
        }
        if lay.old {
            if i32_at(b, o + 16) == 0 {
                stat += dynm;
                dynm = stat;
            }
        } else {
            if i32_at(b, o + 88) != -1 {
                stat += 8;
            }
            stat += dynm;
            dynm = stat;
        }

        let mut p = Piece { material: material.max(0) as usize, ..Default::default() };
        for v in 0..verts {
            if pos_o != -1 {
                p.positions.push(f3_at(b, pos_o as usize + stat * v));
            }
            if nrm_o != -1 {
                p.normals.push(f3_at(b, nrm_o as usize + stat * v));
            }
            if uv_o != -1 {
                let base = uv_o as usize + dynm * v;
                p.uv0.push([f32_at(b, base), f32_at(b, base + 4)]);
                if uv_channels > 1 {
                    p.uv1.push([f32_at(b, base + 8), f32_at(b, base + 12)]);
                }
            }
            if rgba_o != -1 {
                let c = rgba_o as usize + dynm * v;
                if c + 4 <= b.len() {
                    // stored halved, the way every SCS shader expects it back
                    p.rgba.push([
                        2.0 * b[c] as f32 / 255.0,
                        2.0 * b[c + 1] as f32 / 255.0,
                        2.0 * b[c + 2] as f32 / 255.0,
                        b[c + 3] as f32 / 255.0,
                    ]);
                }
            }
        }
        for t in 0..edges / 3 {
            let at = idx_o as usize + t * 6;
            if idx_o < 0 || at + 6 > b.len() {
                break;
            }
            p.tris.push([
                u16::from_le_bytes([b[at], b[at + 1]]) as u32,
                u16::from_le_bytes([b[at + 2], b[at + 3]]) as u32,
                u16::from_le_bytes([b[at + 4], b[at + 5]]) as u32,
            ]);
        }
        m.pieces.push(p);
    }

    // Material names live in the .pmd; a piece only carries an index.
    let most = m.pieces.iter().map(|p| p.material).max().unwrap_or(0);
    m.materials = (0..=most).map(|i| format!("mat_{i:04}")).collect();

    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_decode_least_significant_digit_first() {
        assert_eq!(token_to_string(1), "0");
        assert_eq!(token_to_string(11), "a");
        // the pair this alphabet was solved from
        assert_eq!(token_to_string(0x0000000422e5ac75), "chs_6x4");
        assert_eq!(token_to_string(0x0000000fd69a6cfe), "b_grill");
        assert_eq!(token_to_string(0), "");
    }

    #[test]
    fn rejects_what_is_not_a_pmg() {
        assert!(parse_bytes(&[0u8; 200]).is_err());
        let mut b = vec![0u8; 200];
        b[0] = 0x15;
        b[1..4].copy_from_slice(b"gmP");
        assert!(parse_bytes(&b).is_ok());
    }
}
