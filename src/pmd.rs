//! Reading the `.pmd` descriptor that belongs to a `.pmg`.
//!
//! It carries what the geometry does not: the material path behind each
//! material index, and the part/variant table. Layout from ConverterPIX's
//! `structs/pmd.h`.
//!
//! In a well-formed archive the two files share a base name and pairing is
//! trivial. In an archive with no directory listing they are both nameless, so
//! [`signature`] gives each one a shape that can be matched against the other.

use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Pmd {
    pub material_count: usize,
    pub look_count: usize,
    pub piece_count: usize,
    pub variant_count: usize,
    pub part_count: usize,
    /// material paths of the first look, in material-index order
    pub materials: Vec<String>,
}

impl Pmd {
    /// What a matching `.pmg` must agree with: piece and part counts.
    pub fn signature(&self) -> (usize, usize) {
        (self.piece_count, self.part_count)
    }
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    if o + 4 > b.len() {
        return 0;
    }
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn cstr_at(b: &[u8], o: usize) -> String {
    if o >= b.len() {
        return String::new();
    }
    let end = b[o..].iter().position(|&c| c == 0).map(|n| o + n).unwrap_or(b.len());
    String::from_utf8_lossy(&b[o..end]).to_string()
}

pub fn parse(path: &Path) -> std::io::Result<Pmd> {
    parse_bytes(&std::fs::read(path)?)
}

pub fn parse_bytes(b: &[u8]) -> std::io::Result<Pmd> {
    if b.len() < 64 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "too short for a .pmd"));
    }
    let version = u32_at(b, 0);
    if version != 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unsupported .pmd version {version}"),
        ));
    }
    let material_count = u32_at(b, 4) as usize;
    let look_count = u32_at(b, 8) as usize;
    let piece_count = u32_at(b, 12) as usize;
    let variant_count = u32_at(b, 16) as usize;
    let part_count = u32_at(b, 20) as usize;
    let material_offset = u32_at(b, 56) as usize;

    // Guard against a blob that merely starts with a 4: these counts are read
    // from unnamed files that were only guessed to be descriptors.
    if material_count > 4096 || look_count > 256 || piece_count > 65536 || part_count > 4096 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "implausible .pmd counts"));
    }

    // Look 0 holds the default set; the table is look-major.
    let mut materials = Vec::with_capacity(material_count);
    for j in 0..material_count {
        let at = material_offset + j * 4;
        let off = u32_at(b, at) as usize;
        if off == 0 || off >= b.len() {
            materials.push(String::new());
        } else {
            materials.push(cstr_at(b, off));
        }
    }

    Ok(Pmd { material_count, look_count, piece_count, variant_count, part_count, materials })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blobs_that_only_look_like_one() {
        assert!(parse_bytes(&[0u8; 10]).is_err());
        // version 4 but nonsense counts
        let mut b = vec![0u8; 128];
        b[0] = 4;
        b[4..8].copy_from_slice(&999_999u32.to_le_bytes());
        assert!(parse_bytes(&b).is_err());
    }

    #[test]
    fn reads_counts_and_signature() {
        let mut b = vec![0u8; 128];
        b[0] = 4;
        b[4..8].copy_from_slice(&2u32.to_le_bytes()); // materials
        b[8..12].copy_from_slice(&1u32.to_le_bytes()); // looks
        b[12..16].copy_from_slice(&7u32.to_le_bytes()); // pieces
        b[20..24].copy_from_slice(&3u32.to_le_bytes()); // parts
        let p = parse_bytes(&b).unwrap();
        assert_eq!(p.signature(), (7, 3));
        assert_eq!(p.materials.len(), 2);
    }
}
