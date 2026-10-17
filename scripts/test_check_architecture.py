"""Unit tests for check_architecture.py. Run: python3 scripts/test_check_architecture.py"""

import json
import os
import tempfile
import unittest

import check_architecture
from check_architecture import IMPL_PLUGIN, apple_only_cfg, strip_cfg_test, inherits_workspace_lints, is_macos_binding_name, plugin_errors

FIXTURES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures")
_saved = {}


def setUpModule():
    # Every rustc call goes to the recording fake in fixtures/ (AGENTS.md: fake
    # commands first on PATH), and the target-cfg cache lands in a temp dir.
    _saved["cwd"] = os.getcwd()
    _saved["env"] = {k: os.environ.get(k) for k in ("PATH", "FAKE_RUSTC_LOG")}
    _saved["tmp"] = tempfile.TemporaryDirectory()
    os.environ["PATH"] = FIXTURES + os.pathsep + os.environ.get("PATH", "")
    os.environ["FAKE_RUSTC_LOG"] = os.path.join(_saved["tmp"].name, "rustc.log")
    os.chdir(_saved["tmp"].name)
    check_architecture._TARGET_CFGS = None


def tearDownModule():
    os.chdir(_saved["cwd"])
    for key, value in _saved["env"].items():
        if value is None:
            os.environ.pop(key, None)
        else:
            os.environ[key] = value
    _saved["tmp"].cleanup()
    check_architecture._TARGET_CFGS = None


def rustc_calls():
    with open(os.environ["FAKE_RUSTC_LOG"]) as f:
        return [json.loads(line)["args"] for line in f]


class PluginRule(unittest.TestCase):
    CRATES = {
        "lectern-app": (False, "app", ["lectern-project", "lectern-agents"]),
        "lectern-project": (True, "domain", ["lectern-git", "lectern-executor", "lectern-services"]),
        "lectern-agents": (True, "domain", ["lectern-git"]),
        "lectern-git": (False, "domain", []),
        "lectern-executor": (False, "infra", []),
        "lectern-services": (False, "kernel", []),
        "lectern-project-home": (False, "view", ["lectern-services", "lectern-git"]),
        "lectern-bench": (False, "tool", ["lectern-project", "lectern-agents"]),
    }

    def test_allowed(self):
        self.assertEqual(plugin_errors(self.CRATES), [])

    def test_plugin_depends_on_plugin(self):
        crates = dict(self.CRATES, **{"lectern-agents": (True, "domain", ["lectern-git", "lectern-project"])})
        self.assertEqual(
            plugin_errors(crates),
            ["lectern-agents -> lectern-project: plugin crates must not depend on each other [AGENTS.md architecture rule 7]"],
        )

    def test_library_depends_on_plugin(self):
        crates = dict(self.CRATES, **{"lectern-project-home": (False, "view", ["lectern-project"])})
        self.assertEqual(
            plugin_errors(crates),
            ["lectern-project-home -> lectern-project: only the app and tool crates may depend on a plugin crate [AGENTS.md architecture rule 7]"],
        )

    def test_cfg_test_items_are_ignored(self):
        src = """
pub struct Real;

#[cfg(test)]
mod tests {
    struct Fake;
    impl Plugin for Fake { fn id(&self) -> &str { "fake" } }
}

#[cfg(any(test, feature = "test-support"))]
impl kernel::Plugin for Mock {}

#[cfg(test)]
mod more;
"""
        self.assertFalse(IMPL_PLUGIN.search(strip_cfg_test(src)))
        self.assertIn("pub struct Real;", strip_cfg_test(src))
        self.assertTrue(IMPL_PLUGIN.search(strip_cfg_test(src + "impl Plugin for Real {}")))

    def test_impl_plugin_pattern(self):
        for src in ("impl Plugin for Projects {", "impl kernel::Plugin for X", "impl<T: Send> lectern_kernel::Plugin for Source<T>"):
            with self.subTest(src=src):
                self.assertTrue(IMPL_PLUGIN.search(src))
        for src in ("impl PluginSet {", "impl From<Plugin> for X", "// a Plugin for each source", "impl MyPlugin for X"):
            with self.subTest(src=src):
                self.assertFalse(IMPL_PLUGIN.search(src))


class AppleOnlyCfg(unittest.TestCase):
    def test_apple_only(self):
        for target in (
            'cfg(target_os = "macos")',
            'cfg(target_vendor = "apple")',
            'cfg(any(target_os = "macos", target_os = "ios"))',
            'cfg(all(target_os = "macos", target_arch = "aarch64"))',
            "aarch64-apple-darwin",
            'cfg(all(target_os = "macos", target_has_atomic = "8", target_has_atomic = "ptr"))',
            'cfg(all(target_os = "macos", target_feature = "neon", target_feature = "aes"))',
            'cfg(target_env = "macabi")',
            'cfg(target_abi = "sim")',
            'cfg(any(target_os = "macos", false))',
            'cfg(all(target_os = "macos", true))',
            'cfg(any(target_os = "macos", all(unix, not(target_family = "unix"))))',
            'cfg(any(target_os = "macos", feature = "metal"))',
            'cfg(any(target_os = "macos", test))',
        ):
            with self.subTest(target=target):
                self.assertTrue(apple_only_cfg(target))

    def test_portable(self):
        for target in (
            'cfg(not(target_os = "linux"))',
            'cfg(any(target_os = "macos", target_os = "android"))',
            'cfg(target_arch = "aarch64")',
            'cfg(all(unix, not(target_os = "linux")))',
            "cfg(windows)",
            "cfg(any())",
            'cfg(all(target_os = "macos", target_os = "linux"))',
            "x86_64-unknown-linux-gnu",
            'cfg(all(unix, target_family = "unix", target_family = "wasm"))',
            'cfg(all(target_os = "macos", feature = "metal"))',
            'cfg(any(target_os = "linux", false))',
            "cfg(true)",
            "cfg(false)",
            'cfg(target_family = "unix")',
            'cfg(any(target_os = "macos", target_os = "android"))',
        ):
            with self.subTest(target=target):
                self.assertFalse(apple_only_cfg(target))




class TargetConfigurations(unittest.TestCase):
    def test_target_cfgs_come_from_the_fake_rustc(self):
        check_architecture._TARGET_CFGS = None
        targets = check_architecture.target_cfgs()
        with open(os.path.join(FIXTURES, "rustc.json")) as f:
            fixture = json.load(f)
        # The unknown triple fails in the fake and is skipped.
        self.assertEqual(len(targets), len(fixture["targets"]))
        self.assertEqual(sum(apple for apple, _ in targets), 5)
        calls = rustc_calls()
        self.assertIn(["-vV"], calls)
        self.assertIn(["--print", "target-list"], calls)
        self.assertIn(["--print", "cfg", "--target", "unknown-fixture-target"], calls)

    def test_a_non_apple_witness_makes_predicate_portable(self):
        predicate = 'cfg(any(target_os = "macos", target_feature = "avx512f"))'
        self.assertTrue(apple_only_cfg(predicate, [(True, {("target_os", "macos")}), (False, {("target_os", "linux")})]))
        self.assertFalse(
            apple_only_cfg(
                predicate,
                [(True, {("target_os", "macos")}), (False, {("target_os", "linux"), ("target_feature", "avx512f")})],
            )
        )


class BindingNames(unittest.TestCase):
    def test_names(self):
        for name in ("accesskit_macos", "raw-window-metal", "objc2-app-kit", "core-foundation-sys", "lectern-objc-bridge", "block", "block-sys"):
            with self.subTest(name=name):
                self.assertTrue(is_macos_binding_name(name))
        for name in ("serde", "memchr", "gpui-pre", "lectern-platform", "lectern-review", "block-buffer"):
            with self.subTest(name=name):
                self.assertFalse(is_macos_binding_name(name))


class WorkspaceLints(unittest.TestCase):
    def test_manifests(self):
        cases = {
            '[package]\nname = "a"\n\n[lints]\nworkspace = true\n': True,
            '[package]\nname = "a"\n': False,
            '[package]\nname = "a"\n\n[lints.clippy]\ndbg_macro = "warn"\n': False,
            '[package]\nname = "a"\n\n[lints]\nworkspace = false\n': False,
        }
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "Cargo.toml")
            for text, expected in cases.items():
                with self.subTest(text=text):
                    with open(path, "w") as f:
                        f.write(text)
                    self.assertIs(inherits_workspace_lints(path), expected)


if __name__ == "__main__":
    unittest.main()
