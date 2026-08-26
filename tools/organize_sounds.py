"""Reorganise D:/testxe_SOUND (mirror of each mod's own layout) into
D:/ETS2_Sound_Library, grouped by what a sound is *for*.

Engine RPM bands stay together per source bank: a set of loaded_1..14 belongs
to one engine recording and must never be mixed with another's. Everything
else is flattened per category, with the bank name kept as a prefix only when
two banks contribute the same sample name.
"""
import os, re, json, shutil, hashlib, sys
from collections import defaultdict

SRC = sys.argv[1] if len(sys.argv) > 1 else r"D:\testxe_SOUND"
DST = sys.argv[2] if len(sys.argv) > 2 else r"D:\ETS2_Sound_Library"

# (category, regex on lowercased sample stem) - first match wins
RULES = [
    ("engine/bands",    r"(^|_)(loaded|unloaded|load|unload|noload|nofuel|nf|idle|midle)(_|$)|rpm|^(on|off)(low|mid|high)$|(^|_)e\d+$|(^|_)el\d+$|(^|_)enf\d+$|vs\d"),
    ("engine/exhaust",  r"exhaust|escape"),
    ("engine/start_stop", r"(^|_)(start|stop|engine_on|engine_off|partida|desliga|mogok)(_|$)|start_bad|startbad"),
    ("engine/turbo",    r"turbo|blow_off|bov"),
    ("engine/brake",    r"engine_brake|retarder|jake|freio_motor|desaceleracao"),
    ("transmission",    r"gear|caixa|opticruise|shift|cambio|marcha|clutch|embreagem"),
    ("brakes_air",      r"air_brake|air_cutoff|air_c|air_b|air_dryer|descarga_ar|buang_angin|rem_angin|low_air|cuica|krekk|brake|brk"),
    ("horn",            r"horn|buzina|klakson|ken|kèn|trompete|musical"),
    ("cabin",           r"blinker|winker|seta|pisca|wiper|limpador|stick|button|botton|botao|switch|chave|alavanca|inside|interior|som_int|warning|triang|ar_condicionado|ventilo|ventoinha|aircon|lift_axle|maneco|lever|key|belt|door|porta|window|vidro|cab$"),
    ("chassis",         r"suspension|damaged|reverse|mundur|rain|aero|noise|tire|tyre|pneu|road|wind|hitch|engate|trailer|obrubnik|curb"),
]

def category(stem):
    s = stem.lower()
    for cat, rx in RULES:
        if re.search(rx, s):
            return cat
    return "misc"

def band_index(stem):
    m = re.search(r"(\d+)$", stem)
    return int(m.group(1)) if m else None

stats = defaultdict(int)
manifest = {}
if os.path.isdir(DST):
    shutil.rmtree(DST)

for vehicle in sorted(os.listdir(SRC)):
    vsrc = os.path.join(SRC, vehicle)
    if not os.path.isdir(vsrc):
        continue
    vname = re.sub(r"\s+", " ", vehicle).strip()
    vdst = os.path.join(DST, vname)
    engines = defaultdict(list)     # bank -> [(band_index, stem, path)]
    flat = defaultdict(list)        # category -> [(bank, stem, path)]

    for root, _, files in os.walk(vsrc):
        mp3s = [f[:-4] for f in files if f.lower().endswith(".mp3")]
        # A run of the same prefix numbered 1..N (N >= 4) in one bank is an
        # engine recorded at rising RPM, whatever the prefix is called:
        # paccar_mx_13_rm_1..30, om_457_la_1..16, T3_OM355LA_1..5.
        series = defaultdict(set)
        for stem in mp3s:
            m = re.match(r"^(.*?)[_ ]?(\d+)$", stem)
            if m:
                series[m.group(1)].add(int(m.group(2)))
        band_prefixes = {p for p, ns in series.items() if len(ns) >= 4 and p}
        # names that carry the RPM itself: paccar_mx_13_rm__1050_r, mercedes_1500rpm_ext
        rpm_named = {st for st in mp3s if re.search(r"(^|_)(\d{3,4})(rpm)?(_(r|l|ext|int|e|i))?$", st.lower())}
        for f in files:
            if not f.lower().endswith(".mp3"):
                continue
            path = os.path.join(root, f)
            stem = f[:-4]
            rel = os.path.relpath(root, vsrc).replace("\\", "/")
            bank = rel.split("/")[-1] if rel != "." else "loose"
            # an unnamed stream is only a number; the bank it came from says
            # what it is (interior_sound/00000011 is cabin ambience)
            if re.fullmatch(r"\d+", stem):
                stem_for_rules = bank
            else:
                stem_for_rules = stem
            m = re.match(r"^(.*?)[_ ]?(\d+)$", stem)
            forced = (m and m.group(1) in band_prefixes and category(stem) in ("misc", "engine/bands")) or stem in rpm_named
            cat = "engine/bands" if forced else category(stem_for_rules)
            if cat == "engine/bands":
                engines[(rel, bank)].append((band_index(stem), stem, path))
            else:
                flat[cat].append((bank, stem, path))

    vman = {"engine_sets": {}, "categories": {}}

    # engine bands: one folder per source bank, bands in numeric order
    for (rel, bank), items in sorted(engines.items()):
        setname = bank if bank != "loose" else rel.replace("/", "_")
        sdst = os.path.join(vdst, "engine", "bands", setname)
        os.makedirs(sdst, exist_ok=True)
        items.sort(key=lambda t: (t[1].rstrip("0123456789_"), t[0] or 0))
        groups = defaultdict(list)
        for idx, stem, path in items:
            shutil.copy2(path, os.path.join(sdst, stem + ".mp3"))
            stats["engine/bands"] += 1
            rpm = re.search(r"(^|_)(\d{3,4})(rpm)?(_(r|l|ext|int|e|i))?$", stem.lower())
            kind = re.sub(r"(^|_)\d{3,4}(rpm)?(_(r|l|ext|int|e|i))?$", "", stem.lower()) if rpm else stem.rstrip("0123456789").rstrip("_")
            entry = {"file": stem + ".mp3"}
            if rpm:
                entry["rpm"] = int(rpm.group(2))
            elif idx is not None:
                entry["band"] = idx
            groups[kind].append(entry)
        vman["engine_sets"][setname] = {k: v for k, v in sorted(groups.items())}

    # everything else, flattened per category
    for cat, items in sorted(flat.items()):
        cdst = os.path.join(vdst, cat)
        os.makedirs(cdst, exist_ok=True)
        seen = defaultdict(int)
        for bank, stem, path in items:
            seen[stem] += 1
        names = []
        for bank, stem, path in sorted(items, key=lambda t: (t[1], t[0])):
            name = stem if seen[stem] == 1 else f"{bank}__{stem}"
            out = os.path.join(cdst, name + ".mp3")
            if os.path.exists(out):
                h = hashlib.md5(open(path, "rb").read()).hexdigest()[:6]
                out = os.path.join(cdst, f"{name}__{h}.mp3")
            shutil.copy2(path, out)
            names.append(os.path.basename(out))
            stats[cat] += 1
        vman["categories"][cat] = names

    manifest[vname] = vman

with open(os.path.join(DST, "manifest.json"), "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=1, ensure_ascii=False)

print("vehicles:", len(manifest))
for k, v in sorted(stats.items(), key=lambda kv: -kv[1]):
    print("  %-22s %5d" % (k, v))
print("  total", sum(stats.values()))
