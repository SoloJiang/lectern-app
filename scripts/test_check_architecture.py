"""Unit tests for check_architecture.py. Run: python3 scripts/test_check_architecture.py"""

import unittest

from check_architecture import TooComplex, apple_only_cfg, is_macos_binding_name


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
            'cfg(any(target_os = "macos", feature = "x"))',
            'cfg(any(target_os = "linux", false))',
            "cfg(true)",
            "cfg(false)",
        ):
            with self.subTest(target=target):
                self.assertFalse(apple_only_cfg(target))


    def test_too_complex_fails_closed(self):
        features = ", ".join(f'target_feature = "f{i}"' for i in range(20))
        with self.assertRaises(TooComplex):
            apple_only_cfg(f'cfg(all(target_os = "macos", any({features})))')


class BindingNames(unittest.TestCase):
    def test_names(self):
        for name in ("accesskit_macos", "raw-window-metal", "objc2-app-kit", "core-foundation-sys", "lectern-objc-bridge"):
            with self.subTest(name=name):
                self.assertTrue(is_macos_binding_name(name))
        for name in ("serde", "memchr", "gpui-pre", "lectern-platform", "lectern-review"):
            with self.subTest(name=name):
                self.assertFalse(is_macos_binding_name(name))


if __name__ == "__main__":
    unittest.main()
