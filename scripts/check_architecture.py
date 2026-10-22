#!/usr/bin/env python3
"""Check the architecture rules in AGENTS.md against `cargo metadata`.

Rules checked:
  1. Every workspace crate declares [package.metadata.lectern] layer.
  2. Dependencies point downwards: app -> view -> domain -> infra.
     kernel crates (kernel, services) may be used by any layer.
     tool crates may depend on anything; only app may depend on a tool crate,
     and only as a dev-dependency or an optional one outside `default`.
  3. Crates in the domain, infra and kernel layers never depend on GPUI,
     directly or transitively.
  4. macOS system bindings are direct dependencies of `platform` only. A
     binding is a crate whose name matches MACOS_ONLY_PREFIXES or contains a
     MACOS_NAME_TOKENS token, or any dependency whose
     target predicates can only hold on Apple platforms (apple_only_cfg).
  5. Every workspace crate inherits the workspace lints
     (`[lints] workspace = true`).
  6. Plugin crates (`[package.metadata.lectern] plugin = true`) never depend
     on another plugin crate, and only app and tool crates depend on a plugin
     crate (dev-dependencies included). A crate whose sources contain
     `impl Plugin for` must carry the marker.
  7. Only app and tool crates enable lectern-logging's `sink` feature, in a
     dependency's features or through one of their own features
     (docs/design/logging.md).
  8. `Value::panic_message` (any use of the name `panic_message` in code)
     appears only in the sources of crates/kernel, crates/executor and
     crates/logging (docs/design/logging.md「隐私」).

Each error names the rule's source (AGENTS.md or a design doc) in brackets.

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
import subprocess
import sys
import tomllib

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
# Exact names: these would be too broad as prefixes (block-buffer is not one).
MACOS_ONLY_NAMES = {"block", "block-sys"}
# Name tokens (split on - and _) that mark an Apple binding wherever they
# appear, e.g. accesskit_macos, raw-window-metal, objc2-app-kit.
MACOS_NAME_TOKENS = {"macos", "darwin", "apple", "appkit", "cocoa", "metal", "objc", "objc2", "coreaudio", "corefoundation"}
# A dependency whose target predicate holds on some Apple target and on no
# other target rustc knows is a macOS binding whatever its name, e.g.
# [target.'cfg(target_os = "macos")'.dependencies]. Predicates are evaluated
# against the real cfg of every target (rustc --print cfg), so aliases such as
# `unix` / target_family = "unix", multi-valued keys and Mac Catalyst are exact.
# Names no target sets (feature, test, proc_macro, ...) are false, as in Cargo:
# they cannot select target dependencies. Targets are taken with their default
# configuration, without local rustflags, so the check gives the same answer on
# every machine and in CI.
LINUX_TARGET = "x86_64-unknown-linux-gnu"  # the Linux CI runner
PLATFORM_CRATE = "lectern-platform"


def plugin_errors(crates):
    """Rule 7 (ADR 0025). `crates` maps a workspace crate's name to
    (is_plugin, layer, names of the workspace crates it depends on, dev
    included). Returns the error messages."""
    errors = []
    for name, (_, mine, deps) in crates.items():
        for dep in deps:
            if dep not in crates or not crates[dep][0]:
                continue
            if crates[name][0]:
                errors.append(f"{name} -> {dep}: plugin crates must not depend on each other [AGENTS.md architecture rule 7]")
            elif mine not in ("app", "tool"):
                errors.append(f"{name} -> {dep}: only the app and tool crates may depend on a plugin crate [AGENTS.md architecture rule 7]")
    return errors


_IMPL = re.compile(r"\bimpl\b")
_PLUGIN_FOR = re.compile(r"\s*(?:\w+::)*Plugin\s+for\b")


def has_impl_plugin(text):
    """Whether code text contains `impl Plugin for X`, `impl<T> kernel::Plugin
    for X` or `impl<T: Into<Vec<u8>>> lectern_kernel::Plugin for X`. Generic
    parameter lists are skipped with balanced angle brackets."""
    for m in _IMPL.finditer(text):
        i = m.end()
        while i < len(text) and text[i].isspace():
            i += 1
        if i < len(text) and text[i] == "<":
            depth = 0
            while i < len(text):
                if text[i] == "<":
                    depth += 1
                elif text[i] == ">" and text[i - 1] != "-":
                    depth -= 1
                    if depth == 0:
                        i += 1
                        break
                elif text[i] in "{;":
                    break
                i += 1
            if depth != 0:
                continue
        elif i == m.end():
            continue
        if _PLUGIN_FOR.match(text, i):
            return True
    return False


def strip_comments_and_strings(src, keep_strings=False):
    """Blank out comments, string literals and char literals so that text
    inside them (doc examples, commented-out code) is never read as code.
    With keep_strings, only comments are removed and literals are kept as
    written (cfg predicates such as `feature = "test-support"` need them)."""
    out, i, n = [], 0, len(src)
    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
        elif src.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif src.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            out.append(" ")
        elif (m := re.match(r'b?r(#*)"', src[i:])) is not None:
            close = '"' + m.group(1)
            j = src.find(close, i + m.end())
            start, i = i, n if j < 0 else j + len(close)
            out.append(src[start:i] if keep_strings else '""')
        elif c == '"' or src.startswith('b"', i):
            start = i
            i += 2 if c == "b" else 1
            while i < n and src[i] != '"':
                i += 2 if src[i] == "\\" else 1
            i += 1
            out.append(src[start:i] if keep_strings else '""')
        elif (m := re.match(r"b?'(?:\\.[^']*|[^'\\])'", src[i:])) is not None:
            out.append(m.group(0) if keep_strings else "' '")
            i += m.end()
        else:
            out.append(c)
            i += 1
    return "".join(out)


def _cfg_args(text):
    """Split a cfg argument list at top-level commas."""
    parts, depth, cur = [], 0, []
    for ch in text:
        if ch == "," and depth == 0:
            parts.append("".join(cur).strip())
            cur = []
            continue
        depth += {"(": 1, ")": -1}.get(ch, 0)
        cur.append(ch)
    if "".join(cur).strip():
        parts.append("".join(cur).strip())
    return parts


def test_only_cfg(pred):
    """Whether a cfg predicate can hold only in test builds: `test` or
    `feature = "test-support"`, combined with all/any. `not(...)` and every
    other atom also select production code."""
    pred = pred.strip()
    if pred == "test" or re.fullmatch(r'feature\s*=\s*"test-support"', pred):
        return True
    m = re.fullmatch(r"(all|any)\s*\((.*)\)", pred, re.S)
    if m is None:
        return False
    args = _cfg_args(m.group(2))
    if not args:
        return False
    check = any if m.group(1) == "all" else all
    return check(test_only_cfg(a) for a in args)


CFG_ATTR = re.compile(r"#\[cfg\((.*?)\)\]", re.S)


def strip_cfg_test(src):
    """Remove items gated on a test-only cfg: the attribute and the item after
    it, up to its `;` or its matching closing brace."""
    out, pos = [], 0
    for m in CFG_ATTR.finditer(src):
        if m.start() < pos or not test_only_cfg(m.group(1)):
            continue
        out.append(src[pos : m.start()])
        i = m.end()
        while i < len(src) and src[i] not in "{;":
            i += 1
        if i < len(src) and src[i] == "{":
            depth = 0
            while i < len(src):
                depth += {"{": 1, "}": -1}.get(src[i], 0)
                i += 1
                if depth == 0:
                    break
        else:
            i += 1
        pos = i
    out.append(src[pos:])
    return "".join(out)


def implements_plugin(src_dir):
    """Whether any non-test Rust source under src_dir implements
    `kernel::Plugin`. Test plugins live in a `tests` or `testing` directory,
    a `tests.rs` or `testing.rs` file, or an item gated on a test cfg, and
    are skipped."""
    for root, dirs, files in os.walk(src_dir):
        dirs[:] = [d for d in dirs if d not in ("tests", "testing")]
        for file in files:
            if file.endswith(".rs") and file not in ("tests.rs", "testing.rs"):
                with open(os.path.join(root, file), encoding="utf-8") as f:
                    code = strip_cfg_test(strip_comments_and_strings(f.read(), keep_strings=True))
                    if has_impl_plugin(strip_comments_and_strings(code)):
                        return True
    return False


LOGGING_CRATE = "lectern-logging"
# Crate directories under crates/ whose sources may call Value::panic_message.
PANIC_MESSAGE_DIRS = ("kernel", "executor", "logging")
_PANIC_MESSAGE = re.compile(r"\bpanic_message\b")


def enables_sink(pkg):
    """Whether a package (an entry of `cargo metadata`'s packages) turns on
    lectern-logging's `sink` feature: in the features of its dependency
    (any kind), or through one of its own features (`lectern-logging/sink`,
    `lectern-logging?/sink`, or the same under a renamed dependency)."""
    deps = [d for d in pkg.get("dependencies", []) if d["name"] == LOGGING_CRATE]
    if any("sink" in d.get("features", []) for d in deps):
        return True
    keys = {d.get("rename") or d["name"] for d in deps}
    wanted = {f"{k}/sink" for k in keys} | {f"{k}?/sink" for k in keys}
    return any(f in wanted for values in pkg.get("features", {}).values() for f in values)


def sink_feature_errors(packages, layers):
    """Rule 7. `packages` are workspace members' metadata entries, `layers`
    maps a crate's name to its layer."""
    return [
        f"{pkg['name']} ({layers.get(pkg['name'])}): enables lectern-logging's `sink` feature; "
        "only the app and tool crates may [docs/design/logging.md]"
        for pkg in packages
        if pkg["name"] != LOGGING_CRATE and layers.get(pkg["name"]) not in ("app", "tool") and enables_sink(pkg)
    ]


def uses_panic_message(src):
    """Whether Rust source uses the name `panic_message` outside comments and
    string literals."""
    return bool(_PANIC_MESSAGE.search(strip_comments_and_strings(src)))


def panic_message_errors(crate_dirs):
    """Rule 8. `crate_dirs` maps a workspace crate's name to its directory;
    every `.rs` file under it (src, tests, benches, examples, build.rs)
    counts, except for crates/kernel, crates/executor and crates/logging."""
    errors = []
    for name, crate_dir in sorted(crate_dirs.items()):
        if os.path.basename(os.path.normpath(crate_dir)) in PANIC_MESSAGE_DIRS:
            continue
        for root, dirs, files in os.walk(crate_dir):
            dirs[:] = sorted(d for d in dirs if d != "target")
            for file in sorted(files):
                if not file.endswith(".rs"):
                    continue
                path = os.path.join(root, file)
                with open(path, encoding="utf-8") as f:
                    if uses_panic_message(f.read()):
                        rel = os.path.relpath(path, os.path.dirname(os.path.dirname(crate_dir)))
                        errors.append(
                            f"{name}: {rel} uses Value::panic_message; only kernel, executor and logging may [docs/design/logging.md]"
                        )
    return errors


def metadata(platform=None, all_features=True):
    cmd = ["cargo", "metadata", "--format-version", "1", "--locked"] + (["--all-features"] if all_features else [])
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
    return name in MACOS_ONLY_NAMES or name.startswith(MACOS_ONLY_PREFIXES) or bool(MACOS_NAME_TOKENS & set(re.split(r"[-_]", name)))


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


def target_cfgs():
    """[(is_apple, {(key, value_or_None), ...})] for every target rustc knows,
    in its default configuration. Cached under target/ per rustc version."""
    global _TARGET_CFGS
    if _TARGET_CFGS is not None:
        return _TARGET_CFGS
    version = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
    triples = subprocess.run(["rustc", "--print", "target-list"], check=True, capture_output=True, text=True).stdout.split()
    key = hashlib.sha256(version.encode()).hexdigest()[:16]
    cache = os.path.join("target", f"lectern-target-cfgs-{key}.json")
    if os.path.exists(cache):
        with open(cache) as f:
            raw = json.load(f)
    else:
        def cfg_of(triple):
            # No RUSTFLAGS: rustc reads none from the environment, and Cargo's
            # are deliberately not applied (see the note above).
            out = subprocess.run(["rustc", "--print", "cfg", "--target", triple], capture_output=True, text=True)
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


def gpui_in_own_build(name, target="all"):
    """Whether building `name` on its own (tests included, all features) pulls
    in a GPUI crate. `cargo metadata` unifies features across the whole
    workspace, so a view crate enabling an optional GPUI feature of a shared
    dependency would make a domain crate look GPUI-dependent. `cargo tree -p`
    resolves features for that one package, so it is the authority whenever
    the workspace-wide graph shows only an indirect path."""
    out = subprocess.run(
        ["cargo", "tree", "--locked", "-p", name, "--all-features", "-e", "normal,build,dev",
         "--target", target, "--prefix", "none", "-f", "{p}"],
        check=True, capture_output=True, text=True,
    ).stdout
    return any(line.split(" ")[0].startswith(GPUI_PREFIXES) for line in out.splitlines() if line)


def gpui_free_packages(meta):
    """Workspace crates that build, tests included, without any GPUI crate."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    free = []
    for pid in meta["workspace_members"]:
        seen, stack, uses_gpui, direct = set(), [pid], False, False
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
                    uses_gpui, direct = True, cur == pid
                    break
                stack.append(dep["pkg"])
        if uses_gpui and not direct:
            uses_gpui = gpui_in_own_build(packages[pid]["name"], LINUX_TARGET)
        if not uses_gpui:
            free.append(packages[pid]["name"])
    return sorted(free)


def inherits_workspace_lints(manifest_path):
    """Whether the crate's Cargo.toml sets `[lints] workspace = true`."""
    with open(manifest_path, "rb") as f:
        return tomllib.load(f).get("lints", {}).get("workspace") is True


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
    plugin = {}
    for pid in members:
        pkg = packages[pid]
        lectern = (pkg.get("metadata") or {}).get("lectern", {})
        value = lectern.get("layer")
        if value not in (*RANK, "kernel", "tool"):
            errors.append(f"{pkg['name']}: missing or unknown layer {value!r} [docs/design/cargo-workspace.md]")
        plugin[pid] = lectern.get("plugin", False)
        if not isinstance(plugin[pid], bool):
            errors.append(f"{pkg['name']}: `plugin` must be true or false [docs/adr/0025.md]")
        elif plugin[pid] is False and pkg["name"] != "lectern-kernel" and implements_plugin(os.path.join(os.path.dirname(pkg["manifest_path"]), "src")):
            errors.append(f"{pkg['name']}: implements Plugin; add `plugin = true` to [package.metadata.lectern] [docs/adr/0025.md]")
        if not inherits_workspace_lints(pkg["manifest_path"]):
            errors.append(f"{pkg['name']}: add `[lints] workspace = true` to its Cargo.toml [docs/design/ci.md]")
        layer[pid] = value

    def normal_deps(pid):
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
                yield dep["pkg"]

    release = None

    def in_release_build(pid, dep_name):
        """Whether a plain `cargo build` (default features) compiles dep_name
        as a normal or build dependency of pid. Reads Cargo's own resolve, so
        optional dependencies switched on by `default` count too."""
        nonlocal release
        if release is None:
            meta = metadata(all_features=False)
            names = {p["id"]: p["name"] for p in meta["packages"]}
            release = {
                n["id"]: {names[d["pkg"]] for d in n["deps"] if any(k["kind"] in (None, "build") for k in d["dep_kinds"])}
                for n in meta["resolve"]["nodes"]
            }
        return dep_name in release.get(pid, ())

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
                    errors.append(f"{name} -> {dep_name}: nothing may depend on the app crate [docs/design/cargo-workspace.md]")
                elif theirs == "tool" and mine != "app":
                    errors.append(f"{name} -> {dep_name}: only the app crate may depend on a tool crate [docs/design/cargo-workspace.md]")
                elif theirs == "tool" and in_release_build(pid, dep_name):
                    errors.append(
                        f"{name} -> {dep_name}: tool crates stay out of release builds; "
                        "use a dev-dependency or an optional dependency outside the default features [docs/design/cargo-workspace.md]"
                    )
                elif mine in RANK and theirs in RANK and RANK[theirs] > RANK[mine]:
                    errors.append(f"{name} ({mine}) -> {dep_name} ({theirs}): dependencies must point downwards [AGENTS.md architecture rule 1]")
                elif mine == "kernel" and theirs not in ("kernel", None):
                    errors.append(f"{name} (kernel) -> {dep_name} ({theirs}): kernel crates depend only on kernel crates [docs/design/cargo-workspace.md]")
            # Workspace crates too: native code moved into another member must
            # not bypass platform isolation.
            if name != PLATFORM_CRATE and dep_name != PLATFORM_CRATE and (is_macos_binding_name(dep_name) or apple_only(pid, dep)):
                errors.append(f"{name} -> {dep_name}: macOS bindings belong in {PLATFORM_CRATE} [AGENTS.md architecture rule 6]")

    errors.extend(
        plugin_errors(
            {
                packages[pid]["name"]: (plugin[pid] is True, layer[pid], [packages[d]["name"] for d in own_deps(pid) if d in members])
                for pid in members
            }
        )
    )

    member_pkgs = [packages[pid] for pid in members]
    errors.extend(sink_feature_errors(member_pkgs, {packages[pid]["name"]: layer[pid] for pid in members}))
    errors.extend(panic_message_errors({p["name"]: os.path.dirname(p["manifest_path"]) for p in member_pkgs}))

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
                    # An indirect path may only come from workspace-wide
                    # feature unification; confirm it with this crate alone.
                    if cur == pid or gpui_in_own_build(packages[pid]["name"]):
                        errors.append(
                            f"{packages[pid]['name']} ({layer[pid]}): depends on {packages[dep]['name']}; this layer must not use GPUI [AGENTS.md architecture rule 2]"
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
