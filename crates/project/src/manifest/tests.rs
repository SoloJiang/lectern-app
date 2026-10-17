use super::*;

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/manifests/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Every fixture with the kind it is parsed as.
const FIXTURES: &[(ManifestKind, &str)] = &[
    (
        ManifestKind::PackageJson,
        "package_json/npm_workspaces.json",
    ),
    (ManifestKind::PackageJson, "package_json/yarn1_object.json"),
    (ManifestKind::PackageJson, "package_json/plain.json"),
    (ManifestKind::PackageJson, "package_json/syntax_error.json"),
    (ManifestKind::PnpmWorkspace, "pnpm/pnpm10.yaml"),
    (ManifestKind::PnpmWorkspace, "pnpm/pnpm7_compact.yaml"),
    (ManifestKind::PnpmWorkspace, "pnpm/flow.yaml"),
    (ManifestKind::PnpmWorkspace, "pnpm/flow_multiline.yaml"),
    (ManifestKind::PnpmWorkspace, "pnpm/partial.yaml"),
    (ManifestKind::PnpmWorkspace, "pnpm/syntax_error.yaml"),
    (ManifestKind::CargoToml, "cargo/virtual.toml"),
    (ManifestKind::CargoToml, "cargo/package_workspace.toml"),
    (ManifestKind::CargoToml, "cargo/package.toml"),
    (ManifestKind::CargoToml, "cargo/syntax_error.toml"),
    (ManifestKind::PomXml, "pom/pom_4_0_0.xml"),
    (ManifestKind::PomXml, "pom/pom_4_1_0.xml"),
    (ManifestKind::PomXml, "pom/module.xml"),
    (ManifestKind::PomXml, "pom/property_module.xml"),
    (ManifestKind::PomXml, "pom/syntax_error.xml"),
    (
        ManifestKind::GradleSettings,
        "gradle/settings_groovy.gradle",
    ),
    (
        ManifestKind::GradleSettings,
        "gradle/settings_kotlin.gradle.kts",
    ),
    (ManifestKind::GradleSettings, "gradle/variables.gradle"),
    (ManifestKind::GradleSettings, "gradle/syntax_error.gradle"),
    (ManifestKind::GoWork, "go/go.work"),
    (ManifestKind::GoMod, "go/go.mod"),
    (ManifestKind::GoWork, "go/syntax_error.work"),
];

const ALL_KINDS: [ManifestKind; 8] = [
    ManifestKind::PackageJson,
    ManifestKind::PnpmWorkspace,
    ManifestKind::CargoToml,
    ManifestKind::PomXml,
    ManifestKind::GradleSettings,
    ManifestKind::GradleBuild,
    ManifestKind::GoMod,
    ManifestKind::GoWork,
];

macro_rules! snapshot_tests {
    ($($name:ident: $kind:ident, $file:literal;)*) => {$(
        #[test]
        fn $name() {
            insta::assert_debug_snapshot!(parse(ManifestKind::$kind, &fixture($file)));
        }
    )*};
}

snapshot_tests! {
    manifest_package_json_npm_workspaces: PackageJson, "package_json/npm_workspaces.json";
    manifest_package_json_yarn1_object: PackageJson, "package_json/yarn1_object.json";
    manifest_package_json_plain: PackageJson, "package_json/plain.json";
    manifest_pnpm_v10_comments_and_excludes: PnpmWorkspace, "pnpm/pnpm10.yaml";
    manifest_pnpm_v7_compact_sequence: PnpmWorkspace, "pnpm/pnpm7_compact.yaml";
    manifest_pnpm_flow_sequence: PnpmWorkspace, "pnpm/flow.yaml";
    manifest_pnpm_flow_sequence_multiline: PnpmWorkspace, "pnpm/flow_multiline.yaml";
    manifest_pnpm_unsupported_forms_are_partial: PnpmWorkspace, "pnpm/partial.yaml";
    manifest_cargo_virtual_with_exclude: CargoToml, "cargo/virtual.toml";
    manifest_cargo_package_and_workspace: CargoToml, "cargo/package_workspace.toml";
    manifest_cargo_package: CargoToml, "cargo/package.toml";
    manifest_pom_4_0_0_modules_and_profiles: PomXml, "pom/pom_4_0_0.xml";
    manifest_pom_4_1_0_subprojects: PomXml, "pom/pom_4_1_0.xml";
    manifest_pom_module_group_from_parent: PomXml, "pom/module.xml";
    manifest_pom_property_module_is_partial: PomXml, "pom/property_module.xml";
    manifest_gradle_groovy_settings: GradleSettings, "gradle/settings_groovy.gradle";
    manifest_gradle_kotlin_settings: GradleSettings, "gradle/settings_kotlin.gradle.kts";
    manifest_gradle_variables_are_partial: GradleSettings, "gradle/variables.gradle";
    manifest_go_work_use_block: GoWork, "go/go.work";
    manifest_go_mod_module: GoMod, "go/go.mod";
}

#[test]
fn manifest_syntax_errors_report_the_line() {
    let cases = [
        (
            ManifestKind::PackageJson,
            "package_json/syntax_error.json",
            5,
        ),
        (ManifestKind::PnpmWorkspace, "pnpm/syntax_error.yaml", 3),
        (ManifestKind::CargoToml, "cargo/syntax_error.toml", 3),
        (ManifestKind::PomXml, "pom/syntax_error.xml", 6),
        (
            ManifestKind::GradleSettings,
            "gradle/syntax_error.gradle",
            3,
        ),
        (ManifestKind::GoWork, "go/syntax_error.work", 3),
    ];
    for (kind, file, line) in cases {
        assert_eq!(
            parse(kind, &fixture(file)),
            Err(ManifestError::Syntax { line }),
            "{file}"
        );
    }
}

#[test]
fn manifest_syntax_errors_inline() {
    let syntax = |kind, text: &str| match parse(kind, text.as_bytes()) {
        Err(ManifestError::Syntax { line }) => line,
        other => panic!("{kind:?} {text:?}: {other:?}"),
    };
    assert_eq!(syntax(ManifestKind::PackageJson, ""), 1);
    assert_eq!(syntax(ManifestKind::PackageJson, "{\"workspaces\": 3}"), 1);
    assert_eq!(
        syntax(
            ManifestKind::PnpmWorkspace,
            "packages:\n  - a\npackages:\n  - b\n"
        ),
        3
    );
    assert_eq!(
        syntax(ManifestKind::PnpmWorkspace, "packages: ['a',\n  'b'\n"),
        1
    );
    assert_eq!(
        syntax(ManifestKind::PnpmWorkspace, "packages: ['a'] trailing\n"),
        1
    );
    assert_eq!(
        syntax(
            ManifestKind::CargoToml,
            "[workspace]\nmembers = \"crates/*\"\n"
        ),
        2
    );
    assert_eq!(syntax(ManifestKind::PomXml, ""), 1);
    assert_eq!(syntax(ManifestKind::PomXml, "<project>\n<modules>\n"), 3);
    assert_eq!(
        syntax(ManifestKind::GradleSettings, "include 'a'\ninclude \"b\n"),
        2
    );
    assert_eq!(syntax(ManifestKind::GoWork, "use ./a\n)\n"), 2);
    assert_eq!(syntax(ManifestKind::GoWork, "use \"./a\n"), 1);
    assert_eq!(syntax(ManifestKind::GoMod, "module a\nmodule b\n"), 2);
    assert_eq!(syntax(ManifestKind::GoMod, "module\n"), 1);
}

#[test]
fn manifest_too_large_is_not_parsed() {
    let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
    for kind in ALL_KINDS {
        assert_eq!(parse(kind, &big), Err(ManifestError::TooLarge), "{kind:?}");
    }
    // Exactly the limit is parsed.
    let mut at_limit = b"{\"name\": \"x\"}".to_vec();
    at_limit.resize(MAX_MANIFEST_BYTES, b' ');
    assert_eq!(
        parse(ManifestKind::PackageJson, &at_limit)
            .unwrap()
            .name
            .as_deref(),
        Some("x")
    );
}

#[test]
fn manifest_gradle_build_is_not_read() {
    assert_eq!(
        parse(ManifestKind::GradleBuild, b"this is { not parsed"),
        Ok(Parsed::default())
    );
}

#[test]
fn manifest_utf8_bom_is_skipped() {
    let parsed = parse(
        ManifestKind::PackageJson,
        b"\xEF\xBB\xBF{\"name\": \"bom\"}",
    )
    .unwrap();
    assert_eq!(parsed.name.as_deref(), Some("bom"));
    let parsed = parse(
        ManifestKind::CargoToml,
        b"\xEF\xBB\xBF[package]\nname = \"bom\"\n",
    )
    .unwrap();
    assert_eq!(parsed.name.as_deref(), Some("bom"));
}

#[test]
fn manifest_invalid_utf8() {
    // JSON and TOML must be UTF-8.
    assert_eq!(
        parse(ManifestKind::CargoToml, b"[package]\nname = \"\xFF\"\n"),
        Err(ManifestError::Syntax { line: 2 })
    );
    assert!(matches!(
        parse(ManifestKind::PackageJson, b"{\"name\": \"\xFF\"}"),
        Err(ManifestError::Syntax { .. })
    ));
    // A Latin-1 comment in a pom or settings script does not hide the modules.
    let pom = parse(
        ManifestKind::PomXml,
        b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<project><!-- Jos\xE9 --><modules><module>a</module></modules></project>",
    )
    .unwrap();
    assert_eq!(
        pom.members,
        [Member::Path {
            path: "a".into(),
            name: None
        }]
    );
    let gradle = parse(ManifestKind::GradleSettings, b"// Jos\xE9\ninclude ':a'\n").unwrap();
    assert_eq!(gradle.members.len(), 1);
}

#[test]
fn manifest_package_json_details() {
    let parsed = parse(
        ManifestKind::PackageJson,
        br#"{"workspaces": {"nohoist": ["x"]}}"#,
    )
    .unwrap();
    assert!(parsed.workspace && parsed.members.is_empty() && parsed.name.is_none());
    let parsed = parse(
        ManifestKind::PackageJson,
        br#"{"name": null, "workspaces": []}"#,
    )
    .unwrap();
    assert!(parsed.workspace && parsed.name.is_none());
}

#[test]
fn manifest_pnpm_details() {
    let members = |text: &str| {
        let parsed = parse(ManifestKind::PnpmWorkspace, text.as_bytes()).unwrap();
        let globs: Vec<String> = parsed
            .members
            .iter()
            .map(|m| match m {
                Member::Glob(g) => g.to_string(),
                Member::Path { .. } => unreachable!(),
            })
            .collect();
        (globs, parsed.partial)
    };
    // No packages key: a workspace with no members (pnpm still includes the root).
    assert_eq!(members("catalog:\n  a: 1\n"), (vec![], false));
    assert_eq!(members("packages:\n"), (vec![], false));
    assert_eq!(members("packages: []\n"), (vec![], false));
    assert_eq!(
        members("\"packages\":\r\n  - \"a\\u0041\"\r\n"),
        (vec!["aA".to_owned()], false)
    );
    assert_eq!(
        members("packages:\n  - a#not-a-comment\n"),
        (vec!["a#not-a-comment".to_owned()], false)
    );
    // Out of the subset: not expanded.
    assert_eq!(members("packages: *anchor\n"), (vec![], true));
    assert_eq!(
        members("packages:\n  - a\n    continued\n"),
        (vec!["a".to_owned()], true)
    );
    assert_eq!(members("packages:\n  key: value\n"), (vec![], true));
    assert_eq!(members("packages:\n  - key: value\n"), (vec![], true));
    assert_eq!(
        members("packages: [a, {b: c}, d]\n"),
        (vec!["a".to_owned(), "d".to_owned()], true)
    );
}

#[test]
fn manifest_gradle_details() {
    let parse_gradle = |text: &str| parse(ManifestKind::GradleSettings, text.as_bytes()).unwrap();
    let paths = |parsed: &Parsed| -> Vec<String> {
        parsed
            .members
            .iter()
            .map(|m| match m {
                Member::Path { path, .. } => path.to_string(),
                Member::Glob(_) => unreachable!(),
            })
            .collect()
    };
    // Member access and assignments are not calls.
    let parsed = parse_gradle("foo.include 'x'\ninclude = 3\n");
    assert!(parsed.members.is_empty() && !parsed.partial);
    // The leading colon is optional, empty projects are ignored.
    let parsed = parse_gradle("include 'a', 'b:c', ':', ''\n");
    assert_eq!(paths(&parsed), ["a", "b/c"]);
    // Statements end at a newline unless a comma continues them.
    let parsed = parse_gradle("include 'a'\n'b'\ninclude 'c',\n\n  'd'\n");
    assert_eq!(paths(&parsed), ["a", "c", "d"]);
    // Escapes and triple-quoted strings.
    let parsed =
        parse_gradle("include \"\\u0061\"\ninclude '''b'''\nrootProject.name = \"\"\"r\"\"\"\n");
    assert_eq!(paths(&parsed), ["a", "b"]);
    assert_eq!(parsed.name.as_deref(), Some("r"));
    // A name built from more than one literal is not evaluated.
    let parsed = parse_gradle("rootProject.name = 'a' + 'b'\n");
    assert!(parsed.name.is_none() && parsed.partial);
    // projectDir for a project that is not included changes nothing.
    let parsed = parse_gradle("project(':x').projectDir = file('y')\n");
    assert!(parsed.members.is_empty() && !parsed.partial);
    // A semicolon ends a statement.
    let parsed = parse_gradle("include 'a'; include 'b'\n");
    assert_eq!(paths(&parsed), ["a", "b"]);
}

#[test]
fn manifest_go_details() {
    let parsed = parse(
        ManifestKind::GoMod,
        b"module \"example.com/quoted\" // comment\n",
    )
    .unwrap();
    assert_eq!(parsed.name.as_deref(), Some("example.com/quoted"));
    let parsed = parse(ManifestKind::GoMod, b"module (\n\texample.com/block\n)\n").unwrap();
    assert_eq!(parsed.name.as_deref(), Some("example.com/block"));
    let parsed = parse(ManifestKind::GoWork, b"use .//comment right after\n").unwrap();
    assert_eq!(
        parsed.members,
        [Member::Path {
            path: ".".into(),
            name: None
        }]
    );
}

#[test]
fn manifest_kind_from_file_name() {
    let names = [
        ("package.json", Some(ManifestKind::PackageJson)),
        ("pnpm-workspace.yaml", Some(ManifestKind::PnpmWorkspace)),
        ("Cargo.toml", Some(ManifestKind::CargoToml)),
        ("pom.xml", Some(ManifestKind::PomXml)),
        ("settings.gradle", Some(ManifestKind::GradleSettings)),
        ("settings.gradle.kts", Some(ManifestKind::GradleSettings)),
        ("build.gradle", Some(ManifestKind::GradleBuild)),
        ("build.gradle.kts", Some(ManifestKind::GradleBuild)),
        ("go.mod", Some(ManifestKind::GoMod)),
        ("go.work", Some(ManifestKind::GoWork)),
        ("cargo.toml", None),
        ("package-lock.json", None),
        ("pnpm-workspace.yml", None),
    ];
    for (name, kind) in names {
        assert_eq!(ManifestKind::from_file_name(name), kind, "{name}");
    }
    assert_eq!(ManifestKind::PnpmWorkspace.ecosystem(), Ecosystem::Npm);
    assert_eq!(ManifestKind::GradleBuild.ecosystem(), Ecosystem::Gradle);
    assert_eq!(ManifestKind::GoWork.ecosystem(), Ecosystem::Go);
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n.max(1) as u64).unwrap_or(0)
    }
}

/// Truncated, mutated and random input: every parser returns, never panics.
#[test]
fn manifest_garbage_input_does_not_panic() {
    for &(kind, file) in FIXTURES {
        let bytes = fixture(file);
        for end in 0..=bytes.len() {
            let _ = parse(kind, &bytes[..end]);
        }
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    // Bytes that matter to some parser.
    const SYNTAX: &[u8] = b"{}[]()<>\"'`$\\/*#:,;=!-&|. \n\t\r\xFF\xC3";
    for round in 0..4000 {
        let &(kind, file) = &FIXTURES[round % FIXTURES.len()];
        let mut bytes = fixture(file);
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(bytes.len());
            let b = SYNTAX[rng.below(SYNTAX.len())];
            match rng.below(3) {
                0 if !bytes.is_empty() => bytes[at] = b,
                1 => bytes.insert(at, b),
                _ if !bytes.is_empty() => {
                    bytes.remove(at);
                }
                _ => {}
            }
        }
        let _ = parse(kind, &bytes);
        let len = rng.below(256);
        let noise: Vec<u8> = (0..len).map(|_| SYNTAX[rng.below(SYNTAX.len())]).collect();
        for k in ALL_KINDS {
            let _ = parse(k, &noise);
        }
    }
}

/// Deep nesting stays within the stack: the libraries limit recursion and
/// the hand-written parsers do not recurse on input (Gradle interpolation
/// is capped).
#[test]
fn manifest_deep_nesting_does_not_overflow() {
    let deep = |open: &str, n: usize| open.repeat(n).into_bytes();
    let n = 100_000;
    for kind in ALL_KINDS {
        for open in ["[", "{", "(", "<a>", "\"${", "- "] {
            let _ = parse(kind, &deep(open, n));
        }
    }
    let mut toml = b"a = ".to_vec();
    toml.extend(deep("[", n));
    assert!(parse(ManifestKind::CargoToml, &toml).is_err());
    let mut yaml = b"packages: ".to_vec();
    yaml.extend(deep("[", n));
    assert!(parse(ManifestKind::PnpmWorkspace, &yaml).is_err());
}
