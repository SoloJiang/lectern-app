//! npm's rules (「依赖层」「发现」): package names, a sub-project's direct
//! dependencies, a package's declaration entry, and the candidates of a
//! relative module specifier. Pure functions over manifests and paths; the
//! reads are `Roots`'.

use super::json::Json;

/// Direct dependencies taken from one manifest; more are logged and only
/// resolved on demand.
pub(crate) const MAX_DIRECT: usize = 512;

/// The fields whose keys are the direct dependencies, in this order.
const FIELDS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// Whether `name` is a package name by npm's rules: an optional `@scope/`
/// and one segment, each non-empty, not `.` or `..`, without `/`, `\` or
/// NUL, 214 bytes at most in all.
pub(crate) fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    let segment =
        |s: &str| !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', '\0']);
    match name.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, rest)) => segment(scope) && segment(rest),
            None => false,
        },
        None => segment(name),
    }
}

/// A manifest's direct dependencies: the keys of its four fields, in order,
/// without repeats, valid names only, at most [`MAX_DIRECT`]; and how many
/// more there were.
pub(crate) fn direct_dependencies(manifest: &Json) -> (Vec<String>, usize) {
    let mut out: Vec<String> = Vec::new();
    let mut more = 0;
    for field in FIELDS {
        for (name, _) in manifest.get(field).map_or(&[][..], Json::entries) {
            if !valid_name(name) || out.iter().any(|n| n == name) {
                continue;
            }
            if out.len() >= MAX_DIRECT {
                more += 1;
                continue;
            }
            out.push(name.clone());
        }
    }
    (out, more)
}

/// The `@types` package of `name`: `@types/x` for `x`, `@types/scope__x`
/// for `@scope/x`; None for an `@types` package.
pub(crate) fn types_package(name: &str) -> Option<String> {
    if name.starts_with("@types/") {
        return None;
    }
    Some(
        match name.strip_prefix('@').and_then(|s| s.split_once('/')) {
            Some((scope, rest)) => format!("@types/{scope}__{rest}"),
            None => format!("@types/{name}"),
        },
    )
}

/// The candidates of a package's declaration entry, relative to the
/// package directory, in the order they are tried: `exports`' `"."` types
/// condition (depth first in object order, under `import`, `require`,
/// `node` and `default` too), `types`, `typings`, `index.d.ts`, `main` as
/// `.d.ts`. Paths that leave the package are dropped (`join`).
pub(crate) fn entry_candidates(manifest: &Json) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |p: &str| {
        if let Some(p) = join("", p)
            && !out.contains(&p)
        {
            out.push(p);
        }
    };
    if let Some(dot) = manifest.get("exports").and_then(exports_dot)
        && let Some(types) = types_condition(dot, 0)
    {
        add(types);
    }
    for field in ["types", "typings"] {
        if let Some(p) = manifest.get(field).and_then(Json::as_str) {
            add(p);
        }
    }
    add("index.d.ts");
    if let Some(main) = manifest.get("main").and_then(Json::as_str) {
        for d in declaration_of(main) {
            add(&d);
        }
    }
    out
}

/// `exports`' entry for `"."`: the value itself when it is a string or a
/// conditions object, the `"."` key of a subpath map.
fn exports_dot(exports: &Json) -> Option<&Json> {
    match exports {
        Json::String(_) => Some(exports),
        Json::Object(entries) => {
            if entries.iter().any(|(k, _)| k.starts_with('.')) {
                exports.get(".")
            } else {
                Some(exports)
            }
        }
        _ => None,
    }
}

/// The first `types` condition, depth first in object order, looking under
/// `types`, `import`, `require`, `node` and `default`.
fn types_condition(value: &Json, depth: usize) -> Option<&str> {
    if depth > 8 {
        return None;
    }
    for (key, v) in value.entries() {
        match (key.as_str(), v) {
            ("types", Json::String(s)) => return Some(s),
            ("types" | "import" | "require" | "node" | "default", Json::Object(_)) => {
                if let Some(found) = types_condition(v, depth + 1) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// The declaration files of a script path: `.js`, `.mjs`, `.cjs` as `.d.ts`,
/// `.d.mts`, `.d.cts`; a path without one of those as itself plus `.d.ts`.
fn declaration_of(path: &str) -> Vec<String> {
    for (js, dts) in [(".js", ".d.ts"), (".mjs", ".d.mts"), (".cjs", ".d.cts")] {
        if let Some(stem) = path.strip_suffix(js) {
            return vec![format!("{stem}{dts}")];
        }
    }
    vec![format!("{path}.d.ts")]
}

/// Whether a module specifier is relative (`./`, `../`, `.` or `..`): only
/// those are followed; bare names are recorded only.
pub(crate) fn is_relative(spec: &str) -> bool {
    spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../")
}

/// The files a relative specifier `spec` of the file `from` (both relative
/// to the package directory) may name, in the order tried: `s` itself (its
/// extension allowed is checked by the caller), `s`'s `.js`, `.mjs`, `.cjs`
/// as declarations, `s.d.ts`, `s.d.mts`, `s.d.cts`, `s.ts`, `s.tsx`,
/// `s/index.d.ts`, `s/index.ts`. Candidates that leave the package are
/// dropped; none when `s` does.
pub(crate) fn spec_candidates(from: &str, spec: &str) -> Vec<String> {
    let dir = match from.rfind('/') {
        Some(i) => &from[..i],
        None => "",
    };
    let Some(s) = join(dir, spec) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut add = |p: String| {
        if !p.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    };
    add(s.clone());
    for (js, dts) in [(".js", ".d.ts"), (".mjs", ".d.mts"), (".cjs", ".d.cts")] {
        if let Some(stem) = s.strip_suffix(js) {
            add(format!("{stem}{dts}"));
        }
    }
    // The package directory itself ("..") has only its index files.
    if !s.is_empty() {
        for ext in [".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx"] {
            add(format!("{s}{ext}"));
        }
    }
    let base = if s.is_empty() {
        String::new()
    } else {
        format!("{s}/")
    };
    add(format!("{base}index.d.ts"));
    add(format!("{base}index.ts"));
    out
}

/// `dir` (a relative directory, "" for the top) joined with `path` and
/// normalized lexically: `.` dropped, `..` taking off the last component,
/// repeated or trailing `/` collapsed. None when the result would leave the
/// top (a `..` too many, an absolute path) or `path` has a NUL or `\`. "" is
/// the top itself.
pub(crate) fn join(dir: &str, path: &str) -> Option<String> {
    if path.starts_with('/') || path.contains(['\0', '\\']) {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_npm() {
        for good in ["react", "@types/node", "a.b-c_d", "@s/x.y"] {
            assert!(valid_name(good), "{good}");
        }
        let long = "a".repeat(215);
        for bad in [
            "", ".", "..", "a/b", "@s", "@s/", "@/x", "@s/x/y", "a\\b", "a\0", "@s/..", &long,
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn join_stays_inside() {
        assert_eq!(join("a/b", "../c").as_deref(), Some("a/c"));
        assert_eq!(join("a", "./b//c/").as_deref(), Some("a/b/c"));
        assert_eq!(join("", "..").as_deref(), None);
        assert_eq!(join("a", "../../x").as_deref(), None);
        assert_eq!(join("a", "/etc/passwd").as_deref(), None);
        assert_eq!(join("a", "..").as_deref(), Some(""));
    }

    #[test]
    fn types_packages() {
        assert_eq!(types_package("react").as_deref(), Some("@types/react"));
        assert_eq!(
            types_package("@babel/core").as_deref(),
            Some("@types/babel__core")
        );
        assert_eq!(types_package("@types/node"), None);
    }
}
