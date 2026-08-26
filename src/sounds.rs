//! Pulling a mod's audio out alongside the model.
//!
//! Not every vehicle ships sound, and of those that do, not all of it can be
//! converted. Measured across the test set:
//!
//! * `.ogg` - plain Vorbis, one file per sample. These convert.
//! * `.bank` - an FMOD Studio bank, a container holding the engine and interior
//!   sets. Reading one needs FMOD's own tooling, so it is copied through
//!   untouched rather than silently dropped.
//! * `.soundref` - a text pointer to a sound elsewhere, not audio itself.
//!
//! MP3 encoding is done by ffmpeg when it is on the machine. It is not bundled:
//! it is an order of magnitude larger than this whole program, and the `.ogg`
//! files are perfectly usable as they are. Without ffmpeg the audio still comes
//! out, just in its original format.

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Report {
    pub converted: usize,
    pub copied: usize,
    pub banks: usize,
    /// samples decoded out of FMOD banks
    pub bank_samples: usize,
    pub encoder: Option<PathBuf>,
    pub bank_decoder: Option<PathBuf>,
}

/// ffmpeg from the `FFMPEG` variable, next to this program, or on PATH.
///
/// PATH is walked here rather than handed to `where`: shelling out for the
/// lookup opens a console window of its own, which is exactly the flicker this
/// program goes to some length to avoid everywhere else.
pub fn find_ffmpeg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FFMPEG").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let p = d.join("ffmpeg.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["ffmpeg.exe", "ffmpeg"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// vgmstream-cli, from the `VGMSTREAM` variable, a `vgmstream/` folder next to
/// this program, next to it directly, or on PATH.
///
/// FMOD banks carry their samples as FSB5, and FSB5 Vorbis is Vorbis with the
/// Ogg container and the setup header stripped - only a CRC of the codebook is
/// kept. ffmpeg refuses it outright ("version 5 is not implemented");
/// vgmstream carries the table of FMOD codebooks and decodes it directly.
pub fn find_vgmstream() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VGMSTREAM").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            for rel in ["vgmstream/vgmstream-cli.exe", "vgmstream-cli.exe", "tools/vgmstream-cli.exe"] {
                let p = d.join(rel);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["vgmstream-cli.exe", "vgmstream-cli"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Decode every sample in an FMOD bank into `out_dir`, one MP3 (or WAV when
/// there is no encoder) per sample, named by the sample's own name.
///
/// Names repeat inside a bank: the same sound exists as an interior and an
/// exterior version and both carry the event name. Those are different audio,
/// so a repeated name gets a numeric suffix rather than overwriting.
fn decode_bank(
    bank: &Path,
    out_dir: &Path,
    vgm: &Path,
    encoder: Option<&Path>,
) -> std::io::Result<usize> {
    let tmp = out_dir.join("_wav");
    std::fs::create_dir_all(&tmp)?;

    let mut cmd = Command::new(vgm);
    crate::pipeline::no_window(&mut cmd);
    let pattern = tmp.join("?s_?n.wav");
    let ok = cmd
        .arg("-S")
        .arg("0")
        .arg("-o")
        .arg(&pattern)
        .arg(bank)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_dir_all(&tmp);
        return Ok(0);
    }

    // group by sample name; the subsong number is only kept on a collision
    let mut by_name: std::collections::BTreeMap<String, Vec<PathBuf>> = Default::default();
    for e in std::fs::read_dir(&tmp)?.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("wav") {
            continue;
        }
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let name = stem.split_once('_').map(|(_, n)| n.to_string()).unwrap_or(stem);
        by_name.entry(name).or_default().push(p);
    }

    let mut written = 0usize;
    for (name, mut wavs) in by_name {
        wavs.sort();
        let many = wavs.len() > 1;
        for (i, wav) in wavs.iter().enumerate() {
            let base = if many { format!("{name}_{}", i + 1) } else { name.clone() };
            let safe: String = base
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' })
                .collect();
            let dst_mp3 = out_dir.join(format!("{safe}.mp3"));
            let encoded = match encoder {
                Some(ff) => {
                    let mut c = Command::new(ff);
                    crate::pipeline::no_window(&mut c);
                    c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
                        .arg(wav)
                        .args(["-codec:a", "libmp3lame", "-q:a", "2"])
                        .arg(&dst_mp3)
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
                }
                None => false,
            };
            if encoded {
                written += 1;
            } else {
                let dst_wav = out_dir.join(format!("{safe}.wav"));
                if std::fs::rename(wav, &dst_wav).is_ok() || std::fs::copy(wav, &dst_wav).is_ok() {
                    written += 1;
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(written)
}

fn audio_files(extracted: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![extracted.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if matches!(
                p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(),
                Some("ogg") | Some("wav") | Some("mp3") | Some("bank")
            ) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Keep the archive's folder structure in the name so `engine/start.ogg` and
/// `interior/start.ogg` do not collide.
fn flat_name(extracted: &Path, p: &Path) -> String {
    p.strip_prefix(extracted)
        .unwrap_or(p)
        .to_string_lossy()
        .replace(['/', '\\'], "_")
}

/// The file's own place in the mod, kept as folders.
///
/// A mod already sorts its audio sensibly - `sound/truck/volvo/750/ext/` - so
/// mirroring that beats inventing a scheme. The leading `sound/` carries no
/// information once everything under the folder is audio, so it goes.
fn tree_path(extracted: &Path, p: &Path) -> PathBuf {
    let rel = p.strip_prefix(extracted).unwrap_or(p);
    let s = rel.to_string_lossy().replace('\\', "/");
    let trimmed = s.strip_prefix("sound/").unwrap_or(&s);
    PathBuf::from(trimmed)
}

/// Pull the audio out of one extracted mod into `dest`, keeping its folders and
/// converting what can be converted. FMOD banks are set aside rather than mixed
/// in with the playable files.
pub fn extract_tree(
    extracted: &Path,
    dest: &Path,
    encoder: Option<&Path>,
    mut progress: impl FnMut(String),
) -> std::io::Result<Report> {
    let files = audio_files(extracted);
    let vgm = find_vgmstream();
    let mut rep = Report {
        converted: 0,
        copied: 0,
        banks: 0,
        bank_samples: 0,
        encoder: encoder.map(|p| p.to_path_buf()),
        bank_decoder: vgm.clone(),
    };
    if files.is_empty() {
        return Ok(rep);
    }

    for (i, src) in files.iter().enumerate() {
        let ext = src
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let rel = tree_path(extracted, src);

        if ext == "bank" {
            rep.banks += 1;
            // A bank becomes a folder of its samples, named after the bank,
            // sitting where the bank sat: sound/bus/1836rs.bank -> bus/1836rs/
            if let Some(v) = &vgm {
                let folder = dest.join(rel.with_extension(""));
                std::fs::create_dir_all(&folder)?;
                let n = decode_bank(src, &folder, v, encoder)?;
                if n == 0 {
                    // an event-only master bank, or one that would not decode
                    let _ = std::fs::remove_dir(&folder);
                }
                rep.bank_samples += n;
            } else {
                let out = dest.join("_fmod_banks").join(rel.file_name().unwrap_or(rel.as_os_str()));
                if let Some(p) = out.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let _ = std::fs::copy(src, &out);
            }
            continue;
        }

        let out = if encoder.is_some() && ext != "mp3" {
            dest.join(&rel).with_extension("mp3")
        } else {
            dest.join(&rel)
        };
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let Some(ff) = encoder else {
            let _ = std::fs::copy(src, &out);
            rep.copied += 1;
            continue;
        };
        if ext == "mp3" {
            let _ = std::fs::copy(src, &out);
            rep.copied += 1;
            continue;
        }

        let mut cmd = Command::new(ff);
        crate::pipeline::no_window(&mut cmd);
        let ok = cmd
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(src)
            .args(["-codec:a", "libmp3lame", "-q:a", "2"])
            .arg(&out)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            rep.converted += 1;
        } else {
            // an encode that fails must not lose the sample
            let _ = std::fs::copy(src, dest.join(&rel));
            rep.copied += 1;
        }
        if i % 50 == 0 {
            progress(format!("{}/{}", i + 1, files.len()));
        }
    }
    Ok(rep)
}

pub fn extract(
    extracted: &Path,
    outdir: &Path,
    mut progress: impl FnMut(String),
) -> std::io::Result<Report> {
    let files = audio_files(extracted);
    let mut rep = Report {
        converted: 0,
        copied: 0,
        banks: 0,
        bank_samples: 0,
        encoder: find_ffmpeg(),
        bank_decoder: None,
    };
    if files.is_empty() {
        return Ok(rep);
    }

    let dir = outdir.join("sounds");
    std::fs::create_dir_all(&dir)?;

    for (i, src) in files.iter().enumerate() {
        let ext = src
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let base = flat_name(extracted, src);

        if ext == "bank" {
            let _ = std::fs::copy(src, dir.join(&base));
            rep.banks += 1;
            continue;
        }

        // already mp3, or no encoder to hand: pass it through
        let can_encode = rep.encoder.is_some() && ext != "mp3";
        if !can_encode {
            let _ = std::fs::copy(src, dir.join(&base));
            rep.copied += 1;
            continue;
        }

        let dst = dir.join(format!("{}.mp3", base.trim_end_matches(&format!(".{ext}"))));
        let mut cmd = Command::new(rep.encoder.as_ref().unwrap());
        crate::pipeline::no_window(&mut cmd);
        let ok = cmd
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(src)
            .args(["-codec:a", "libmp3lame", "-q:a", "2"])
            .arg(&dst)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            rep.converted += 1;
        } else {
            // an encode that fails must not lose the sample
            let _ = std::fs::copy(src, dir.join(&base));
            rep.copied += 1;
        }

        if i % 25 == 0 {
            progress(format!("sound {}/{}", i + 1, files.len()));
        }
    }
    Ok(rep)
}
