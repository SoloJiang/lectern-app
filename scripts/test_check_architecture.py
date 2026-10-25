"""Unit tests for check_architecture.py. Run: python3 scripts/test_check_architecture.py"""

import json
import os
import tempfile
import unittest

import check_architecture
from check_architecture import has_impl_plugin, implements_plugin, missing_plugin_mark, apple_only_cfg, strip_cfg_test, strip_comments_and_strings, test_only_cfg, inherits_workspace_lints, is_macos_binding_name, plugin_errors
from check_architecture import enables_sink, panic_message_errors, sink_feature_errors, uses_panic_message
from check_architecture import print_lint_allow_errors, workspace_print_lint_errors
from check_architecture import hooks_enablers, hooks_feature_errors

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
        self.assertFalse(has_impl_plugin(strip_cfg_test(src)))
        self.assertIn("pub struct Real;", strip_cfg_test(src))
        self.assertTrue(has_impl_plugin(strip_cfg_test(src + "impl Plugin for Real {}")))

    def test_test_only_cfg(self):
        for pred in ("test", 'feature = "test-support"', 'any(test, feature = "test-support")', 'all(test, unix)'):
            with self.subTest(pred=pred):
                self.assertTrue(test_only_cfg(pred))
        for pred in ("not(test)", 'any(test, target_os = "macos")', "unix", 'feature = "metal"', "any()"):
            with self.subTest(pred=pred):
                self.assertFalse(test_only_cfg(pred))

    def test_production_cfg_is_kept(self):
        src = "#[cfg(not(test))]\nimpl Plugin for Real {}\n"
        self.assertTrue(has_impl_plugin(strip_cfg_test(src)))

    def test_comments_and_strings_are_ignored(self):
        src = """
/// ```
/// impl Plugin for Example {}
/// ```
// impl Plugin for Old {}
/* impl Plugin for Block { /* nested */ } */
const DOC: &str = "impl Plugin for Text";
const RAW: &str = r#"impl Plugin for Raw "quoted""#;
fn f<'a>(c: char) -> bool { c == '"' }
"""
        cleaned = strip_comments_and_strings(src)
        self.assertFalse(has_impl_plugin(cleaned))
        self.assertIn("fn f<'a>(c: char)", cleaned)
        self.assertTrue(has_impl_plugin(strip_comments_and_strings(src + "impl Plugin for Real {}")))

    def test_feature_gated_test_double_in_a_file(self):
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "lib.rs"), "w") as f:
                f.write('#[cfg(feature = "test-support")]\nimpl Plugin for Mock {}\n'
                        '#[cfg(any(test, feature = "test-support"))]\nimpl kernel::Plugin for Fake {}\n')
            self.assertFalse(implements_plugin(d))
            with open(os.path.join(d, "real.rs"), "w") as f:
                f.write('const S: &str = "x";\nimpl Plugin for Real {}\n')
            self.assertTrue(implements_plugin(d))

    def test_tool_crates_need_no_plugin_mark(self):
        # ADR 0051: a tool crate's plugin (bench's `bench-probe`) does not
        # make it a plugin crate; any other layer still needs the mark.
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "probe.rs"), "w") as f:
                f.write("impl Plugin for ProbePlugin {}\n")
            self.assertTrue(missing_plugin_mark("lectern-review", "domain", d))
            self.assertTrue(missing_plugin_mark("lectern-app", "app", d))
            self.assertFalse(missing_plugin_mark("lectern-bench", "tool", d))
            self.assertFalse(missing_plugin_mark("lectern-kernel", "kernel", d))
        with tempfile.TemporaryDirectory() as d:
            self.assertFalse(missing_plugin_mark("lectern-review", "domain", d))

    def test_impl_plugin_pattern(self):
        for src in ("impl Plugin for Projects {", "impl kernel::Plugin for X", "impl<T: Send> lectern_kernel::Plugin for Source<T>",
                    "impl<T: Into<Vec<u8>>> Plugin for Adapter<T>", "impl<F: Fn() -> u8> kernel::Plugin for Hook<F>"):
            with self.subTest(src=src):
                self.assertTrue(has_impl_plugin(src))
        for src in ("impl PluginSet {", "impl From<Plugin> for X", "// a Plugin for each source", "impl MyPlugin for X"):
            with self.subTest(src=src):
                self.assertFalse(has_impl_plugin(src))


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


def package(name, deps=(), features=None):
    """A `cargo metadata` package entry: deps are (name, features, rename)."""
    return {
        "name": name,
        "dependencies": [{"name": n, "features": list(f), "rename": r, "kind": None} for n, f, r in deps],
        "features": features or {},
    }


class LoggingSinkRule(unittest.TestCase):
    LAYERS = {
        "lectern-app": "app",
        "lectern-bench": "tool",
        "lectern-cache": "infra",
        "lectern-kernel": "kernel",
        "lectern-review-view": "view",
        "lectern-logging": "kernel",
    }

    def test_enables_sink(self):
        self.assertTrue(enables_sink(package("a", [("lectern-logging", ["sink"], None)])))
        self.assertTrue(enables_sink(package("a", [("lectern-logging", [], None)], {"x": ["lectern-logging/sink"]})))
        self.assertTrue(enables_sink(package("a", [("lectern-logging", [], None)], {"x": ["lectern-logging?/sink"]})))
        self.assertTrue(enables_sink(package("a", [("lectern-logging", [], "logging")], {"x": ["logging/sink"]})))
        self.assertFalse(enables_sink(package("a", [("lectern-logging", ["test-support"], None)])))
        self.assertFalse(enables_sink(package("a", [("other", ["sink"], None)], {"sink": ["other/sink"]})))
        self.assertFalse(enables_sink(package("a", [("lectern-logging", [], None)], {"sink": []})))

    def test_only_app_and_tool_crates(self):
        packages = [
            package("lectern-app", [("lectern-logging", ["sink"], None)]),
            package("lectern-bench", [("lectern-logging", ["sink", "test-support"], None)]),
            package("lectern-logging", features={"sink": [], "test-support": []}),
            package("lectern-kernel", [("lectern-logging", [], None)]),
            package("lectern-cache", [("lectern-logging", ["test-support"], None)]),
        ]
        self.assertEqual(sink_feature_errors(packages, self.LAYERS), [])
        packages += [
            package("lectern-cache", [("lectern-logging", ["sink"], None)]),
            package("lectern-review-view", [("lectern-logging", [], None)], {"disk": ["lectern-logging/sink"]}),
        ]
        self.assertEqual(
            sink_feature_errors(packages, self.LAYERS),
            [
                "lectern-cache (infra): enables lectern-logging's `sink` feature; only the app and tool crates may [docs/design/logging.md]",
                "lectern-review-view (view): enables lectern-logging's `sink` feature; only the app and tool crates may [docs/design/logging.md]",
            ],
        )


class BenchHooksRule(unittest.TestCase):
    SOURCE = "[docs/design/platform.md「测量构建的方法替换」]"
    OTHER = "only lectern-bench's `probe` feature may"
    BENCH = "it may do so only through its `probe` feature"

    def test_hooks_enablers(self):
        platform = "lectern-platform"
        self.assertEqual(hooks_enablers(package("a", [(platform, ["bench-hooks"], None)])), ["dependency"])
        self.assertEqual(hooks_enablers(package("a", [(platform, [], None)], {"x": [f"{platform}/bench-hooks"]})), ["feature `x`"])
        self.assertEqual(hooks_enablers(package("a", [(platform, [], None)], {"x": [f"{platform}?/bench-hooks"]})), ["feature `x`"])
        self.assertEqual(hooks_enablers(package("a", [(platform, [], "plat")], {"x": ["plat/bench-hooks"]})), ["feature `x`"])
        self.assertEqual(
            hooks_enablers(package("a", [(platform, ["bench-hooks"], None)], {"y": [f"{platform}/bench-hooks"], "x": [f"{platform}/bench-hooks"]})),
            ["dependency", "feature `x`", "feature `y`"],
        )
        self.assertEqual(hooks_enablers(package("a", [(platform, ["other"], None)], {"x": [f"{platform}/other"]})), [])
        self.assertEqual(hooks_enablers(package("a", [("other", ["bench-hooks"], None)], {"x": ["other/bench-hooks"]})), [])
        # A name in the package's own feature list does not count, and a
        # package without the dependency never enables the feature.
        self.assertEqual(hooks_enablers(package("a", [], {"x": ["lectern-platform/bench-hooks"]})), [])
        self.assertEqual(hooks_enablers(package("a")), [])

    def test_probe_of_bench_is_the_one_way(self):
        packages = [
            package(
                "lectern-bench",
                [("lectern-platform", [], None), ("lectern-kernel", [], None)],
                {"probe": ["dep:lectern-kernel", "dep:lectern-platform", "lectern-platform/bench-hooks"], "other": []},
            ),
            package("lectern-platform", features={"bench-hooks": [], "default": []}),
            package("lectern-app", [("lectern-platform", ["test-support"], None)], {"bench-probe": ["lectern-bench/probe"]}),
        ]
        self.assertEqual(hooks_feature_errors(packages), [])
        # The optional form of the same.
        packages[0]["features"]["probe"] = ["lectern-platform?/bench-hooks"]
        self.assertEqual(hooks_feature_errors(packages), [])

    def test_every_other_way_is_an_error(self):
        platform = "lectern-platform"
        packages = [
            # Another crate: in the dependency's features, and through a feature.
            package("lectern-app", [(platform, ["bench-hooks"], None)]),
            package("lectern-review-view", [(platform, [], None)], {"disk": [f"{platform}/bench-hooks"]}),
            # lectern-bench: in the dependency's features, and through a
            # feature that is not `probe` (alone, and next to `probe`).
            package("lectern-bench", [(platform, ["bench-hooks"], None)], {"probe": [f"{platform}/bench-hooks"], "all": [f"{platform}?/bench-hooks"]}),
            # lectern-platform: in `default`.
            package("lectern-platform", features={"bench-hooks": [], "default": ["bench-hooks"]}),
        ]
        self.assertEqual(
            hooks_feature_errors(packages),
            [
                f"lectern-app: dependency enables lectern-platform's `bench-hooks` feature; {self.OTHER} {self.SOURCE}",
                f"lectern-review-view: feature `disk` enables lectern-platform's `bench-hooks` feature; {self.OTHER} {self.SOURCE}",
                f"lectern-bench: dependency enables lectern-platform's `bench-hooks` feature; {self.BENCH} {self.SOURCE}",
                f"lectern-bench: feature `all` enables lectern-platform's `bench-hooks` feature; {self.BENCH} {self.SOURCE}",
                f"lectern-platform: `bench-hooks` is in its `default` features; it must stay off {self.SOURCE}",
            ],
        )

    def test_a_workspace_without_the_probe_yet_passes(self):
        # Before bench's step 4, lectern-bench has no `probe` feature and
        # lectern-platform's `bench-hooks` is enabled by nobody.
        packages = [
            package("lectern-bench", [("lectern-platform", [], None)], {}),
            package("lectern-platform", features={"bench-hooks": []}),
        ]
        self.assertEqual(hooks_feature_errors(packages), [])


class PanicMessageRule(unittest.TestCase):
    def test_uses_panic_message(self):
        self.assertTrue(uses_panic_message("let v = Value::panic_message(text);"))
        self.assertTrue(uses_panic_message("use lectern_logging::Value; fn f() { Value::panic_message }"))
        self.assertFalse(uses_panic_message('// Value::panic_message\nconst S: &str = "panic_message";'))
        self.assertFalse(uses_panic_message("let panic_messages = 1; fn my_panic_message_count() {}"))

    def test_only_kernel_executor_and_logging(self):
        with tempfile.TemporaryDirectory() as root:
            dirs = {}
            for name in ("kernel", "executor", "logging", "cache", "agents"):
                d = os.path.join(root, "crates", name)
                os.makedirs(os.path.join(d, "src"))
                os.makedirs(os.path.join(d, "tests"))
                with open(os.path.join(d, "src", "lib.rs"), "w") as f:
                    f.write("fn f(t: PanicText) -> Value { Value::panic_message(t) }\n")
                dirs[f"lectern-{name}"] = d
            with open(os.path.join(dirs["lectern-agents"], "src", "lib.rs"), "w") as f:
                f.write("// Value::panic_message is for kernel and executor only.\n")
            with open(os.path.join(dirs["lectern-agents"], "tests", "t.rs"), "w") as f:
                f.write("#[test] fn t() { let _ = lectern_logging::Value::panic_message; }\n")
            self.assertEqual(
                panic_message_errors(dirs),
                [
                    "lectern-agents: crates/agents/tests/t.rs uses Value::panic_message; only kernel, executor and logging may [docs/design/logging.md]",
                    "lectern-cache: crates/cache/src/lib.rs uses Value::panic_message; only kernel, executor and logging may [docs/design/logging.md]",
                ],
            )


class PrintLintRule(unittest.TestCase):
    def test_workspace_sets_both_lints(self):
        cases = {
            '[workspace.lints.clippy]\nprint_stderr = "warn"\nprint_stdout = "deny"\n': [],
            '[workspace.lints.clippy]\nprint_stderr = { level = "warn", priority = 1 }\nprint_stdout = "forbid"\n': [],
            '[workspace.lints.clippy]\nprint_stderr = "allow"\n': [
                'workspace: set `print_stderr = "warn"` in [workspace.lints.clippy] [docs/design/logging.md]',
                'workspace: set `print_stdout = "warn"` in [workspace.lints.clippy] [docs/design/logging.md]',
            ],
            '[workspace]\nresolver = "3"\n\n[workspace.lints.clippy]\nprint_stdout = "warn"\n': [
                'workspace: set `print_stderr = "warn"` in [workspace.lints.clippy] [docs/design/logging.md]',
            ],
        }
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "Cargo.toml")
            for text, expected in cases.items():
                with self.subTest(text=text):
                    with open(path, "w") as f:
                        f.write(text)
                    self.assertEqual(workspace_print_lint_errors(path), expected)

    def test_only_tool_crates_and_build_scripts_allow_them(self):
        allow = '#![allow(clippy::print_stderr, reason = "prints")]\nfn main() { eprintln!("x"); }\n'
        with tempfile.TemporaryDirectory() as root:
            dirs, layers = {}, {"lectern-bench": "tool", "lectern-i18n": "infra", "lectern-cache": "infra", "lectern-welcome": "view"}
            for name in ("bench", "i18n", "cache", "welcome"):
                d = os.path.join(root, "crates", name)
                os.makedirs(os.path.join(d, "src"))
                with open(os.path.join(d, "src", "lib.rs"), "w") as f:
                    f.write("// clippy::print_stdout is on for this crate.\nconst S: &str = \"print_stdout\";\n")
                dirs[f"lectern-{name}"] = d
            with open(os.path.join(dirs["lectern-bench"], "src", "lib.rs"), "w") as f:
                f.write(allow)
            with open(os.path.join(dirs["lectern-i18n"], "build.rs"), "w") as f:
                f.write(allow)
            self.assertEqual(print_lint_allow_errors(dirs, layers), [])
            os.makedirs(os.path.join(dirs["lectern-cache"], "src", "build"))
            with open(os.path.join(dirs["lectern-cache"], "src", "build", "build.rs"), "w") as f:
                f.write(allow)
            with open(os.path.join(dirs["lectern-welcome"], "src", "lib.rs"), "w") as f:
                f.write("#[expect(clippy::print_stdout, reason = \"x\")]\nfn f() { println!(); }\n")
            self.assertEqual(
                print_lint_allow_errors(dirs, layers),
                [
                    "lectern-cache: crates/cache/src/build/build.rs allows clippy::print_stderr or print_stdout; "
                    "log through lectern-logging's macros [docs/design/logging.md]",
                    "lectern-welcome: crates/welcome/src/lib.rs allows clippy::print_stderr or print_stdout; "
                    "log through lectern-logging's macros [docs/design/logging.md]",
                ],
            )


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
