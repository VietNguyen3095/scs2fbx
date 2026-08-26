#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod archive;
mod cityhash;
mod assemble;
mod fbx;
mod scene;
mod pim;
mod pit;
mod pmd;
mod pmg;
mod fbxscene;
mod hashfs;
mod pipeline;
mod rawzip;
mod sii;
mod sii3nk;
mod sounds;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    // A tiny CLI so the pipeline can be scripted and tested without the GUI.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "extract" {
        if args.len() < 4 {
            eprintln!("usage: scs2fbx extract <archive.scs> <outdir>");
            std::process::exit(2);
        }
        let mut fs = match archive::Archive::open(&PathBuf::from(&args[2])) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        println!("{}  {} entries", fs.kind(), fs.entry_count());
        match fs.extract_all(&PathBuf::from(&args[3]), true, |m, _| {
            if m.starts_with("deep scan") || m.starts_with("extraction") {
                println!("  {m}");
            }
        }) {
            Ok((w, r, d)) => println!("named {w} + deep {r}, {d} written under their hash"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // Build one sound library out of a folder of mods.
    //
    // A vehicle whose audio cannot be converted contributes nothing, so it is
    // left out entirely rather than leaving an empty folder behind - most mods
    // ship only FMOD banks, which need FMOD's own tools to open.
    if args.len() > 3 && args[1] == "soundlib" {
        let src = PathBuf::from(&args[2]);
        let dest = PathBuf::from(&args[3]);
        // `--reuse <dir>`: a folder of earlier conversions to take extractions from
        let reuse: Option<PathBuf> = args
            .iter()
            .position(|a| a == "--reuse")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from);
        let ff = sounds::find_ffmpeg();
        match &ff {
            Some(p) => println!("encoding with {}", p.display()),
            None => println!("ffmpeg not found - audio will be copied in its original format"),
        }
        match sounds::find_vgmstream() {
            Some(p) => println!("decoding FMOD banks with {}", p.display()),
            None => println!("vgmstream not found - FMOD banks will be copied undecoded"),
        }

        let mut archives: Vec<PathBuf> = std::fs::read_dir(&src)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("scs"))
                    .collect()
            })
            .unwrap_or_default();
        archives.sort();
        println!("{} archive(s)", archives.len());

        let (mut with, mut without, mut mp3, mut banks) = (0usize, 0usize, 0usize, 0usize);
        let mut bank_only: Vec<String> = Vec::new();
        for a in &archives {
            let stem = a.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            // Reuse an extraction from a previous conversion when one is
            // offered, since unpacking a 500 MB archive again to read its audio
            // is the slowest part of this by far.
            let prior = reuse.as_ref().map(|r: &PathBuf| {
                r.join(stem.replace(
                    |c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_',
                    "_",
                ))
                .join("_work")
                .join("extracted")
            });
            let work = dest.join("_work").join(&stem);
            let extracted = match prior {
                Some(p) if p.is_dir() => p,
                _ => work.clone(),
            };
            if !extracted.is_dir() {
                let Ok(mut fs) = archive::Archive::open(a) else {
                    println!("  {stem}: cannot open");
                    continue;
                };
                if fs.extract_all(&extracted, true, |_, _| {}).is_err() {
                    println!("  {stem}: cannot extract");
                    continue;
                }
            }

            // A clean name for the folder: drop the version brackets mods carry.
            let mut name = String::new();
            let mut depth = 0i32;
            for c in stem.chars() {
                match c {
                    '[' | '(' => depth += 1,
                    ']' | ')' => depth = (depth - 1).max(0),
                    _ if depth == 0 => name.push(c),
                    _ => {}
                }
            }
            let name = name.trim().trim_end_matches(['-', '_', '.']).trim().to_string();
            let name = if name.is_empty() { stem.clone() } else { name };

            let out = dest.join(&name);
            match sounds::extract_tree(&extracted, &out, ff.as_deref(), |_| {}) {
                Ok(r) if r.converted + r.copied + r.bank_samples == 0 => {
                    // nothing playable: take the folder back out again
                    let _ = std::fs::remove_dir_all(&out);
                    without += 1;
                    if r.banks > 0 {
                        bank_only.push(format!("{name} ({} bank)", r.banks));
                    }
                }
                Ok(r) => {
                    with += 1;
                    mp3 += r.converted + r.bank_samples;
                    banks += r.banks;
                    println!(
                        "  {name}: {} loose + {} from {} bank(s)",
                        r.converted, r.bank_samples, r.banks
                    );
                }
                Err(e) => println!("  {name}: {e}"),
            }
        }
        let _ = std::fs::remove_dir_all(dest.join("_work"));
        println!("\n{with} vehicle(s) with sound -> {}", dest.display());
        println!("{mp3} mp3 in total, {banks} FMOD bank(s) read");
        if !bank_only.is_empty() {
            println!(
                "{} skipped, FMOD banks only and no vgmstream to open them: {}",
                bank_only.len(),
                bank_only.join(", ")
            );
        }
        println!("{without} skipped in total");
        return Ok(());
    }

    // Audio on its own, without converting the vehicle. Useful for an archive
    // that has no model to build - a trailer pack, an addon - and for pulling
    // the sound out of something already converted without redoing the geometry.
    if args.len() > 3 && args[1] == "sounds" {
        let archive = PathBuf::from(&args[2]);
        let outdir = PathBuf::from(&args[3]);
        let extracted = outdir.join("_work").join("extracted");
        if !extracted.join("def").is_dir() && !extracted.is_dir() {
            let mut fs = match archive::Archive::open(&archive) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            println!("{}  {} entries", fs.kind(), fs.entry_count());
            if let Err(e) = fs.extract_all(&extracted, true, |_, _| {}) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        match sounds::extract(&extracted, &outdir, |m| println!("   {m}")) {
            Ok(r) if r.converted + r.copied + r.banks == 0 => println!("no sound in this archive"),
            Ok(r) => {
                if r.encoder.is_none() && r.copied > 0 {
                    eprintln!("   ! ffmpeg not found - audio copied in its original format");
                }
                println!(
                    "{} to MP3, {} copied as-is, {} FMOD bank(s) -> {}",
                    r.converted,
                    r.copied,
                    r.banks,
                    outdir.join("sounds").display()
                );
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // Read a .pmg with the built-in reader and print what it found, so the
    // result can be held against the .pim ConverterPIX makes from the same file.
    if args.len() > 2 && args[1] == "pmginfo" {
        match pmg::parse(&PathBuf::from(&args[2])) {
            Ok(m) => {
                let verts: usize = m.pieces.iter().map(|p| p.positions.len()).sum();
                let tris: usize = m.pieces.iter().map(|p| p.tris.len()).sum();
                println!(
                    "pieces={} verts={} tris={} parts={} locators={}",
                    m.pieces.len(), verts, tris, m.parts.len(), m.locators.len()
                );
                for p in m.parts.iter().take(6) {
                    println!("  part   {:?} {} pieces", p.name, p.pieces.len());
                }
                for l in m.locators.iter().take(6) {
                    println!(
                        "  loc    {:<16} ({:7.3},{:7.3},{:7.3})",
                        l.name, l.position[0], l.position[1], l.position[2]
                    );
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // Every model in an archive, in one FBX, laid out in rows, with textures.
    //
    // For a trailer pack or any archive whose definitions did not survive:
    // there is nothing to assemble, but the geometry is all there. A model
    // needs its `.pmd` descriptor for material names, and in an archive with no
    // directory listing both files are nameless - so they are paired by their
    // position in the archive, which is where the packer wrote them together.
    // The (piece, part) counts each file records independently are the check
    // that the pairing is right: across one 228-model pack, all 228 agree.
    if args.len() > 3 && args[1] == "models" {
        let archive = PathBuf::from(&args[2]);
        let outdir = PathBuf::from(&args[3]);
        let mut fs = match hashfs::HashFs::open(&archive) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };

        let all: Vec<hashfs::Entry> = fs.entries.values().filter(|e| !e.is_dir()).copied().collect();
        let mut models: Vec<(u64, crate::pim::Model)> = Vec::new();
        let mut descs: Vec<(u64, pmd::Pmd)> = Vec::new();
        for e in &all {
            let Ok(data) = fs.read_entry(e) else { continue };
            if data.len() > 4 && &data[1..4] == b"gmP" {
                if let Ok(m) = pmg::parse_bytes(&data) {
                    models.push((e.offset, m));
                }
            } else if let Ok(d) = pmd::parse_bytes(&data) {
                descs.push((e.offset, d));
            }
        }
        descs.sort_by_key(|d| d.0);
        models.sort_by_key(|m| m.0);
        println!("{} model(s), {} descriptor(s)", models.len(), descs.len());

        struct Loaded {
            name: String,
            pieces: Vec<crate::pim::Piece>,
            mats: Vec<String>,
            lo: [f32; 3],
            hi: [f32; 3],
        }
        let mut loaded: Vec<Loaded> = Vec::new();
        let mut seen: std::collections::HashSet<u64> = Default::default();
        let (mut dupes, mut skipped, mut paired, mut unpaired) = (0usize, 0usize, 0usize, 0usize);

        for (off, m) in &models {
            let pieces: Vec<crate::pim::Piece> = m
                .pieces
                .iter()
                .filter(|pc| !pc.positions.is_empty() && !pc.tris.is_empty())
                .cloned()
                .collect();
            if pieces.is_empty() {
                skipped += 1;
                continue;
            }

            // A pack often ships the same model more than once.
            let mut h: u64 = 0xcbf29ce484222325;
            for pc in &pieces {
                for v in &pc.positions {
                    for c in v {
                        h ^= (c * 1000.0).round() as i64 as u64;
                        h = h.wrapping_mul(0x100000001b3);
                    }
                }
            }
            if !seen.insert(h) {
                dupes += 1;
                continue;
            }

            let mats = descs
                .iter()
                .min_by_key(|d| (d.0 as i64 - *off as i64).abs())
                .filter(|d| d.1.piece_count == m.pieces.len() && d.1.part_count == m.parts.len())
                .map(|d| {
                    paired += 1;
                    d.1.materials.clone()
                })
                .unwrap_or_else(|| {
                    unpaired += 1;
                    Vec::new()
                });

            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            for pc in &pieces {
                for v in &pc.positions {
                    for i in 0..3 {
                        lo[i] = lo[i].min(v[i]);
                        hi[i] = hi[i].max(v[i]);
                    }
                }
            }
            let name = m
                .parts
                .first()
                .map(|p| p.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("model_{off:x}"));
            loaded.push(Loaded { name, pieces, mats, lo, hi });
        }
        if loaded.is_empty() {
            println!("nothing readable in this archive");
            return Ok(());
        }
        println!("{paired} paired with a descriptor, {unpaired} without");

        // A .mat is text and names a .tobj; a .tobj is a small binary whose tail
        // is the path of the image it wraps. Both are read straight out of the
        // archive by path, so none of this needs the extraction on disk.
        let tex_dir = outdir.join("textures");
        let _ = std::fs::create_dir_all(&tex_dir);
        let mut mat_index: std::collections::HashMap<String, usize> = Default::default();
        let mut mats_out: Vec<fbxscene::Material> = Vec::new();
        let mut staged: std::collections::HashMap<String, Option<String>> = Default::default();
        let mut tex_written = 0usize;

        mats_out.push(fbxscene::Material {
            name: "mat_unknown".into(),
            diffuse: [0.8, 0.8, 0.8],
            texture: None,
            normal_map: None,
            wants_alpha: false,
            uv_set: "UVMap".into(),
        });
        let fallback = 0usize;

        let cols = (loaded.len() as f32).sqrt().ceil().max(1.0) as usize;
        let margin = 1.5f32;
        let mut place: Vec<(f32, f32)> = Vec::with_capacity(loaded.len());
        let mut z_cursor = 0.0f32;
        for row in loaded.chunks(cols) {
            let mut x_cursor = 0.0f32;
            let depth = row.iter().map(|l| l.hi[2] - l.lo[2]).fold(0.0f32, f32::max);
            for l in row {
                let w = l.hi[0] - l.lo[0];
                place.push((x_cursor + w * 0.5, z_cursor + depth * 0.5));
                x_cursor += w + margin;
            }
            z_cursor += depth + margin;
        }

        let mut meshes = Vec::new();
        let mut tris = 0usize;
        for (n, l) in loaded.iter().enumerate() {
            let (px, pz) = place[n];
            let dx = px - (l.lo[0] + l.hi[0]) * 0.5;
            let dy = -l.lo[1];
            let dz = pz - (l.lo[2] + l.hi[2]) * 0.5;
            for (i, pc) in l.pieces.iter().enumerate() {
                tris += pc.tris.len();
                let slot = match l.mats.get(pc.material) {
                    Some(path) if !path.is_empty() => {
                        if let Some(k) = mat_index.get(path) {
                            *k
                        } else {
                            let texture = if let Some(v) = staged.get(path) {
                                v.clone()
                            } else {
                                let mut found = None;
                                if let Some(md) = fs.read_path(path) {
                                    let text = String::from_utf8_lossy(&md).to_string();
                                    // The shader name decides whether the base
                                    // texture's alpha is opacity or a specular
                                    // mask - the same question the vehicle path
                                    // answers, and the same wrong answer turns
                                    // solid panels see-through.
                                    let effect = text
                                        .lines()
                                        .find(|l| l.trim_start().starts_with("material"))
                                        .and_then(|l| l.split('"').nth(1))
                                        .unwrap_or("")
                                        .to_string();
                                    let keeps_alpha = pit::Material {
                                        effect: effect.clone(),
                                        ..Default::default()
                                    }
                                    .wants_alpha();
                                    if let Some(tobj) = text
                                        .lines()
                                        .find(|l| l.trim_start().starts_with("texture[0]"))
                                        .and_then(|l| l.split('"').nth(1))
                                    {
                                        if let Some(td) = fs.read_path(tobj.trim()) {
                                            // the image path is the last printable
                                            // run in the .tobj
                                            let s = String::from_utf8_lossy(&td);
                                            if let Some(img) = s
                                                .split(|c: char| c.is_control())
                                                .filter(|t| t.contains('/') && t.contains('.'))
                                                .next_back()
                                            {
                                                if let Some(mut bytes) = fs.read_path(img) {
                                                    let flat =
                                                        img.trim_start_matches('/').replace('/', "_");
                                                    if let Some(legacy) =
                                                        crate::assemble::dds_from_dx10(&bytes)
                                                    {
                                                        bytes = legacy;
                                                    }
                                                    if !keeps_alpha {
                                                        crate::assemble::dds_force_opaque(&mut bytes);
                                                    }
                                                    if std::fs::write(tex_dir.join(&flat), &bytes).is_ok()
                                                    {
                                                        tex_written += 1;
                                                        found = Some(format!("textures/{flat}"));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                staged.insert(path.clone(), found.clone());
                                found
                            };
                            let short =
                                path.rsplit('/').next().unwrap_or("material").trim_end_matches(".mat");
                            mats_out.push(fbxscene::Material {
                                name: format!("mat_{short}"),
                                diffuse: [0.8, 0.8, 0.8],
                                texture,
                                normal_map: None,
                                wants_alpha: false,
                                uv_set: "UVMap".into(),
                            });
                            mat_index.insert(path.clone(), mats_out.len() - 1);
                            mats_out.len() - 1
                        }
                    }
                    _ => fallback,
                };
                meshes.push(fbxscene::Mesh {
                    name: format!("{}_{}_{}", l.name, n, i),
                    positions: pc
                        .positions
                        .iter()
                        .map(|v| [v[0] + dx, v[1] + dy, v[2] + dz])
                        .collect(),
                    uvs: pc.uv0.clone(),
                    uvs2: Vec::new(),
                    colors: pc.rgba.clone(),
                    tris: pc.tris.clone(),
                    material: slot,
                });
            }
        }

        let stem = archive
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "models".into());
        let out = outdir.join(format!("{stem} models.fbx"));
        if let Some(p) = out.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        match fbxscene::write_file(&out, &meshes, &mats_out) {
            Ok(()) => println!(
                "{} model(s) in {} row(s), {} meshes, {} triangles, {} materials, {} textures -> {}",
                loaded.len(),
                loaded.len().div_ceil(cols),
                meshes.len(),
                tris,
                mats_out.len(),
                tex_written,
                out.display()
            ),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        if dupes > 0 || skipped > 0 {
            println!("{dupes} duplicate(s) dropped, {skipped} unreadable");
        }
        return Ok(());
    }

    // Test whether a packer writes a model and its descriptor next to each
    // other in the archive. If it does, offset order pairs them even when both
    // are nameless - and the (piece, part) counts they each carry independently
    // are the check on whether that pairing is right.
    if args.len() > 2 && args[1] == "pairtest" {
        let mut fs = match hashfs::HashFs::open(&PathBuf::from(&args[2])) {
            Ok(f) => f,
            Err(e) => { eprintln!("{e}"); std::process::exit(1); }
        };
        let all: Vec<crate::hashfs::Entry> =
            fs.entries.values().filter(|e| !e.is_dir()).copied().collect();
        let mut models: Vec<(u64, usize, usize)> = Vec::new();
        let mut descs: Vec<(u64, usize, usize)> = Vec::new();
        for e in &all {
            let Ok(data) = fs.read_entry(e) else { continue };
            if data.len() > 4 && &data[1..4] == b"gmP" {
                if let Ok(m) = pmg::parse_bytes(&data) {
                    models.push((e.offset, m.pieces.len(), m.parts.len()));
                }
            } else if let Ok(d) = pmd::parse_bytes(&data) {
                descs.push((e.offset, d.piece_count, d.part_count));
            }
        }
        models.sort_by_key(|m| m.0);
        descs.sort_by_key(|d| d.0);
        println!("{} models, {} descriptors", models.len(), descs.len());

        let mut agree = 0usize;
        let mut disagree = 0usize;
        for (off, pieces, parts) in &models {
            // nearest descriptor by position in the file
            let Some(best) = descs
                .iter()
                .min_by_key(|d| (d.0 as i64 - *off as i64).abs())
            else { continue };
            if (best.1, best.2) == (*pieces, *parts) { agree += 1 } else { disagree += 1 }
        }
        println!("nearest-by-offset agrees on shape: {agree}, disagrees: {disagree}");
        return Ok(());
    }

    // How many nameless models can be matched to their nameless descriptors.
    if args.len() > 2 && args[1] == "pairinfo" {
        let root = PathBuf::from(&args[2]);
        let mut pmgs = Vec::new();
        let mut pmds = Vec::new();
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                match p.extension().and_then(|s| s.to_str()) {
                    Some("pmg") => {
                        if let Ok(m) = pmg::parse(&p) {
                            pmgs.push((p.clone(), m.pieces.len(), m.parts.len()));
                        }
                    }
                    Some("pmd") | Some("bin") => {
                        if let Ok(d) = pmd::parse(&p) {
                            pmds.push((p.clone(), d));
                        }
                    }
                    _ => {}
                }
            }
        }
        println!("{} models, {} descriptors", pmgs.len(), pmds.len());
        let (mut unique, mut harmless, mut risky, mut none) = (0, 0, 0, 0);
        for (_, pieces, parts) in &pmgs {
            let hits: Vec<&(PathBuf, pmd::Pmd)> =
                pmds.iter().filter(|(_, d)| d.signature() == (*pieces, *parts)).collect();
            if hits.is_empty() {
                none += 1;
                continue;
            }
            if hits.len() == 1 {
                unique += 1;
                continue;
            }
            // Several descriptors share this shape. That only matters if they
            // disagree about the materials - otherwise any of them is right.
            let first = &hits[0].1.materials;
            if hits.iter().all(|(_, d)| &d.materials == first) {
                harmless += 1;
            } else {
                risky += 1;
            }
        }
        println!(
            "unique {unique}, ambiguous-but-identical-materials {harmless},              genuinely ambiguous {risky}, unmatched {none}"
        );
        return Ok(());
    }

    // Why a given .scs took the path it did. Protector variants differ, and
    // guessing from the outside wastes more time than printing the numbers.
    if args.len() > 2 && args[1] == "zipinfo" {
        match rawzip::RawZip::open(&PathBuf::from(&args[2])) {
            Ok(z) => {
                println!("entries            {}", z.entries.len());
                println!("flagged encrypted  {}", z.flagged_encrypted);
                println!("inconsistent       {}", z.inconsistent);
                println!("falsely locked     {}", z.is_falsely_locked());
                for e in z.entries.iter().take(3) {
                    println!(
                        "  {:<40} flag=0x{:04x} method={} {}->{} at {}",
                        e.name, e.flag, e.method, e.csize, e.usize_, e.local_offset
                    );
                }
            }
            Err(e) => println!("RawZip::open failed: {e}"),
        }
        return Ok(());
    }

    if args.len() > 1 && args[1] == "convert" {
        return cli_convert(&args);
    }

    // A 2 x 1 x 0.5 m box with a distinct size on every axis, so an importer's
    // idea of scale and axis order can be read straight off the result.
    if args.len() > 2 && args[1] == "fbxtest" {
        let (sx, sy, sz) = (1.0f32, 0.5, 0.25);
        let p = |x: f32, y: f32, z: f32| [x * sx, y * sy, z * sz];
        let positions = vec![
            p(-1.0, -1.0, -1.0), p(1.0, -1.0, -1.0), p(1.0, 1.0, -1.0), p(-1.0, 1.0, -1.0),
            p(-1.0, -1.0, 1.0), p(1.0, -1.0, 1.0), p(1.0, 1.0, 1.0), p(-1.0, 1.0, 1.0),
        ];
        let tris = vec![
            [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
            [0, 1, 5], [0, 5, 4], [2, 3, 7], [2, 7, 6],
            [1, 2, 6], [1, 6, 5], [0, 4, 7], [0, 7, 3],
        ];
        let mesh = fbxscene::Mesh {
            name: "cube".into(),
            positions,
            uvs: vec![[0.0, 0.0]; 8],
            uvs2: Vec::new(),
            colors: Vec::new(),
            tris,
            material: 0,
        };
        let mat = fbxscene::Material {
            name: "cube_mat".into(),
            diffuse: [0.8, 0.3, 0.2],
            texture: None,
            normal_map: None,
            wants_alpha: false,
            uv_set: "UVMap".into(),
        };
        match fbxscene::write_file(&PathBuf::from(&args[2]), &[mesh], &[mat]) {
            Ok(()) => println!("wrote {}", args[2]),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([780.0, 640.0])
            .with_min_inner_size([620.0, 460.0])
            .with_title("scs2fbx"),
        ..Default::default()
    };
    eframe::run_native(
        "scs2fbx",
        opts,
        Box::new(|_cc| Ok(Box::<app::App>::default())),
    )
}

fn cli_convert(args: &[String]) -> eframe::Result<()> {
    if args.len() < 6 {
        eprintln!("usage: scs2fbx convert <archive.scs> <outdir> <cabin> <chassis> [interior_variant]");
        std::process::exit(2);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    let outdir = PathBuf::from(&args[3]);
    let opt = pipeline::Options {
        archive: PathBuf::from(&args[2]),
        outdir: outdir.clone(),
        converter_pix: exe_dir.join("tools").join("converter_pix.exe"),
        vehicle: String::new(),
        cabin: args[4].clone(),
        chassis: args[5].clone(),
        interior_variant: args.get(6).cloned().unwrap_or_default(),
        variants_row: !args.iter().any(|a| a == "--no-variants"),
        sounds: args.iter().any(|a| a == "--sounds"),
        skip_existing: args.iter().any(|a| a == "--skip-existing"),
        cleanup: false,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || pipeline::run(opt, tx));
    let mut failed = false;
    while let Ok(m) = rx.recv() {
        match m {
            pipeline::Msg::Step(s) => println!("== {s}"),
            pipeline::Msg::Info(s) => println!("   {s}"),
            pipeline::Msg::Warn(s) => println!("   ! {s}"),
            pipeline::Msg::Error(s) => {
                eprintln!("ERROR {s}");
                failed = true;
                break;
            }
            pipeline::Msg::Progress(_) => {}
            pipeline::Msg::Done(outs) => {
                for o in outs {
                    println!("OUT {}", o.display());
                }
                break;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}















