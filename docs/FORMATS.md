# Notes on the formats

Things that cost time to work out, recorded so they do not have to be worked out
again. The source comments carry the detail.

**HashFS keys paths with a hybrid CityHash64.** Strings of 64 bytes or fewer use
the 1.0.x short branches; longer ones use the 1.1 main loop. Across 4773 real
paths, 1.0.x alone scores 2322/2322 on the short ones and 0/2451 on the long
ones; the hybrid scores 4773/4773. Do not "correct" it toward any single
official CityHash release.

**A `.pim` locator quaternion is (w, x, y, z), not (x, y, z, w).** Read the other
way, a truck's cab-back panel lands three metres above the roof.

**SCS geometry is Y-up, which is also FBX's convention**, so coordinates go out
untouched — the Blender addon's own conversion is a single `Rotation(pi/2, X)`,
exactly the axis change an FBX importer applies to a Y-up file.

**A packer writes a model and its descriptor next to each other.** That is the
only handle left when an archive ships no directory listing and both are named
by hash — and the piece/part counts each file carries independently confirm it.
Across one 228-model pack, all 228 agree.

**A `.pmg` piece's field offsets follow C declaration order, not the offset
comments in ConverterPIX's own headers** — those are stale for 0x14/0x15, where
the index pool offset is at +96 and not the +88 the comment claims.

**A name token is base 38 over `0-9`, then `a-z`, then `_`** — not letters
first. Getting it backwards turns `chs_6x4` into `mr19g6e`.

**A `.pit` nests `Attribute`/`Texture` blocks inside `Material`.** A reader that
treats any `}` as closing the current material shuts it at the first attribute
and loses every texture path.

**Some mods are "locked" without being encrypted.** They set the ZIP encrypted
flag, or overwrite the local file headers outright, or overstate the central
directory size — the game ignores all of it, while conforming tools ask for a
password that was never set. Where the metadata contradicts itself, this reads
the archive anyway. A genuinely encrypted archive is still refused.

**Some mods obfuscate their definitions with "3nK".** That is not encryption
either: an XOR against a fixed 256-byte table, offset by a seed in the file's own
header. It has to be undone in the deep scan as well as at parse time — the
definitions are where model paths are harvested from, so leaving them scrambled
puts a car's entire bodywork in `_unknown` under a hash.

**A deep scan that only reads named entries is circular.** One trailer pack
lists 33 of its 2399 entries, while 679 unnamed material files each spell out
the full path of the textures they use. Reading the unnamed entries too took the
named count from 50 to 1814.

## Sound

An FMOD `.bank` is a RIFF `FEV FMT` container with an FSB5 sample bank inside.
Across 59 banks, every one is FSB5 Vorbis except two in PCM16 — Vorbis with the
Ogg framing and the codebook header stripped, only a CRC of the codebook left.
ffmpeg refuses it (`version 5 is not implemented`); vgmstream carries FMOD's
codebook table and decodes it straight from the `.bank`, sample names included.

Modern mods ship almost everything this way: across 31 test mods only 5 had
loose `.ogg` files, while 23 had banks holding 5000-odd samples between them.

A truck's `def/vehicle/truck/<name>/engine/sound.sui` lists
`name|/sound/x.bank#event` pairs. Sample names repeat inside a bank because the
interior and exterior versions share an event name; they are different
recordings.

**An engine is a set of short loops, each recorded at one fixed RPM.** Mods from
before 1.37 declare the parameters outright, which is what makes the model
readable at all — this is one truck's exterior definition:

| slot | `pitch_reference` | `min_rpm` | `max_rpm` | `volume` |
|---|---|---|---|---|
| `engine[]` (off throttle), 8 bands | 580 … 2210 | 200 | 3000 | 0.45 → 0.62 |
| `engine_load[]` (on throttle), 6 bands | 580 … 2150 | 300 | 3000 | 0.55 → 0.90 |
| `engine_exhaust[]`, 5 bands | 600 … 2210 | 300 | 3000 | 0.55 → 0.75 |
| `engine_nofuel[]`, 6 bands | 600 … 2250 | 300 | 3000 | 0.46 → 0.75 |

Each band declares its own window, and the windows overlap — `unloaded_3` is
recorded at 1050 and plays across 850–1160, its neighbours at 800 and 1240. The
overlap is the crossfade region. A band's playback rate is `rpm /
pitch_reference`, so both bands in an overlap are pulled to the same pitch and
blend instead of beating against each other. Loaded bands are louder than
unloaded ones at the same RPM, volume rises with RPM, and the top band's window
runs to 3000 so nothing goes silent past the redline.

Banks from FMOD mods declare none of this; the bands are only numbered, and
some carry the RPM in the file name instead (`..._1050_r`).
