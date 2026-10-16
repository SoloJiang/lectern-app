#!/usr/bin/env python3
"""Check the architecture rules in AGENTS.md against `cargo metadata`.

Rules checked:
  1. Every workspace crate declares [package.metadata.lectern] layer.
  2. Dependencies point downwards: app -> view -> domain -> infra.
     kernel crates (kernel, services) may be used by any layer.
     tool crates may depend on anything; only app may depend on a tool crate.
  3. Crates in the domain, infra and kernel layers never depend on GPUI,
     directly or transitively.
  4. macOS system bindings are direct dependencies of `platform` only.

Exit status is 1 when any rule is broken.

With --gpui-free-packages, print `-p <crate>` for every crate that builds
without GPUI instead (used by the Linux CI job).
"""

import json
import subprocess
import sys

RANK = {"app": 4, "view": 3, "domain": 2, "infra": 1}
GPUI_FREE_LAYERS = {"domain", "infra", "kernel"}
GPUI_PREFIXES = ("gpui",)
MACOS_ONLY = {
    "objc",
    "objc2",
    "objc2-foundation",
    "objc2-app-kit",
    "cocoa",
    "cocoa-foundation",
    "core-foundation",
    "core-foundation-sys",
    "core-graphics",
    "core-text",
    "fsevent",
    "fsevent-sys",
    "security-framework",
    "metal",
}
PLATFORM_CRATE = "lectern-platform"


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def gpui_free_packages(meta):
    """Workspace crates whose normal dependency graph contains no GPUI crate."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    free = []
    for pid in meta["workspace_members"]:
        seen, stack, uses_gpui = set(), [pid], False
        while stack and not uses_gpui:
            for dep in nodes[stack.pop()]["deps"]:
                if dep["pkg"] in seen or not any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
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
    meta = metadata()
    if "--gpui-free-packages" in sys.argv:
        print(" ".join(f"-p {name}" for name in gpui_free_packages(meta)))
        return 0
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

    for pid in members:
        name, mine = packages[pid]["name"], layer[pid]
        for dep in normal_deps(pid):
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
            elif dep_name in MACOS_ONLY and packages[pid]["name"] != PLATFORM_CRATE:
                errors.append(f"{name} -> {dep_name}: macOS bindings belong in {PLATFORM_CRATE}")

    for pid in members:
        if layer[pid] not in GPUI_FREE_LAYERS:
            continue
        seen, stack = set(), [pid]
        while stack:
            cur = stack.pop()
            for dep in normal_deps(cur):
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
