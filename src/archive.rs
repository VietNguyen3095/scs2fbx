//! One interface over the two container formats a `.scs` file can be.
//!
//! Most mods ship HashFS ("SCS#"). Some ship an ordinary zip instead - and a few
//! of those carry a small header before the zip data (seen in the wild: `AEM!`
//! followed by zero padding). Because a zip is located from its End Of Central
//! Directory at the *end* of the file, a prefix like that is harmless as long as
//! the reader tolerates it, so the magic at offset 0 is not a reliable test.
//! We try HashFS first and fall back to zip.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::hashfs::{HashFs, ScsError};

pub enum Archive {
    HashFs(Box<HashFs>),
    Zip(Box<zip::ZipArchive<File>>),
    /// a zip whose metadata claims encryption it does not have
    RawZip(Box<crate::rawzip::RawZip>),
}

type Result<T> = std::result::Result<T, ScsError>;

impl Archive {
    pub fn open(path: &Path) -> Result<Self> {
        let mut f = File::open(path).map_err(ScsError::Io)?;
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic).map_err(ScsError::Io)?;
        if &magic == b"SCS#" {
            return HashFs::open(path).map(|h| Archive::HashFs(Box::new(h)));
        }
        // A mod "locked" by flipping the encryption bit has to be caught before
        // the conforming reader, because that reader will believe the bit and
        // demand a password nobody ever set. Only take this path when the local
        // headers actually contradict the directory - a genuinely encrypted zip
        // is consistent with itself, and must still be refused.
        if let Ok(raw) = crate::rawzip::RawZip::open(path) {
            if raw.is_falsely_locked() {
                return Ok(Archive::RawZip(Box::new(raw)));
            }
        }
        f.seek(SeekFrom::Start(0)).map_err(ScsError::Io)?;
        match zip::ZipArchive::new(f) {
            Ok(z) => Ok(Archive::Zip(Box::new(z))),
            Err(e) => {
                // report the HashFS problem if it at least looked like one
                if &magic == b"SCS#" {
                    Err(ScsError::NotHashFs)
                } else {
                    Err(ScsError::Inflate(format!(
                        "not a HashFS archive and not readable as a zip: {e}"
                    )))
                }
            }
        }
    }

    pub fn kind(&self) -> String {
        match self {
            Archive::HashFs(h) => format!("HashFS v{}", h.version),
            Archive::Zip(_) => "zip".to_string(),
            Archive::RawZip(r) => format!(
                "zip with a fake encryption flag on {} entries",
                r.flagged_encrypted
            ),
        }
    }

    pub fn entry_count(&self) -> usize {
        match self {
            Archive::HashFs(h) => h.entries.len(),
            Archive::Zip(z) => z.len(),
            Archive::RawZip(r) => r.entries.len(),
        }
    }

    /// Returns (named, recovered_by_deep_scan, written_under_their_hash).
    /// A zip stores real names, so the last two are always zero.
    pub fn extract_all(
        &mut self,
        dest: &Path,
        deep: bool,
        mut progress: impl FnMut(&str, f32),
    ) -> Result<(usize, usize, usize)> {
        match self {
            Archive::HashFs(h) => h.extract_all(dest, deep, progress),
            Archive::Zip(z) => {
                let total = z.len().max(1);
                let mut written = 0usize;
                for i in 0..z.len() {
                    let mut e = match z.by_index(i) {
                        Ok(e) => e,
                        Err(zip::result::ZipError::UnsupportedArchive(m))
                            if m.contains("Password") =>
                        {
                            return Err(ScsError::Encrypted)
                        }
                        Err(e) => return Err(ScsError::Inflate(e.to_string())),
                    };
                    let name = e.name().replace('\\', "/");
                    let mut out = PathBuf::from(dest);
                    for part in name.split('/') {
                        if part.is_empty() || part == "." || part == ".." {
                            continue;
                        }
                        out.push(part);
                    }
                    if name.ends_with('/') || e.is_dir() {
                        std::fs::create_dir_all(&out).map_err(ScsError::Io)?;
                        continue;
                    }
                    if let Some(p) = out.parent() {
                        std::fs::create_dir_all(p).map_err(ScsError::Io)?;
                    }
                    let mut buf = Vec::with_capacity(e.size() as usize);
                    e.read_to_end(&mut buf).map_err(ScsError::Io)?;
                    std::fs::write(&out, buf).map_err(ScsError::Io)?;
                    written += 1;
                    if i % 64 == 0 {
                        progress(&format!("extracting {name}"), 0.5 * (i as f32 / total as f32));
                    }
                }
                progress("extraction done", 0.5);
                Ok((written, 0, 0))
            }
            Archive::RawZip(r) => {
                let total = r.entries.len().max(1);
                let mut written = 0usize;
                let mut failed = 0usize;
                for i in 0..r.entries.len() {
                    let name = r.entries[i].name.replace('\\', "/");
                    let mut out = PathBuf::from(dest);
                    for part in name.split('/') {
                        if part.is_empty() || part == "." || part == ".." {
                            continue;
                        }
                        out.push(part);
                    }
                    if name.ends_with('/') {
                        std::fs::create_dir_all(&out).map_err(ScsError::Io)?;
                        continue;
                    }
                    if let Some(p) = out.parent() {
                        std::fs::create_dir_all(p).map_err(ScsError::Io)?;
                    }
                    // One unreadable entry should not lose the other four
                    // thousand: this container is deliberately malformed, so
                    // treat per-entry failure as expected and keep going.
                    match r.read(i) {
                        Ok(buf) => {
                            std::fs::write(&out, buf).map_err(ScsError::Io)?;
                            written += 1;
                        }
                        Err(_) => failed += 1,
                    }
                    if i % 64 == 0 {
                        progress(&format!("extracting {name}"), 0.5 * (i as f32 / total as f32));
                    }
                }
                if failed > 0 {
                    progress(&format!("extraction done, {failed} entries unreadable"), 0.5);
                } else {
                    progress("extraction done", 0.5);
                }
                Ok((written, 0, 0))
            }
        }
    }
}

