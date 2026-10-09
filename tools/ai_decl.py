"""Parse an exported idTech 6 decl (`doomx cat` output, `key = value;` / `key = { ... }`) into Python objects.

Repeated keys become lists. Usage:
  ai_decl.py attacks <attackgraph.decl>     one line per attack (node, name, arc, distance, via node, timers)
  ai_decl.py fsm <aifsmmanager.decl> [layer] states + transitions of the FSM sub graphs touching `layer`
  ai_decl.py dump <decl> <dotted.path>      print one sub tree
"""
import re
import sys

TOK = re.compile(r'"(?:[^"\\]|\\.)*"|[{}=;]|[^\s{}=;"]+')


def parse(text):
    toks = TOK.findall(text)
    pos = 0

    def block():
        nonlocal pos
        out = {}
        while pos < len(toks):
            t = toks[pos]
            if t == '}':
                pos += 1
                if pos < len(toks) and toks[pos] == ';':
                    pos += 1
                return out
            key = t.strip('"')
            pos += 1
            if pos < len(toks) and toks[pos] == '=':
                pos += 1
            if pos < len(toks) and toks[pos] == '{':
                pos += 1
                val = block()
            else:
                val = toks[pos].strip('"') if pos < len(toks) else None
                pos += 1
                if pos < len(toks) and toks[pos] == ';':
                    pos += 1
            if key in out:
                if not isinstance(out[key], list) or not getattr(out[key], 'multi', False):
                    lst = Multi([out[key]])
                    out[key] = lst
                out[key].append(val)
            else:
                out[key] = val
        return out

    while pos < len(toks) and toks[pos] != '{':
        pos += 1
    pos += 1
    return block()


class Multi(list):
    multi = True


def many(v):
    if v is None:
        return []
    return list(v) if isinstance(v, Multi) else [v]


def get(d, path, default=None):
    for p in path.split('.'):
        if not isinstance(d, dict) or p not in d:
            return default
        d = d[p]
    return d


def rng(d, key):
    r = get(d, key)
    if not isinstance(r, dict):
        return '-'
    lo, hi = r.get('minRange'), r.get('maxRange')
    if isinstance(lo, dict):
        lo, hi = lo.get('value'), hi.get('value')
    return f'{lo}..{hi}'


def attacks(root):
    for sg in many(get(root, 'edit.subGraphs.subGraph')):
        name = get(sg, 'object.object.name')
        for node in many(get(sg, 'nodes.node')):
            n = get(node, 'object.object', {})
            al = n.get('attackList', {})
            items = [al[k] for k in al if k.startswith('item[')]
            print(f'== subgraph {name} node {n.get("name")} ({len(items)} attacks)')
            for a in items:
                print(f'  {a.get("attackName")}: arc {get(a, "arcDirection.value")}+-{get(a, "arcHalfLength.value")}'
                      f' dist {rng(a, "distanceRange")} vert {rng(a, "distanceRange_Vertical")}'
                      f' abs {rng(a, "distanceRange_absolute")} 2d {a.get("use2DDistanceChecks")}'
                      f' w {a.get("weight")} dis {a.get("disabled")}'
                      f' via {str(a.get("viaNode")).split("/")[-2:]} dest {str(a.get("destNode")).split("/")[-1]}'
                      f' timer {a.get("attackTimer")} tba {rng(a, "timeBetweenAttacks")}'
                      f' shared {a.get("restrictWithSharedTimer")}:{a.get("sharedTimer")}:{rng(a, "sharedTimerInterval")}'
                      f' moving {a.get("usableWhileStopped")}/{a.get("usableWhileWalking")}/{a.get("usableWhileRunning")}'
                      f'/{a.get("usableOutsideOfMoveCycle")} pred {get(a, "predictionTime.value")}'
                      f' flags "{a.get("flags")}" src {get(a, "sourceNodes.num")} pre {get(a, "preconditions.num")}'
                      f' tok {get(a, "requiredTokens.num")} mem {get(a, "requiredMemoryKeys.num")}'
                      f' validator {a.get("attackValidator")} expand {a.get("expand")}')
        print('  links', flat(sg.get('links')))


def fsm(root, layer=None):
    for sg in many(get(root, 'edit.subGraphs.subGraph')):
        hdr = get(sg, 'object.object', {})
        layers = many(get(sg, 'layers.layer'))
        if layer and layer not in layers:
            continue
        print(f'== fsm {hdr.get("name")} root {hdr.get("isRoot")} layers {layers}')
        for node in many(get(sg, 'nodes.node')):
            n = get(node, 'object.object', {})
            nl = many(get(node, 'layers.layer'))
            if layer and nl and layer not in nl:
                continue
            st = n.get('stateType', {})
            obj = st.get('object', {}) if isinstance(st, dict) else {}
            print(f'  state {n.get("name")} [{st.get("className")}] {flat(obj)} child {n.get("childFSMName")} layers {nl}')
        trans = sg.get('links', {}) or {}
        for start, links in trans.items():
            for link in many(get(links, 'link')):
                t = get(link, 'object.object', {})
                tt = t.get('transitionType', {})
                obj = tt.get('object', {}) if isinstance(tt, dict) else {}
                print(f"  {link.get('startNode')} -> {link.get('endNode')} #"
                      f'{t.get("orderIndex")} [{tt.get("className")}]'
                      f' {t.get("transCode")} {t.get("transGroup")} {flat(obj)}')


def flat(d, pre=''):
    out = []
    if isinstance(d, dict):
        for k, v in d.items():
            if k in ('position',):
                continue
            if isinstance(v, (dict, list)):
                out.append(flat(v, pre + k + '.'))
            else:
                out.append(f'{pre}{k}={v}')
    elif isinstance(d, list):
        for i, v in enumerate(d):
            out.append(flat(v, f'{pre}{i}.'))
    return ' '.join(x for x in out if x)


def main():
    cmd, path = sys.argv[1], sys.argv[2]
    root = parse(open(path, encoding='utf-8', errors='replace').read())
    if cmd == 'attacks':
        attacks(root)
    elif cmd == 'fsm':
        fsm(root, sys.argv[3] if len(sys.argv) > 3 else None)
    elif cmd == 'dump':
        print(flat(get(root, sys.argv[3])))


if __name__ == '__main__':
    main()
