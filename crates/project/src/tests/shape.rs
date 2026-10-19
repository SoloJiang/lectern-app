//! bench's synthetic repositories (`testing::Shape`) are what「性能」says
//! they are (`shape_*`).

use std::collections::HashMap;
use std::sync::Arc;

use lectern_services::project::{FileKind, FileList, ManifestKind, Package};

use crate::testing::Shape;
use crate::{ManifestResult, manifest, resolve_packages};

fn resolve(shape: &Shape) -> Vec<Package> {
    let mut files = FileList::builder();
    for path in &shape.paths {
        files.push(path, FileKind::File);
    }
    let files = files.build();
    assert_eq!(files.len(), shape.paths.len(), "paths are valid and unique");
    let contents: HashMap<&str, &[u8]> = shape
        .manifests
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let read = |path: &str| -> Option<ManifestResult> {
        let kind = ManifestKind::from_file_name(path.rsplit('/').next()?)?;
        let bytes = contents.get(path)?;
        Some(manifest::parse(kind, bytes).map(Arc::new))
    };
    resolve_packages(&files, "repo", &read)
}

/// Every manifest is listed, parses, and is the only manifest-named path;
/// the parse cache (2 MB) holds all of them, so a warm cache stays warm.
fn check(shape: &Shape, files: usize, manifests: usize) -> Vec<Package> {
    assert_eq!(shape.paths.len(), files);
    assert_eq!(shape.manifests.len(), manifests);
    assert!(shape.paths.windows(2).all(|w| w[0] < w[1]));
    let named = shape
        .paths
        .iter()
        .filter(|p| ManifestKind::from_file_name(p.rsplit('/').next().unwrap()).is_some())
        .count();
    assert_eq!(named, manifests);
    for (path, content) in &shape.manifests {
        assert!(shape.paths.binary_search(path).is_ok(), "{path} listed");
        let kind = ManifestKind::from_file_name(path.rsplit('/').next().unwrap()).unwrap();
        let parsed = manifest::parse(kind, content).unwrap();
        assert!(!parsed.partial, "{path}");
    }
    let bytes: usize = shape.manifests.iter().map(|(_, c)| c.len()).sum();
    assert!(bytes < 2 * 1024 * 1024, "{bytes} bytes of manifests");
    let packages = resolve(shape);
    assert_eq!(packages.len(), shape.packages);
    assert!(
        packages.iter().all(|p| !p.manifests.is_empty()),
        "every sub-project has its own manifest"
    );
    packages
}

#[test]
fn shape_cloudprint_is_728_files_and_25_manifests() {
    let packages = check(&Shape::cloudprint(), 728, 25);
    let undeclared: Vec<&str> = packages
        .iter()
        .filter(|p| !p.declared && !p.dir.is_empty())
        .map(|p| &*p.dir)
        .collect();
    assert_eq!(
        undeclared,
        ["apps/miniapp-console/frontend", "tools/codegen"]
    );
    assert!(packages[0].dir.is_empty() && packages[0].manifests.len() == 2);
}

#[test]
fn shape_monorepo_declares_every_member() {
    let shape = Shape::monorepo(10_000, 200);
    let packages = check(&shape, 10_000, 200);
    assert!(packages.iter().skip(1).all(|p| p.declared));
    assert_eq!(packages[0].manifests.len(), 4);
    let by_eco = |kind: ManifestKind| {
        packages
            .iter()
            .skip(1)
            .filter(|p| p.manifests.iter().any(|m| m.kind == kind))
            .count()
    };
    assert_eq!(
        (
            by_eco(ManifestKind::PackageJson),
            by_eco(ManifestKind::PomXml),
            by_eco(ManifestKind::GoMod),
            by_eco(ManifestKind::CargoToml)
        ),
        (149, 39, 4, 4)
    );
}

#[test]
fn shape_monorepo_100k_counts() {
    let shape = Shape::monorepo(100_000, 2_000);
    assert_eq!(shape.paths.len(), 100_000);
    assert_eq!(shape.manifests.len(), 2_000);
    assert_eq!(shape.packages, 1_997);
}
