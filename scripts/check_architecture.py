#!/usr/bin/env python3
"""Check the architecture rules in AGENTS.md against `cargo metadata`.

Rules checked:
  1. Every workspace crate declares [package.metadata.lectern] layer.
  2. Dependencies point downwards: app -> view -> domain -> infra.
     kernel crates (kernel, services) may be used by any layer.
     tool crates may depend on anything; only app may depend on a tool crate.
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

import json
import re
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
# A dependency whose target predicates can only hold on Apple platforms is a
# macOS binding whatever its name, e.g.
# [target.'cfg(target_os = "macos")'.dependencies]. The predicate is parsed and
# checked for satisfiability with every Apple OS ruled out (see apple_only_cfg).
APPLE_OSES = {"macos", "ios", "tvos", "watchos", "visionos"}
# cfg keys that hold exactly one value per target. Every other key (feature,
# target_feature, target_family, target_has_atomic, ...) can hold several, so
# each of its key = "value" pairs is an independent flag.
SINGLE_VALUED_CFG_KEYS = {
    "target_os", "target_vendor", "target_arch", "target_env", "target_abi",
    "target_endian", "target_pointer_width", "panic",
}
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
    ("all"|"any", [preds]), ("not", pred), ("atom", key, value_or_None)."""
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
        return ("atom", name, None)

    result = pred()
    if pos != len(tokens):
        raise ValueError(f"trailing tokens in {text!r}")
    return result


def apple_only_cfg(target):
    """True when a dependency's target (a cfg(...) predicate or a target
    triple) can only be true on Apple platforms: the predicate is
    unsatisfiable once target_os is not an Apple OS and target_vendor is not
    "apple". Every other atom is left free, so unrelated conditions (arch,
    family, features) never make a portable dependency look Apple-only."""
    if not target.startswith("cfg("):
        return "-apple-" in target
    tree = parse_cfg(target[4:-1])
    atoms = set()

    def collect(node):
        if node[0] == "atom":
            atoms.add(node[1:])
        else:
            for child in node[1] if node[0] in ("all", "any") else [node[1]]:
                collect(child)

    collect(tree)
    keyed = {}
    for key, value in atoms:
        if value is not None and key in SINGLE_VALUED_CFG_KEYS:
            keyed.setdefault(key, set()).add(value)
    full = {key: sorted(values) + [None] for key, values in keyed.items()}
    flags = sorted({(key, value) for key, value in atoms if value is None or key not in SINGLE_VALUED_CFG_KEYS}, key=str)
    keys = sorted(full)
    if len(flags) > 10 or any(len(d) > 16 for d in full.values()):
        return False  # too large to enumerate; treat as portable

    def holds(node, env):
        kind = node[0]
        if kind == "atom":
            key, value = node[1], node[2]
            if key in SINGLE_VALUED_CFG_KEYS and value is not None:
                return env[key] == value
            return env[(key, value)]
        if kind == "not":
            return not holds(node[1], env)
        results = (holds(child, env) for child in node[1])
        return all(results) if kind == "all" else any(results)

    def satisfiable(domains):
        def assignments(i, env):
            if i == len(keys):
                for bits in range(1 << len(flags)):
                    yield {**env, **{f: bool(bits >> j & 1) for j, f in enumerate(flags)}}
                return
            for value in domains[keys[i]]:
                yield from assignments(i + 1, {**env, keys[i]: value})

        return any(holds(tree, env) for env in assignments(0, {}))

    banned = {"target_os": APPLE_OSES, "target_vendor": {"apple"}}
    non_apple = {key: [v for v in values if v not in banned.get(key, ())] for key, values in full.items()}
    # Compiled somewhere (e.g. not cfg(any())) and nowhere outside Apple.
    return satisfiable(full) and not satisfiable(non_apple)


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
                elif mine in RANK and theirs in RANK and RANK[theirs] > RANK[mine]:
                    errors.append(f"{name} ({mine}) -> {dep_name} ({theirs}): dependencies must point downwards")
                elif mine == "kernel" and theirs not in ("kernel", None):
                    errors.append(f"{name} (kernel) -> {dep_name} ({theirs}): kernel crates depend only on kernel crates")
            elif (is_macos_binding_name(dep_name) or apple_only(pid, dep)) and name != PLATFORM_CRATE:
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
