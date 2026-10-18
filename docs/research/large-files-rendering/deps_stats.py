"""Shape of a node_modules tree, for docs/research/large-files-rendering.md.

Usage: python3 -I deps_stats.py <project-dir>

Reads <project-dir>/package.json and <project-dir>/node_modules without
following symlinks for the totals. For every direct dependency it resolves
the declaration entry the way TypeScript's bundler resolution does in the
common cases (`types` / `typings`, `exports["."]` with a `types` condition,
`index.d.ts`, `main` with `.d.ts`, then `@types/<name>`) and follows relative
imports, re-exports and `/// <reference path>` from that entry: the
"declaration closure" a goto into the package needs.
"""

import json
import os
import re
import sys
from collections import Counter

DTS = (".d.ts", ".d.mts", ".d.cts")


def kind(name):
    if name.endswith(DTS):
        return "dts"
    if name.endswith((".js", ".mjs", ".cjs", ".jsx")):
        return "js"
    if name.endswith((".ts", ".tsx", ".mts", ".cts")):
        return "ts_src"
    if name.endswith(".map"):
        return "map"
    if name.endswith(".json"):
        return "json"
    for ext in (".go", ".rs", ".py", ".pyi", ".java"):
        if name.endswith(ext):
            return ext[1:]
    if name.endswith((".node", ".wasm", ".so", ".dylib", ".exe")) or "." not in name:
        return "binary_or_noext"
    return "other"


def totals(root):
    files = Counter()
    sizes = Counter()
    links = 0
    pkgs = set()
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for d in list(dirnames):
            if os.path.islink(os.path.join(dirpath, d)):
                links += 1
        for f in filenames:
            p = os.path.join(dirpath, f)
            st = os.lstat(p)
            if os.path.islink(p):
                links += 1
                continue
            k = kind(f)
            files[k] += 1
            sizes[k] += st.st_size
            if f == "package.json":
                parent = os.path.dirname(p)
                gp = os.path.basename(os.path.dirname(parent))
                ggp = os.path.basename(os.path.dirname(os.path.dirname(parent)))
                if gp == "node_modules" or (gp.startswith("@") and ggp == "node_modules"):
                    try:
                        with open(p, encoding="utf-8") as fh:
                            m = json.load(fh)
                        if "name" in m:
                            pkgs.add((m.get("name"), m.get("version")))
                    except (OSError, ValueError):
                        pass
    return files, sizes, links, pkgs


def types_condition(exp):
    """The `types` target of an exports entry, searching nested conditions."""
    if isinstance(exp, str):
        return exp if exp.endswith(DTS) else None
    if isinstance(exp, list):
        for e in exp:
            t = types_condition(e)
            if t:
                return t
        return None
    if isinstance(exp, dict):
        if "types" in exp and isinstance(exp["types"], str):
            return exp["types"]
        for key in ("import", "require", "node", "default"):
            if key in exp:
                t = types_condition(exp[key])
                if t:
                    return t
    return None


def entry(pkg_dir, manifest):
    for key in ("types", "typings"):
        v = manifest.get(key)
        if isinstance(v, str) and os.path.isfile(os.path.join(pkg_dir, v)):
            return v, key
    exp = manifest.get("exports")
    if exp is not None:
        dot = exp.get(".", exp) if isinstance(exp, dict) else exp
        t = types_condition(dot)
        if t and os.path.isfile(os.path.join(pkg_dir, t)):
            return t, "exports"
    if os.path.isfile(os.path.join(pkg_dir, "index.d.ts")):
        return "index.d.ts", "index"
    main = manifest.get("main")
    if isinstance(main, str):
        base = re.sub(r"\.(c|m)?js$", "", main)
        for ext in DTS:
            if os.path.isfile(os.path.join(pkg_dir, base + ext)):
                return base + ext, "main"
    return None, None


SPEC = re.compile(
    r"""(?:from\s+|import\s*\(\s*|import\s+|require\s*\(\s*)["']([^"']+)["']"""
    r"""|///\s*<reference\s+path\s*=\s*["']([^"']+)["']"""
    r"""|///\s*<reference\s+types\s*=\s*["']([^"']+)["']"""
)


def resolve(base_dir, spec):
    p = os.path.normpath(os.path.join(base_dir, spec))
    stem = re.sub(r"\.(c|m)?js$", "", p)
    cands = [p] if p.endswith(DTS) else []
    cands += [stem + e for e in DTS] + [os.path.join(p, "index" + e) for e in DTS]
    for c in cands:
        if os.path.isfile(c):
            return c
    return None


def closure(start):
    seen = set()
    stack = [os.path.realpath(start)]
    bare = set()
    total = 0
    while stack:
        f = stack.pop()
        if f in seen:
            continue
        seen.add(f)
        try:
            with open(f, encoding="utf-8", errors="replace") as fh:
                text = fh.read()
        except OSError:
            continue
        total += len(text.encode("utf-8"))
        for m in SPEC.finditer(text):
            spec = m.group(1) or m.group(2)
            if m.group(3):
                bare.add(m.group(3))
                continue
            if spec is None:
                continue
            if spec.startswith("."):
                r = resolve(os.path.dirname(f), spec)
                if r:
                    stack.append(os.path.realpath(r))
            elif m.group(2):
                r = resolve(os.path.dirname(f), spec)
                if r:
                    stack.append(os.path.realpath(r))
            else:
                bare.add(spec.split("/")[0] if not spec.startswith("@") else "/".join(spec.split("/")[:2]))
    return len(seen), total, bare


def pkg_size(pkg_dir):
    n = 0
    b = 0
    dts = 0
    for dirpath, dirnames, filenames in os.walk(pkg_dir, followlinks=False):
        dirnames[:] = [d for d in dirnames if d != "node_modules"]
        for f in filenames:
            p = os.path.join(dirpath, f)
            if os.path.islink(p):
                continue
            n += 1
            s = os.lstat(p).st_size
            b += s
            if f.endswith(DTS):
                dts += s
    return n, b, dts


def tree(root):
    """Files and bytes by kind under any directory (Go, Cargo, Python caches)."""
    files, sizes, links, _ = totals(root)
    mib = 1024 * 1024
    big = 0
    for dirpath, _, filenames in os.walk(root, followlinks=False):
        for f in filenames:
            p = os.path.join(dirpath, f)
            if not os.path.islink(p) and os.lstat(p).st_size > 1024 * 1024:
                big += 1
    print(f"dir\t{root}")
    print(f"files\t{sum(files.values())}\tsymlinks\t{links}\tover_1MiB\t{big}")
    print(f"bytes\t{sum(sizes.values()) / mib:.1f} MiB")
    for k in sorted(files):
        print(f"  {k}\t{files[k]} files\t{sizes[k] / mib:.1f} MiB")


def main():
    if sys.argv[1] == "--tree":
        tree(sys.argv[2])
        return
    proj = sys.argv[1]
    nm = os.path.join(proj, "node_modules")
    with open(os.path.join(proj, "package.json"), encoding="utf-8") as fh:
        app = json.load(fh)
    files, sizes, links, pkgs = totals(nm)
    mib = 1024 * 1024
    print(f"project\t{proj}")
    print(f"packages (name@version)\t{len(pkgs)}")
    print(f"files\t{sum(files.values())}\tsymlinks\t{links}")
    print(f"bytes\t{sum(sizes.values()) / mib:.1f} MiB")
    for k in sorted(files):
        print(f"  {k}\t{files[k]} files\t{sizes[k] / mib:.1f} MiB")

    direct = list(app.get("dependencies", {})) + list(app.get("devDependencies", {}))
    print()
    print("direct\tversion\tpkg_files\tpkg_MiB\tpkg_dts_KiB\ttypes_from\tentry\tclosure_files\tclosure_KiB\tbare_imports")
    sum_closure = 0
    sum_pkg = 0
    for name in direct:
        d = os.path.join(nm, name)
        if not os.path.exists(d):
            print(f"{name}\tmissing")
            continue
        real = os.path.realpath(d)
        with open(os.path.join(real, "package.json"), encoding="utf-8") as fh:
            man = json.load(fh)
        n, b, dts = pkg_size(real)
        sum_pkg += b
        e, how = entry(real, man)
        if e is None and not name.startswith("@types/"):
            tname = "@types/" + (name[1:].replace("/", "__") if name.startswith("@") else name)
            td = os.path.join(nm, tname)
            if os.path.exists(td):
                treal = os.path.realpath(td)
                with open(os.path.join(treal, "package.json"), encoding="utf-8") as fh:
                    tman = json.load(fh)
                te, thow = entry(treal, tman)
                if te:
                    real, e, how = treal, te, "@types:" + thow
        if e is None:
            print(f"{name}\t{man.get('version')}\t{n}\t{b / mib:.2f}\t{dts / 1024:.0f}\tnone\t-\t0\t0\t0")
            continue
        cf, cb, bare = closure(os.path.join(real, e))
        sum_closure += cb
        print(
            f"{name}\t{man.get('version')}\t{n}\t{b / mib:.2f}\t{dts / 1024:.0f}\t{how}\t{e}\t{cf}\t{cb / 1024:.0f}\t{len(bare)}"
        )
    print()
    print(f"direct packages total\t{sum_pkg / mib:.1f} MiB")
    print(f"direct declaration closures total\t{sum_closure / mib:.2f} MiB")
    print(f"all declaration files\t{sizes['dts'] / mib:.1f} MiB in {files['dts']} files")


if __name__ == "__main__":
    main()
