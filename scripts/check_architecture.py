#!/usr/bin/env python3
"""Check the architecture rules in AGENTS.md against `cargo metadata`.

Rules checked:
  1. Every workspace crate declares [package.metadata.lectern] layer.
  2. Dependencies point downwards: app -> view -> domain -> infra.
     kernel crates (kernel, services) may be used by any layer.
     tool crates may depend on anything; only app may depend on a tool crate,
     and only as a dev-dependency or an optional (feature-gated) one.
  3. Crates in the domain, infra and kernel layers never depend on GPUI,
     directly or transitively.
  4. macOS system bindings are direct dependencies of `platform` only. A
     binding is a crate whose name matches MACOS_ONLY_PREFIXES or contains a
     MACOS_NAME_TOKENS token, or any dependency whose
     target predicates can only hold on Apple platforms (apple_only_cfg).

Optional dependencies are included (`cargo metadata --all-features`), so a
rule broken behind a feature flag still fails. A crate's own dev-dependencies
count too; those of its dependencies are never compiled and are skipped.

Exit status is 1 when any rule is broken.

With --gpui-free-packages, print `-p <crate>` for every crate that builds
and tests without GPUI instead (used by the Linux CI job).
"""

import concurrent.futures
import hashlib
import json
import os
import re
import tomllib
import subprocess
import sys

RANK = {"app": 4, "view": 3, "domain": 2, "infra": 1}
GPUI_FREE_LAYERS = {"domain", "infra", "kernel"}
GPUI_PREFIXES = ("gpui",)
# macOS system bindings, matched by name prefix so that -sys crates and new
# objc2-* framework crates are covered without listing each one.
MACOS_ONLY_PREFIXES = (
    "objc",
    "cocoa",
    "core-foundation",
    "core-graphics",
    "core-text",
    "core-video",
    "fsevent",
    "security-framework",
    "metal",
    "dispatch",
    "block2",
    "io-kit",
    "mach2",
    "mach-sys",
    "mac-notification",
    "screencapturekit",
    "system-configuration",
    "tao-core-video",
    "coreaudio",
    "cidre",
    "icrate",
    "apple-",
)
# Name tokens (split on - and _) that mark an Apple binding wherever they
# appear, e.g. accesskit_macos, raw-window-metal, objc2-app-kit.
MACOS_NAME_TOKENS = {"macos", "darwin", "apple", "appkit", "cocoa", "metal", "objc", "objc2", "coreaudio", "corefoundation"}
# A dependency whose target predicate holds on some Apple target and on no
# other target rustc knows is a macOS binding whatever its name, e.g.
# [target.'cfg(target_os = "macos")'.dependencies]. Predicates are evaluated
# against the real cfg of every target (rustc --print cfg), so aliases such as
# `unix` / target_family = "unix", multi-valued keys and Mac Catalyst are exact.
# Names no target sets (feature, test, proc_macro, ...) are false, as in Cargo:
# they cannot select target dependencies. Configured rustflags are applied.
LINUX_TARGET = "x86_64-unknown-linux-gnu"  # the Linux CI runner
PLATFORM_CRATE = "lectern-platform"


def metadata(platform=None):
    cmd = ["cargo", "metadata", "--format-version", "1", "--locked", "--all-features"]
    if platform:
        # Keep only the dependencies that are compiled for this target.
        cmd += ["--filter-platform", platform]
    out = subprocess.run(
        cmd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def is_macos_binding_name(name):
    return name.startswith(MACOS_ONLY_PREFIXES) or bool(MACOS_NAME_TOKENS & set(re.split(r"[-_]", name)))


def parse_cfg(text):
    """Parse a cfg predicate (the part inside cfg(...)) into nested tuples:
    ("all"|"any", [preds]), ("not", pred), ("atom", key, value_or_None),
    ("const", bool) for the true / false literals."""
    tokens = re.findall(r'"[^"]*"|[A-Za-z_][A-Za-z0-9_]*|[(),=]', text)
    pos = 0

    def take(expected=None):
        nonlocal pos
        tok = tokens[pos]
        if expected is not None and tok != expected:
            raise ValueError(f"expected {expected!r}, got {tok!r} in {text!r}")
        pos += 1
        return tok

    def pred():
        name = take()
        if name in ("all", "any", "not") and pos < len(tokens) and tokens[pos] == "(":
            take("(")
            items = []
            while tokens[pos] != ")":
                items.append(pred())
                if tokens[pos] == ",":
                    take(",")
            take(")")
            return ("not", items[0]) if name == "not" else (name, items)
        if pos < len(tokens) and tokens[pos] == "=":
            take("=")
            return ("atom", name, take().strip('"'))
        if name in ("true", "false"):
            return ("const", name == "true")
        return ("atom", name, None)

    result = pred()
    if pos != len(tokens):
        raise ValueError(f"trailing tokens in {text!r}")
    return result


_TARGET_CFGS = None


def cargo_rustflags(triple):
    """The rustflags Cargo would use for `triple`: CARGO_ENCODED_RUSTFLAGS,
    else RUSTFLAGS, else build.rustflags / target.<triple>.rustflags from the
    Cargo config files (this directory and its parents, then CARGO_HOME)."""
    if os.environ.get("CARGO_ENCODED_RUSTFLAGS"):
        return [f for f in os.environ["CARGO_ENCODED_RUSTFLAGS"].split("\x1f") if f]
    if os.environ.get("RUSTFLAGS"):
        return os.environ["RUSTFLAGS"].split()
    dirs, cur = [], os.path.abspath(".")
    while True:
        dirs.append(cur)
        parent = os.path.dirname(cur)
        if parent == cur:
            break
        cur = parent
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    files = [os.path.join(d, ".cargo", n) for d in dirs for n in ("config.toml", "config")] + [
        os.path.join(home, n) for n in ("config.toml", "config")
    ]
    flags = []
    for path in files:
        if not os.path.isfile(path):
            continue
        with open(path, "rb") as f:
            conf = tomllib.load(f)
        for value in ((conf.get("target") or {}).get(triple, {}).get("rustflags"), (conf.get("build") or {}).get("rustflags")):
            if value:
                flags += value.split() if isinstance(value, str) else list(value)
    return flags


def target_cfgs():
    """[(is_apple, {(key, value_or_None), ...})] for every target rustc knows,
    with Cargo's rustflags applied. Cached under target/, keyed by the rustc
    version and the flags."""
    global _TARGET_CFGS
    if _TARGET_CFGS is not None:
        return _TARGET_CFGS
    version = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
    triples = subprocess.run(["rustc", "--print", "target-list"], check=True, capture_output=True, text=True).stdout.split()
    flags = {t: cargo_rustflags(t) for t in triples}
    key = hashlib.sha256((version + json.dumps(flags, sort_keys=True)).encode()).hexdigest()[:16]
    cache = os.path.join("target", f"lectern-target-cfgs-{key}.json")
    if os.path.exists(cache):
        with open(cache) as f:
            raw = json.load(f)
    else:
        def cfg_of(triple):
            out = subprocess.run(["rustc", *flags[triple], "--print", "cfg", "--target", triple], capture_output=True, text=True)
            return out.stdout.splitlines() if out.returncode == 0 else None

        with concurrent.futures.ThreadPoolExecutor(max_workers=os.cpu_count() or 4) as pool:
            raw = {t: lines for t, lines in zip(triples, pool.map(cfg_of, triples)) if lines is not None}
        os.makedirs("target", exist_ok=True)
        with open(cache, "w") as f:
            json.dump(raw, f)
    result = []
    for lines in raw.values():
        cfg = set()
        for line in lines:
            key, _, value = line.partition("=")
            cfg.add((key, value.strip('"') if value else None))
        result.append((("target_vendor", "apple") in cfg, cfg))
    _TARGET_CFGS = result
    return result


def apple_only_cfg(target, targets=None):
    """True when a dependency's target (a cfg(...) predicate or a target
    triple) holds on at least one Apple target and on no other target. Each
    predicate has one value per target: names a target does not set
    (feature, test, proc_macro, ...) are false, exactly as Cargo evaluates
    target-dependency tables."""
    if not target.startswith("cfg("):
        return "-apple-" in target
    targets = target_cfgs() if targets is None else targets
    tree = parse_cfg(target[4:-1])

    def holds(node, cfg):
        kind = node[0]
        if kind == "const":
            return node[1]
        if kind == "atom":
            return node[1:] in cfg
        if kind == "not":
            return not holds(node[1], cfg)
        results = (holds(child, cfg) for child in node[1])
        return all(results) if kind == "all" else any(results)

    on_apple = any(holds(tree, cfg) for apple, cfg in targets if apple)
    return on_apple and not any(holds(tree, cfg) for apple, cfg in targets if not apple)


def gpui_free_packages(meta):
    """Workspace crates that build, tests included, without any GPUI crate."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    free = []
    for pid in meta["workspace_members"]:
        seen, stack, uses_gpui = set(), [pid], False
        while stack and not uses_gpui:
            cur = stack.pop()
            # The crate's own dev-dependencies are compiled for its tests;
            # those of its dependencies are not.
            kinds = (None, "build", "dev") if cur == pid else (None, "build")
            for dep in nodes[cur]["deps"]:
                if dep["pkg"] in seen or not any(k["kind"] in kinds for k in dep["dep_kinds"]):
                    continue
                seen.add(dep["pkg"])
                if packages[dep["pkg"]]["name"].startswith(GPUI_PREFIXES):
                    uses_gpui = True
                    break
                stack.append(dep["pkg"])
        if not uses_gpui:
            free.append(packages[pid]["name"])
    return sorted(free)


def main():
    if "--gpui-free-packages" in sys.argv:
        # What the Linux job compiles: a GPUI dependency declared only for
        # macOS does not keep a crate out of Linux validation.
        print(" ".join(f"-p {name}" for name in gpui_free_packages(metadata(LINUX_TARGET))))
        return 0
    meta = metadata()
    packages = {p["id"]: p for p in meta["packages"]}
    members = set(meta["workspace_members"])
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    errors = []

    layer = {}
    for pid in members:
        pkg = packages[pid]
        value = (pkg.get("metadata") or {}).get("lectern", {}).get("layer")
        if value not in (*RANK, "kernel", "tool"):
            errors.append(f"{pkg['name']}: missing or unknown layer {value!r}")
        layer[pid] = value

    def normal_deps(pid):
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
                yield dep["pkg"]

    def in_release_build(pid, dep_name):
        """A normal or build dependency that is not optional is always compiled."""
        return any(
            d["name"] == dep_name and d["kind"] in (None, "build") and not d["optional"]
            for d in packages[pid]["dependencies"]
        )

    def apple_only(pid, dep_pkg):
        for dep in nodes[pid]["deps"]:
            if dep["pkg"] == dep_pkg:
                targets = [k["target"] for k in dep["dep_kinds"]]
                return all(t and apple_only_cfg(t) for t in targets)
        return False

    def own_deps(pid):
        """A workspace crate's direct dependencies, dev-dependencies included:
        they are compiled into its tests, examples and benchmarks. The
        dev-dependencies of its dependencies are never compiled."""
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] in (None, "build", "dev") for k in dep["dep_kinds"]):
                yield dep["pkg"]

    for pid in members:
        name, mine = packages[pid]["name"], layer[pid]
        for dep in own_deps(pid):
            dep_name = packages[dep]["name"]
            if dep in members:
                theirs = layer[dep]
                if theirs == "app":
                    errors.append(f"{name} -> {dep_name}: nothing may depend on the app crate")
                elif theirs == "tool" and mine != "app":
                    errors.append(f"{name} -> {dep_name}: only the app crate may depend on a tool crate")
                elif theirs == "tool" and in_release_build(pid, dep_name):
                    errors.append(
                        f"{name} -> {dep_name}: tool crates stay out of release builds; "
                        "use a dev-dependency or an optional, feature-gated dependency"
                    )
                elif mine in RANK and theirs in RANK and RANK[theirs] > RANK[mine]:
                    errors.append(f"{name} ({mine}) -> {dep_name} ({theirs}): dependencies must point downwards")
                elif mine == "kernel" and theirs not in ("kernel", None):
                    errors.append(f"{name} (kernel) -> {dep_name} ({theirs}): kernel crates depend only on kernel crates")
            # Workspace crates too: native code moved into another member must
            # not bypass platform isolation.
            if name != PLATFORM_CRATE and dep_name != PLATFORM_CRATE and (is_macos_binding_name(dep_name) or apple_only(pid, dep)):
                errors.append(f"{name} -> {dep_name}: macOS bindings belong in {PLATFORM_CRATE}")

    for pid in members:
        if layer[pid] not in GPUI_FREE_LAYERS:
            continue
        seen, stack = set(), [pid]
        while stack:
            cur = stack.pop()
            for dep in (own_deps if cur == pid else normal_deps)(cur):
                if dep in seen:
                    continue
                seen.add(dep)
                if packages[dep]["name"].startswith(GPUI_PREFIXES):
                    errors.append(
                        f"{packages[pid]['name']} ({layer[pid]}): depends on {packages[dep]['name']}; this layer must not use GPUI"
                    )
                    stack.clear()
                    break
                stack.append(dep)

    if errors:
        print("Architecture check failed:")
        for e in sorted(set(errors)):
            print(f"  - {e}")
        return 1
    print(f"Architecture check passed for {len(members)} crates.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
