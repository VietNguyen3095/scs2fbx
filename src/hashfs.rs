//! HashFS v1 reader.
//!
//! Layout, established by reading real archives rather than from documentation:
//!
//! ```text
//! header   "SCS#" | u16 version | u16 salt | [4] hash method ("CITY")
//!          u32 entry_count | u32 entry_table_start
//! entry    u64 hash | u64 offset | u32 flags | u32 crc | u32 size | u32 zsize   (32 bytes)
//! flags    bit0 = directory, bit1 = zlib compressed
//! ```
//!
//! Entries are keyed by [`crate::cityhash::city_hash64`] of the path, with no
//! leading slash; the archive root is the empty path. A directory entry's payload
//! is plain UTF-8 text, one child name per line, `*` prefixed for subdirectories.
//!
//! Not every entry is reachable from the root: a mod may ship files that no
//! directory listing mentions. Those can only be recovered by scanning the files
//! that ARE reachable for path-like strings ("deep" mode), and whatever is still
//! unresolved gets written out under its hash.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::cityhash::city_hash64;

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub hash: u64,
    pub offset: u64,
    pub flags: u32,
    pub size: u32,
    pub zsize: u32,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn is_compressed(&self) -> bool {
        self.flags & 2 != 0
    }
}

pub struct HashFs {
    file: File,
    pub version: u16,
    #[allow(dead_code)] // read from the header, kept for diagnostics
    pub salt: u16,
    pub entries: HashMap<u64, Entry>,
}

#[derive(Debug)]
pub enum ScsError {
    Io(std::io::Error),
    NotHashFs,
    UnsupportedVersion(u16),
    Inflate(String),
    /// The zip declares its entries encrypted. In the archives seen so far this
    /// is a "protector" flag rather than real encryption - the payload is plain
    /// deflate - but the flag is the author saying the mod is not to be opened
    /// by other tools, so it is honoured rather than worked around.
    Encrypted,
}

impl std::fmt::Display for ScsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScsError::Io(e) => write!(f, "io error: {e}"),
            ScsError::NotHashFs => write!(f, "not an SCS# archive (a plain .zip .scs is not supported yet)"),
            ScsError::UnsupportedVersion(v) => write!(
                f,
                "this is a HashFS v{v} archive (game 1.50+); only v1 is implemented so far"
            ),
            ScsError::Inflate(e) => write!(f, "decompression failed: {e}"),
            ScsError::Encrypted => write!(
                f,
                "this .scs is marked protected by its author - ask them for a copy \
                 you are allowed to convert"
            ),
        }
    }
}

impl From<std::io::Error> for ScsError {
    fn from(e: std::io::Error) -> Self {
        ScsError::Io(e)
    }
}

type Result<T> = std::result::Result<T, ScsError>;

impl HashFs {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut hdr = [0u8; 20];
        file.read_exact(&mut hdr)?;
        if &hdr[0..4] != b"SCS#" {
            return Err(ScsError::NotHashFs);
        }
        let version = u16::from_le_bytes([hdr[4], hdr[5]]);
        let salt = u16::from_le_bytes([hdr[6], hdr[7]]);
        if version != 1 {
            return Err(ScsError::UnsupportedVersion(version));
        }
        let count = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]) as usize;
        let start = u32::from_le_bytes([hdr[16], hdr[17], hdr[18], hdr[19]]) as u64;

        file.seek(SeekFrom::Start(start))?;
        let mut raw = vec![0u8; count * 32];
        file.read_exact(&mut raw)?;

        let mut entries = HashMap::with_capacity(count);
        for i in 0..count {
            let b = &raw[i * 32..i * 32 + 32];
            let e = Entry {
                hash: u64::from_le_bytes(b[0..8].try_into().unwrap()),
                offset: u64::from_le_bytes(b[8..16].try_into().unwrap()),
                flags: u32::from_le_bytes(b[16..20].try_into().unwrap()),
                size: u32::from_le_bytes(b[24..28].try_into().unwrap()),
                zsize: u32::from_le_bytes(b[28..32].try_into().unwrap()),
            };
            entries.insert(e.hash, e);
        }
        Ok(HashFs { file, version, salt, entries })
    }

    pub(crate) fn read_entry(&mut self, e: &Entry) -> Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(e.offset))?;
        let mut buf = vec![0u8; e.zsize as usize];
        self.file.read_exact(&mut buf)?;
        if e.is_compressed() {
            use flate2::read::ZlibDecoder;
            let mut out = Vec::with_capacity(e.size as usize);
            ZlibDecoder::new(&buf[..])
                .read_to_end(&mut out)
                .map_err(|e| ScsError::Inflate(e.to_string()))?;
            Ok(out)
        } else {
            Ok(buf)
        }
    }

    /// Read one entry by its archive path, if it is there.
    pub fn read_path(&mut self, path: &str) -> Option<Vec<u8>> {
        let h = city_hash64(path.trim_start_matches('/').as_bytes());
        let e = *self.entries.get(&h)?;
        if e.is_dir() {
            return None;
        }
        self.read_entry(&e).ok()
    }

    /// Walk every path reachable from the archive root.
    pub fn list_tree(&mut self) -> Result<(Vec<String>, Vec<String>)> {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();
        let mut stack = vec![String::new()];
        while let Some(p) = stack.pop() {
            let h = city_hash64(p.as_bytes());
            let Some(e) = self.entries.get(&h).copied() else { continue };
            if !seen.insert(h) {
                continue;
            }
            if e.is_dir() {
                let data = self.read_entry(&e)?;
                dirs.push(p.clone());
                for line in String::from_utf8_lossy(&data).split('\n') {
                    let name = line.trim_end_matches('\r');
                    if name.is_empty() {
                        continue;
                    }
                    let child = name.strip_prefix('*').unwrap_or(name);
                    stack.push(if p.is_empty() {
                        child.to_string()
                    } else {
                        format!("{p}/{child}")
                    });
                }
            } else {
                files.push(p);
            }
        }
        files.sort();
        dirs.sort();
        Ok((files, dirs))
    }

    /// Extract to `dest`. Returns (written, recovered_by_deep_scan, dumped_by_hash).
    pub fn extract_all(
        &mut self,
        dest: &Path,
        deep: bool,
        mut progress: impl FnMut(&str, f32),
    ) -> Result<(usize, usize, usize)> {
        let (files, _dirs) = self.list_tree()?;
        let mut resolved: HashSet<u64> = HashSet::new();
        let mut written = 0usize;

        let total = files.len().max(1);
        for (i, p) in files.iter().enumerate() {
            let h = city_hash64(p.as_bytes());
            let Some(e) = self.entries.get(&h).copied() else { continue };
            let data = self.read_entry(&e)?;
            write_file(dest, p, &data)?;
            resolved.insert(h);
            written += 1;
            if i % 64 == 0 {
                progress(&format!("extracting {p}"), 0.35 * (i as f32 / total as f32));
            }
        }

        // Anything the directory tree never mentioned: harvest path strings out of
        // the files we did get, and see which unresolved hashes they unlock.
        // One pass only gets one hop: def/ names models, models name materials,
        // materials name textures. Keep scanning whatever the previous round
        // recovered until a round finds nothing new.
        let mut recovered = 0usize;
        if deep {
            let mut frontier: Vec<String> = files.clone();
            let mut tried: HashSet<String> = HashSet::new();
            let mut round = 0;

            // Scan the entries the tree never named, too.
            //
            // Only naming what is already named is circular, and it loses a
            // great deal: a trailer pack whose listing covers 33 of its 2399
            // entries still holds 679 material files, and every one of them
            // spells out the full path of the textures it uses. Those strings
            // are the only way back to a name for them. One pass over the
            // archive up front is cheap next to what it unlocks.
            let unnamed: Vec<Entry> = self
                .entries
                .values()
                .filter(|e| !e.is_dir() && !resolved.contains(&e.hash))
                .copied()
                .collect();
            if !unnamed.is_empty() {
                progress(
                    &format!("deep scan: reading {} unnamed entries", unnamed.len()),
                    0.34,
                );
                let mut seeds: HashSet<String> = HashSet::new();
                for e in &unnamed {
                    let data = self.read_entry(e)?;
                    let data = crate::sii3nk::decode(&data).unwrap_or(data);
                    harvest_paths(&data, "", &mut seeds);
                }
                // A path also tells us the folders above it, and a folder's own
                // entry - which is just a list of its children - is very often
                // still in the table even when nothing links to it from the
                // root. Walking into those listings recovers the files no
                // string in the archive ever spells out: the models themselves.
                let mut dirs: HashSet<String> = HashSet::new();
                for cand in &seeds {
                    let mut d = cand.as_str();
                    while let Some((parent, _)) = d.rsplit_once('/') {
                        if !dirs.insert(parent.to_string()) {
                            break;
                        }
                        d = parent;
                    }
                }
                let mut from_dirs: HashSet<String> = HashSet::new();
                for d in &dirs {
                    let h = city_hash64(d.as_bytes());
                    let Some(e) = self.entries.get(&h).copied() else { continue };
                    if !e.is_dir() {
                        continue;
                    }
                    let Ok(data) = self.read_entry(&e) else { continue };
                    for line in String::from_utf8_lossy(&data).split('\n') {
                        let name = line.trim_end_matches('\r').trim();
                        if name.is_empty() {
                            continue;
                        }
                        let child = name.strip_prefix('*').unwrap_or(name);
                        from_dirs.insert(if d.is_empty() {
                            child.to_string()
                        } else {
                            format!("{d}/{child}")
                        });
                    }
                }

                for cand in seeds.into_iter().chain(from_dirs) {
                    if !tried.insert(cand.clone()) {
                        continue;
                    }
                    let h = city_hash64(cand.as_bytes());
                    if resolved.contains(&h) {
                        continue;
                    }
                    let Some(e) = self.entries.get(&h).copied() else { continue };
                    if e.is_dir() {
                        // keep walking: a folder can hold folders
                        frontier.push(cand);
                        continue;
                    }
                    let data = self.read_entry(&e)?;
                    write_file(dest, &cand, &data)?;
                    resolved.insert(h);
                    recovered += 1;
                    frontier.push(cand);
                }
            }
            while !frontier.is_empty() {
                round += 1;
                progress(
                    &format!("deep scan round {round}: {} files to scan", frontier.len()),
                    0.35 + 0.1_f32.min(round as f32 * 0.02),
                );
                let mut candidates: HashSet<String> = HashSet::new();
                for p in &frontier {
                    let h = city_hash64(p.as_bytes());
                    let Some(e) = self.entries.get(&h).copied() else { continue };
                    let data = self.read_entry(&e)?;
                    // An obfuscated definition holds the same path references as
                    // a plain one, just XORed. Harvesting the raw bytes finds
                    // nothing, so the models those files name stay nameless and
                    // land in `_unknown` - which is how five car mods converted
                    // to an empty scene while their bodywork sat in the archive.
                    let data = crate::sii3nk::decode(&data).unwrap_or(data);
                    let dir = p.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                    harvest_paths(&data, dir, &mut candidates);
                }

                let mut next = Vec::new();
                for cand in candidates {
                    if !tried.insert(cand.clone()) {
                        continue;
                    }
                    let h = city_hash64(cand.as_bytes());
                    if resolved.contains(&h) {
                        continue;
                    }
                    let Some(e) = self.entries.get(&h).copied() else { continue };
                    if e.is_dir() {
                        continue;
                    }
                    let data = self.read_entry(&e)?;
                    write_file(dest, &cand, &data)?;
                    resolved.insert(h);
                    recovered += 1;
                    next.push(cand);
                }
                progress(
                    &format!("deep scan round {round}: recovered {} new files", next.len()),
                    0.45,
                );
                frontier = next;
            }
        }

        // Whatever is still unnamed still has to come out, keyed by its hash.
        let mut dumped = 0usize;
        let leftovers: Vec<Entry> = self
            .entries
            .values()
            .filter(|e| !e.is_dir() && !resolved.contains(&e.hash))
            .copied()
            .collect();
        for e in leftovers {
            let data = self.read_entry(&e)?;
            let ext = sniff_extension(&data);
            let name = format!("_unknown/{:016x}{}", e.hash, ext);
            write_file(dest, &name, &data)?;
            dumped += 1;
        }

        progress("extraction done", 0.5);
        Ok((written, recovered, dumped))
    }
}

fn write_file(dest: &Path, rel: &str, data: &[u8]) -> Result<()> {
    let mut out = PathBuf::from(dest);
    for part in rel.split('/') {
        out.push(part);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out, data)?;
    Ok(())
}

/// Pull plausible archive paths out of a blob. SCS text formats (.sii, .mat,
/// .tobj) reference other files as `/dir/file.ext`, which is exactly what the
/// entry table is keyed by once the leading slash is removed.
fn harvest_paths(data: &[u8], owner_dir: &str, out: &mut HashSet<String>) {
    const SIBLINGS: [&str; 9] = ["pmg", "pmd", "pmc", "pma", "pms", "pit", "pis", "tobj", "dds"];

    let mut emit = |token: &str| {
        let t = token.trim_start_matches('/').to_ascii_lowercase();
        if t.is_empty() {
            return;
        }
        out.insert(t.clone());
        // model/material references name one member of a family; the archive holds
        // the others under the same stem
        if let Some((stem, _ext)) = t.rsplit_once('.') {
            for ext in SIBLINGS {
                out.insert(format!("{stem}.{ext}"));
            }
        }
        // A material path names the model's own folder.
        //
        // `/vehicle/trailer_eu/doosung/box/materials/x.mat` puts the model in
        // `.../box/`, and SCS names it after that folder often enough to be
        // worth the handful of hashes it costs to check. This is the only route
        // to a name for a model in an archive that ships no directory listing:
        // nothing else in the file refers to it.
        if let Some(dir) = t.strip_suffix(&t[t.rfind('/').map(|i| i + 1).unwrap_or(0)..]) {
            let dir = dir.trim_end_matches('/');
            if let Some(parent) = dir.strip_suffix("/materials") {
                let own = parent.rsplit('/').next().unwrap_or("");
                let up = parent.rsplit('/').nth(1).unwrap_or("");
                for stem in [own, up, "model", "body"] {
                    if stem.is_empty() {
                        continue;
                    }
                    for ext in SIBLINGS {
                        out.insert(format!("{parent}/{stem}.{ext}"));
                    }
                }
            }
        }

        // `@include "units/foo.sui"` and friends are relative to the file that
        // contains them, not to the archive root
        if !owner_dir.is_empty() && !token.starts_with('/') {
            let joined = format!("{owner_dir}/{t}");
            out.insert(joined.clone());
            if let Some((stem, _)) = joined.rsplit_once('.') {
                for ext in SIBLINGS {
                    out.insert(format!("{stem}.{ext}"));
                }
            }
        }
    };

    let mut cur = Vec::<u8>::new();
    for &b in data.iter().chain(std::iter::once(&0u8)) {
        let ok = b == b'/' || b == b'.' || b == b'_' || b == b'-' || b.is_ascii_alphanumeric();
        if ok {
            cur.push(b);
            continue;
        }
        if cur.len() > 4 {
            if let Ok(s) = std::str::from_utf8(&cur) {
                if s.contains('.') && (s.contains('/') || !owner_dir.is_empty()) {
                    emit(s);
                }
            }
        }
        cur.clear();
    }

    // `icon: "g7_000"` in a .sii names a UI icon, not a path; the file lives in a
    // fixed location. Without this rule those textures are only recoverable by hash.
    for m in find_quoted_after(data, b"icon:") {
        for ext in ["mat", "tobj", "dds"] {
            out.insert(format!("material/ui/accessory/{m}.{ext}"));
            out.insert(format!("material/ui/{m}.{ext}"));
        }
    }
}

/// Values of `key: "value"` pairs in SCS text formats.
fn find_quoted_after(data: &[u8], key: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(pos) = find(&data[i..], key) {
        let mut j = i + pos + key.len();
        while j < data.len() && (data[j] == b' ' || data[j] == b'\t') {
            j += 1;
        }
        if j < data.len() && data[j] == b'"' {
            j += 1;
            let start = j;
            while j < data.len() && data[j] != b'"' && data[j] != b'\n' {
                j += 1;
            }
            if j < data.len() && data[j] == b'"' {
                if let Ok(s) = std::str::from_utf8(&data[start..j]) {
                    if !s.is_empty() && s.len() < 128 {
                        out.push(s.to_ascii_lowercase());
                    }
                }
            }
        }
        i += pos + key.len();
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Name a blob the deep scan could not find a path for, by what it contains.
///
/// This matters more than it looks. A mod whose definitions are obfuscated
/// yields no path strings to harvest, so its real content ends up here - the
/// BMW F30 car mod hides a 14.7 MB body model and every one of its engine
/// samples this way. Calling those `.bin` loses them twice over: the sound pass
/// skips them because it matches on extension, and nothing downstream can tell
/// a model from a texture.
fn sniff_extension(data: &[u8]) -> &'static str {
    match data {
        [0x44, 0x44, 0x53, 0x20, ..] => ".dds",
        [0xFF, 0xD8, 0xFF, ..] => ".jpg",
        [0x89, b'P', b'N', b'G', ..] => ".png",
        // SCS model formats put a version byte first, then the tag backwards
        [_, b'g', b'm', b'P', ..] => ".pmg",
        [_, b'd', b'm', b'P', ..] => ".pmd",
        [_, b'c', b'm', b'P', ..] => ".pmc",
        [_, b'a', b'm', b'P', ..] => ".pma",
        [_, b's', b'm', b'P', ..] => ".pms",
        [_, b'p', b'm', b'P', ..] => ".ppd",
        [b'O', b'g', b'g', b'S', ..] => ".ogg",
        // RIFF is a container, not a format: only the WAVE flavour is audio, and
        // calling the others .wav just sent ffmpeg chasing files it cannot open
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'A', b'V', b'E', ..] => ".wav",
        [b'R', b'I', b'F', b'X', ..] | [b'F', b'S', b'B', b'5', ..] => ".bank",
        [b'B', b'S', b'I', b'I', ..] => ".sii",
        [b'S', b'c', b's', b'C', ..] => ".sii",
        [0x33, b'n', b'K', ..] => ".sii",
        _ => {
            let head = &data[..data.len().min(64)];
            if head.starts_with(b"SiiNunit") {
                ".sii"
            } else if head.starts_with(b"material") {
                ".mat"
            } else if head.starts_with(b"map ") || head.starts_with(b"\x01\x0a") {
                ".tobj"
            } else {
                ".bin"
            }
        }
    }
}


