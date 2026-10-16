"""Unit tests for check_docs.py. Run: python3 scripts/test_check_docs.py"""

import os
import tempfile
import unittest

from check_docs import check, headings, slug, unique_slugs


class Slug(unittest.TestCase):
    def test_github_rules(self):
        cases = {
            "性能预算": "性能预算",
            "对 Lectern 的建议": "对-lectern-的建议",
            "交互规范「动效」": "交互规范动效",
            "`kernel` 设计 (v2)": "kernel-设计-v2",
            "Build and test (macOS arm64)": "build-and-test-macos-arm64",
            "[链接](x.md) 标题": "链接-标题",
        }
        for heading, expected in cases.items():
            with self.subTest(heading=heading):
                self.assertEqual(slug(heading), expected)


class Headings(unittest.TestCase):
    def test_atx_and_setext(self):
        lines = ["# One", "", "Two", "===", "", "Three", "---", "", "- [ ] item", "", "| a |", "|---|"]
        self.assertEqual(headings(lines), ["One", "Two", "Three"])

    def test_inline_code_kept_for_slug(self):
        self.assertEqual(unique_slugs(headings(["## `kernel` 设计"])), ["kernel-设计"])

    def test_nested_fence(self):
        lines = ["````md", "```", "# Not a heading", "```", "````", "# Real"]
        self.assertEqual(headings(lines), ["Real"])

    def test_unique_slugs(self):
        self.assertEqual(unique_slugs(["Foo", "Foo-1", "Foo"]), ["foo", "foo-1", "foo-2"])
        self.assertEqual(unique_slugs(["A", "A", "A"]), ["a", "a-1", "a-2"])


class Check(unittest.TestCase):
    def run_check(self, files):
        with tempfile.TemporaryDirectory() as root:
            for rel, text in files.items():
                path = os.path.join(root, rel)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, "w", encoding="utf-8") as f:
                    f.write(text)
            return check(root, list(files))

    def test_valid_links(self):
        errors = self.run_check({
            "a.md": "# 标题\n\n## 二级 标题\n\n## 二级 标题\n",
            "docs/b.md": "[a](../a.md) [x](../a.md#二级-标题-1) [self](#c) [web](https://x.y/z)\n\n## C\n",
        })
        self.assertEqual(errors, [])

    def test_broken_links(self):
        errors = self.run_check({
            "a.md": "# 标题\n",
            "b.md": "[missing](nope.md)\n[anchor](a.md#没有)\n",
        })
        self.assertEqual(len(errors), 2)
        self.assertIn("file not found", errors[0])
        self.assertIn("#没有", errors[1])

    def test_reference_definitions(self):
        errors = self.run_check({
            "a.md": "# 标题\n",
            "b.md": "[ok][x] [bad][y]\n\n[x]: a.md#标题\n[y]: missing.md\n",
        })
        self.assertEqual(len(errors), 1)
        self.assertIn("missing.md", errors[0])

    def test_percent_encoded(self):
        errors = self.run_check({
            "design notes.md": "# 性能预算\n",
            "b.md": "[x](design%20notes.md#%E6%80%A7%E8%83%BD%E9%A2%84%E7%AE%97)\n",
        })
        self.assertEqual(errors, [])

    def test_markdown_edge_cases(self):
        errors = self.run_check({
            "my guide.md": "# Performance Budget\n",
            "b.md": (
                "<!-- [retired](deleted.md) -->\n"
                "<!--\n[also retired](gone.md)\n-->\n"
                "[angle](<my guide.md>)\n"
                "`` `[x](missing.md)` ``\n"
                "Text.[^1]\n\n[^1]: This is a note.\n"
            ),
        })
        self.assertEqual(errors, [])

    def test_fragment_case(self):
        errors = self.run_check({
            "a.md": "# Performance Budget\n",
            "b.md": "[ok](a.md#performance-budget) [bad](a.md#Performance-Budget)\n",
        })
        self.assertEqual(len(errors), 1)
        self.assertIn("#Performance-Budget", errors[0])

    def test_code_is_ignored(self):
        errors = self.run_check({
            "a.md": "`[x](nope.md)`\n\n```\n[y](nope.md)\n```\n",
        })
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
