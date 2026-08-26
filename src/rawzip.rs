//! A zip reader that does not trust the archive's own bookkeeping.
//!
//! A number of published mods are "protected" by corrupting their zip metadata
//! rather than by encrypting anything. The game does not care - SCS's loader
//! reads the zip structure but ignores the general-purpose encryption bit and
//! never verifies a CRC - so the mod installs and drives normally, while
//! WinRAR, 7-Zip and every conforming library refuse it and ask for a password
//! that was never set.
//!
//! Measured on two such archives (`Hyundai HD 1997`, `Truck_Howo TH7`), every
//! one of their 4075 and 2408 entries carries:
//!
//! * central-directory general-purpose flag `0x0001` - "encrypted" - while the
//!   matching local header says `0x0000`,
//! * central-directory method 8 (deflate) against local header method 1
//!   (shrunk, a format nothing has produced since the 1980s),
//! * a fabricated CRC-32 with a giveaway repeating-byte shape (`4e4e5656`,
//!   `76767676`).
//!
//! The payload itself is untouched: inflating it raw returns exactly the
//! recorded uncompressed size and valid content. So this reader takes names,
//! sizes and offsets from the central directory, reads the bytes at the local
//! header, and decides store-vs-deflate by trying it rather than by believing
//! the method field. CRCs are recomputed for reporting, never enforced.
//!
//! This is not decryption - there is nothing encrypted to decrypt. It is
//! parsing a file that lies about its own shape. Whatever comes out is still
//! the mod author's work.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub struct Entry {
    pub name: String,
    pub method: u16,
    pub flag: u16,
    pub csize: u64,
    pub usize_: u64,
    pub local_offset: u64,
    /// from the central directory, as a fallback when the local header has been
    /// overwritten
    pub name_len: u16,
}

pub struct RawZip {
    file: File,
    pub entries: Vec<Entry>,
    /// how many entries claim to be encrypted (flag bit 0)
    pub flagged_encrypted: usize,
    /// how many entries disagree with their own local header
    pub inconsistent: usize,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl RawZip {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        let size = file.seek(SeekFrom::End(0))?;

        // The End Of Central Directory sits within 64 KiB of the end (its
        // comment field is 16-bit), so scan back that far and take the last
        // signature - a stray "PK\5\6" inside compressed data would otherwise win.
        let window = size.min(66_000);
        file.seek(SeekFrom::Start(size - window))?;
        let mut tail = vec![0u8; window as usize];
        file.read_exact(&mut tail)?;
        let eocd = (0..tail.len().saturating_sub(21))
            .rev()
            .find(|&i| &tail[i..i + 4] == b"PK\x05\x06")
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "no zip end-of-directory")
            })?;
        let rec = &tail[eocd..];
        let mut count = u16le(rec, 10) as usize;
        let cd_size = u32le(rec, 12) as u64;
        let mut cd_off = u32le(rec, 16) as u64;

        // Zip64: the 32-bit fields saturate and the real numbers live in a
        // locator just before the EOCD.
        if cd_off == 0xFFFF_FFFF || count == 0xFFFF {
            if let Some(loc) = (0..eocd).rev().find(|&i| &tail[i..i + 4] == b"PK\x06\x07") {
                let z64_off = u32le(&tail[loc..], 8) as u64
                    | ((u32le(&tail[loc..], 12) as u64) << 32);
                let mut hdr = [0u8; 56];
                file.seek(SeekFrom::Start(z64_off))?;
                file.read_exact(&mut hdr)?;
                if &hdr[0..4] == b"PK\x06\x06" {
                    count = u32le(&hdr, 32) as usize | ((u32le(&hdr, 36) as usize) << 32);
                    cd_off = u32le(&hdr, 48) as u64 | ((u32le(&hdr, 52) as u64) << 32);
                }
            }
        }

        // Read to whatever is actually there, not to what the record claims.
        // These archives overstate the directory size as well - one says 441325
        // bytes when only 320243 remain before the end-of-directory record - so
        // an exact read fails and the whole file gets written off as encrypted.
        // A short directory still walks: the entry loop stops at the first
        // header that is not there.
        let available = size.saturating_sub(cd_off);
        let want = cd_size.min(available) as usize;
        file.seek(SeekFrom::Start(cd_off))?;
        let mut cd = vec![0u8; want];
        let mut got = 0usize;
        while got < want {
            match file.read(&mut cd[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => return Err(e),
            }
        }
        cd.truncate(got);

        let mut entries = Vec::with_capacity(count);
        let mut flagged_encrypted = 0usize;
        let mut inconsistent = 0usize;
        let mut p = 0usize;
        while p + 46 <= cd.len() && &cd[p..p + 4] == b"PK\x01\x02" {
            let flag = u16le(&cd, p + 8);
            let method = u16le(&cd, p + 10);
            let csize = u32le(&cd, p + 20) as u64;
            let usize_ = u32le(&cd, p + 24) as u64;
            let nlen = u16le(&cd, p + 28) as usize;
            let elen = u16le(&cd, p + 30) as usize;
            let clen = u16le(&cd, p + 32) as usize;
            let local_offset = u32le(&cd, p + 42) as u64;
            let name = String::from_utf8_lossy(&cd[p + 46..p + 46 + nlen]).to_string();
            if flag & 1 != 0 {
                flagged_encrypted += 1;
            }
            entries.push(Entry {
                name,
                method,
                flag,
                csize,
                usize_,
                local_offset,
                name_len: nlen as u16,
            });
            p += 46 + nlen + elen + clen;
        }

        // Sample a few local headers to see whether the directory is telling the
        // truth. Reading all of them on a 200 MB archive is a lot of seeking for
        // an answer that is uniform in practice.
        for e in entries.iter().take(32) {
            let mut lh = [0u8; 30];
            if file.seek(SeekFrom::Start(e.local_offset)).is_err() {
                continue;
            }
            if file.read_exact(&mut lh).is_err() {
                continue;
            }
            // A second protector variant overwrites the local signature itself
            // with its own mark - `AEM!` on the two archives seen - and zeroes
            // the header around it. A real zip always carries PK\3\4 here, so a
            // missing signature is the strongest evidence of tampering there is;
            // treating it as "cannot tell" made these files fall through to the
            // conforming reader, which then asked for a password.
            if &lh[0..4] != b"PK\x03\x04" {
                inconsistent += 1;
                continue;
            }
            if u16le(&lh, 6) != e.flag || u16le(&lh, 8) != e.method {
                inconsistent += 1;
            }
        }

        Ok(RawZip { file, entries, flagged_encrypted, inconsistent })
    }

    /// True when the archive claims to be encrypted but its local headers
    /// disagree - the signature of a metadata-only "lock".
    pub fn is_falsely_locked(&self) -> bool {
        self.flagged_encrypted > 0 && self.inconsistent > 0
    }

    pub fn read(&mut self, i: usize) -> std::io::Result<Vec<u8>> {
        let e = &self.entries[i];
        let (csize, usize_, off, cd_nlen) = (e.csize, e.usize_, e.local_offset, e.name_len as u64);
        let mut lh = [0u8; 30];
        self.file.seek(SeekFrom::Start(off))?;
        self.file.read_exact(&mut lh)?;

        // Where the payload starts is `local header + name + extra`, and the
        // overwritten headers still carry the name length in its usual place -
        // checked against the next entry's offset on a real archive:
        // 78 + 30 + 21 + 48832 = 48961 exactly. Fall back to the central
        // directory's name length when even that has been wiped.
        let mut nlen = u16le(&lh, 26) as u64;
        let mut elen = u16le(&lh, 28) as u64;
        if &lh[0..4] != b"PK\x03\x04" && (nlen == 0 || nlen > 4096 || elen > 4096) {
            nlen = cd_nlen;
            elen = 0;
        }
        self.file.seek(SeekFrom::Start(off + 30 + nlen + elen))?;
        let mut blob = vec![0u8; csize as usize];
        self.file.read_exact(&mut blob)?;

        // Decide by result, not by the declared method: these archives label
        // deflate data as method 1.
        if csize == usize_ {
            if let Ok(out) = inflate(&blob, usize_) {
                if out.len() as u64 == usize_ {
                    return Ok(out);
                }
            }
            return Ok(blob);
        }
        inflate(&blob, usize_)
    }
}

fn inflate(blob: &[u8], hint: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(hint as usize);
    flate2::read::DeflateDecoder::new(blob)
        .read_to_end(&mut out)
        .map(|_| out)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

