"""Summarise the campaign weapon mods from decls extracted by `doomx extract` (gamedata/re/mods/generated/decls).

usage: mods_summary.py <decls_root>
For every perk group in perkgroups/perkgroups/weapons/sp: each perk family (base perk, unhide slot, upgrades,
mastery, weaponMastery) and, for every perk, the upgrade decls it activates with their weapon modifiers
(type, opType (default SET), value, fireMode). Read-only; prints text."""
import os, re, sys

TOK = re.compile(r'"(?:[^"\\]|\\.)*"|[{}=;]|[^\s{}=;"]+')


def parse(text):
    toks = TOK.findall(text)
    pos = 0

    def block():
        nonlocal pos
        out = {}
        while pos < len(toks) and toks[pos] != "}":
            key = toks[pos]; pos += 1
            if toks[pos] == "=":
                pos += 1
            if toks[pos] == "{":
                pos += 1; val = block(); pos += 1
            else:
                val = toks[pos].strip('"'); pos += 1
            if pos < len(toks) and toks[pos] == ";":
                pos += 1
            out[key] = val
        return out

    if toks and toks[0] == "{":
        pos = 1
        return block()
    return block()


def items(d):
    if not isinstance(d, dict):
        return []
    return [d[k] for k in sorted((k for k in d if k.startswith("item[")), key=lambda s: int(s[5:-1]))]


def load(root, kind, name):
    p = os.path.join(root, kind, *name.split("/")) + ".decl"
    if not os.path.exists(p):
        return None
    d = parse(open(p, encoding="latin1").read())
    return d.get("edit", d)


def mod_str(m):
    data = m.get("data", {}) if isinstance(m.get("data"), dict) else {}
    vals = ",".join(f"{k[5:] if k.startswith('value') else k}={v}" for k, v in data.items())
    op = m.get("opType", "SET").replace("MOD_OPERATOR_", "")
    fm = m.get("fireMode", "").replace("WEAPONFIREMODE_", "")
    return f"{m.get('type', '?').replace('WMT_WEAPON_', '')} {op} {vals}" + (f" [{fm}]" if fm else "")


def upgrade_str(root, name):
    u = load(root, "upgrade", name)
    if u is None:
        return f"    upgrade {name}: <missing>"
    fm = u.get("fireMode", "").replace("WEAPONFIREMODE_", "")
    flags = (" allFireModes" if u.get("allFireModes") == "true" else "") + (f" mode={fm}" if fm else "")
    mods = [mod_str(m) for m in items(u.get("modifiersWeapon", {}))]
    mods = [m for m in mods if not m.startswith(("FX_DECL", "RETICLE_DECL"))]
    return f"    upgrade {name.split('/')[-1]}{flags}: " + ("; ".join(mods) if mods else "(fx/reticle only)")


def main():
    root = sys.argv[1]
    gdir = os.path.join(root, "perkgroups", "perkgroups", "weapons", "sp")
    for f in sorted(os.listdir(gdir)):
        g = load(root, "perkgroups", "perkgroups/weapons/sp/" + f[:-5])
        print(f"== {f[:-5]}")
        for fam in items(g.get("perkFamilies", {})):
            wm = fam.get("weaponMastery", {})
            wmt = wm.get("type", "") if isinstance(wm, dict) else ""
            extra = {k: v for k, v in wm.items() if k not in ("type", "masteryEventSound")} if isinstance(wm, dict) else {}
            print(f"  family {fam.get('base','?').split('/')[-1]}  slot={fam.get('unhideWeaponModSelect','NONE')[-1]}"
                  f"  mastery={wmt} {extra if extra else ''}")
            perks = [fam.get("base")] + items(fam.get("upgrades", {})) + [fam.get("mastery")]
            for pn in perks:
                if not pn:
                    continue
                p = load(root, "perks", pn)
                if p is None:
                    print(f"   perk {pn}: <missing>"); continue
                cost = p.get("schematicData", {}).get("buildPoints", "") if isinstance(p.get("schematicData"), dict) else ""
                print(f"   perk {pn.split('/')[-1]}" + (f" (points {cost})" if cost else ""))
                for un in items(p.get("upgrades", {})):
                    print(upgrade_str(root, un))


if __name__ == "__main__":
    main()
