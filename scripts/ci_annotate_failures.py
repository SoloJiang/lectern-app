#!/usr/bin/env python3
"""Turn failed tests in nextest's JUnit report into GitHub annotations.

Annotations are readable through the check-runs API, so a failing test's
name and output can be seen without downloading the job log.

Usage: ci_annotate_failures.py [path/to/junit.xml]
"""

import sys
import xml.etree.ElementTree as ET
from pathlib import Path

DEFAULT_REPORT = Path("target/nextest/ci/junit.xml")
# GitHub keeps at most 10 error annotations per step.
MAX_ANNOTATIONS = 10
MAX_LINES = 40


def escape(text: str) -> str:
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def escape_property(text: str) -> str:
    return escape(text).replace(":", "%3A").replace(",", "%2C")


def main() -> int:
    report = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_REPORT
    if not report.is_file():
        print(f"::warning::no JUnit report at {report}")
        return 0
    failed = []
    for case in ET.parse(report).getroot().iter("testcase"):
        for kind in ("failure", "error"):
            node = case.find(kind)
            if node is None:
                continue
            output = node.text or ""
            for stream in ("system-out", "system-err"):
                extra = case.find(stream)
                if extra is not None and extra.text:
                    output += "\n" + extra.text
            name = f"{case.get('classname', '')} {case.get('name', '')}".strip()
            failed.append((name, node.get("message") or kind, output))
            break
    for name, message, output in failed[:MAX_ANNOTATIONS]:
        lines = output.strip().splitlines()
        if len(lines) > MAX_LINES:
            lines = lines[:10] + ["…"] + lines[-(MAX_LINES - 11):]
        body = "\n".join([message, *lines])
        print(f"::error title={escape_property(name)}::{escape(body)}")
    if len(failed) > MAX_ANNOTATIONS:
        rest = ", ".join(name for name, _, _ in failed[MAX_ANNOTATIONS:])
        print(f"::error title=more failed tests::{escape(rest)}")
    print(f"{len(failed)} failed test(s) in {report}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
