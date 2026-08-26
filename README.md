# scs2fbx

Convert a Euro Truck Simulator 2 / American Truck Simulator vehicle mod (`.scs`)
into an `.fbx` with textures, and pull its audio out as MP3.

One executable — no Blender, no Python, no addon. The geometry is read straight
out of the mod and the FBX is written directly, about 30 seconds per vehicle.

## Install

Download `scs2fbx.exe` from [Releases](../../releases) and run it. Nothing to
install.

Two optional tools unlock the audio side, found on `PATH` or beside the exe:

| Tool | Without it |
|---|---|
| [ffmpeg](https://ffmpeg.org) | audio comes out in its original format instead of MP3 |
| [vgmstream](https://vgmstream.org) | FMOD `.bank` files are copied undecoded (`vgmstream-cli.exe` in a `vgmstream/` folder beside the exe) |

## Use

Run `scs2fbx.exe`, drop a `.scs` on the window, press **Convert**. Output lands
beside the archive:

```
<mod name>/
    <mod name>.fbx
    textures/
    sounds/          (with the sound option on)
```

### CLI

```bash
scs2fbx.exe convert  "mod.scs" "out_dir" <cabin> <chassis> [interior_variant]
scs2fbx.exe extract  "mod.scs" "out_dir"          # unpack the archive
scs2fbx.exe sounds   "mod.scs" "out_dir"          # audio only
scs2fbx.exe soundlib "mods_dir" "out_dir"         # one sound library from many mods
scs2fbx.exe models   "mod.scs" "out_dir"          # every model, one FBX, laid out in rows
scs2fbx.exe pmginfo  "model.pmg"
scs2fbx.exe zipinfo  "mod.scs"
```

Flags: `--no-variants`, `--sounds`, `--skip-existing`, and `--reuse <dir>` for
`soundlib`.

`convert` builds the vehicle the game shows by default — chassis, cab, interior,
accessories, wheels and the paint job. `models` is for archives with nothing to
assemble, like a trailer pack: it reads every model it finds and lays them out
in rows in a single file.

## What comes out

**Variants are kept.** A mod ships several bumpers, roof bars and bunk layouts.
Leaving them in place stacks them inside the bodywork, so they are parked in a
row beside the vehicle as `*_alt_<variant>`. This is most of the file size — one
bus goes from 48 MB to 271 MB — so `--no-variants` turns it off.

**No custom split normals.** No normal layer is written, so nothing overrides
the normals your application computes. PIX geometry already splits vertices at
every hard edge and UV seam, so recomputed normals match the authored ones.

**Colour only.** Textures are wired to diffuse and nothing else. In SCS shaders
a base texture's alpha channel is a specular mask for everything except the
glass family, and an importer reading it as opacity turns solid bodywork
see-through — so alpha is forced opaque there. Real glass keeps its alpha.

## Limits

- Wheels only appear if the mod ships wheel models; many use base-game ones.
- Textures a mod references but does not ship get a neutral placeholder.
- DDS in BC6H/BC7 cannot be rewritten to a legacy header; some applications
  will not read them.
- Animation is not exported.

## Build

```bash
cargo build --release
```

Any stable Rust toolchain on Windows. With a GNU toolchain from w64devkit, copy
`libgcc.a` to `libgcc_eh.a` — Rust still links `-lgcc_eh`.

```bash
cargo test
```

[`docs/FORMATS.md`](docs/FORMATS.md) records what the file formats cost to work
out — the HashFS hash, the `.pmg` layout, the fake-locked archives, how ETS2
drives engine audio — so it does not have to be worked out again.

## Licence

scs2fbx is MIT ([LICENSE](LICENSE)). Bundled third-party software is
listed in [NOTICE](NOTICE).

`vendor/converter_pix.exe` is [ConverterPIX](https://github.com/mwl4/ConverterPIX)
by Michał Leśniak, **LGPL-3.0** — see
[`vendor/ConverterPIX-LICENSE.txt`](vendor/ConverterPIX-LICENSE.txt). It is
embedded in the executable, unpacked at runtime and run as a separate process;
its source is at the link above. The 3nK table and algorithm come from
[SII_Decrypt](https://github.com/TheLazyTomcat/SII_Decrypt) by František Milt.

This tool only converts. The models, textures and sounds inside a mod belong to
its author — several archives it can open are marked by their authors as not for
redistribution — and vehicle marques hold separate trademark and design rights.
Publishing anything built from mod content needs the author's permission.
