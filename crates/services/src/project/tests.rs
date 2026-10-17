use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_kernel::{Event, Service, service};

use super::*;

/// A `Projects` that knows nothing, to check the trait is usable as a
/// shared trait object.
struct Empty;

impl Projects for Empty {
    fn open(&self, _path: &Path) -> OpenRequest {
        OpenRequest(1)
    }
    fn close(&self, _project: &ProjectKey) {}
    fn set_active_worktree(&self, _project: &ProjectKey, _worktree: &WorktreeKey) {}
    fn snapshot(&self, _project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        None
    }
    fn open_projects(&self) -> Vec<ProjectKey> {
        Vec::new()
    }
    fn locate(&self, _path: &Path) -> Option<Location> {
        None
    }
    fn resolve(&self, _path: &Path) -> Result<Resolved, ResolveError> {
        Err(OpenError::NotFound)
    }
    fn resolve_package(
        &self,
        _project: &ProjectKey,
        _worktree: &WorktreeKey,
        _rel: &str,
    ) -> Result<PackageMatch, ScanError> {
        Err(ScanError::RootMissing)
    }
}

fn path(p: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(p))
}

fn shareable<T: Send + Sync + 'static>() {}

#[test]
fn projects_service_id_and_multiple() {
    assert_eq!(<dyn Projects as Service>::ID, "projects");
    const { assert!(!<dyn Projects as Service>::MULTIPLE) };
    let key = service::<dyn Projects>();
    assert_eq!(key.id(), "projects");
    assert!(!key.multiple());
}

#[test]
fn projects_is_a_shared_trait_object() {
    let projects: Arc<dyn Projects> = Arc::new(Empty);
    let key = ProjectKey::Folder { root: path("/x") };
    assert_eq!(projects.open(Path::new("/x")), OpenRequest(1));
    assert!(projects.snapshot(&key).is_none());
    assert!(projects.open_projects().is_empty());
    assert!(projects.locate(Path::new("/x")).is_none());
    assert_eq!(projects.resolve(Path::new("/x")), Err(OpenError::NotFound));
    let worktree = WorktreeKey(path("/x/.git"));
    assert_eq!(
        projects.resolve_package(&key, &worktree, ""),
        Err(ScanError::RootMissing)
    );
    // Snapshots and lookup results cross threads inside `Arc`s.
    shareable::<Arc<dyn Projects>>();
    shareable::<ProjectSnapshot>();
    shareable::<Location>();
    shareable::<Resolved>();
}

#[test]
fn project_event_shape() {
    fn event<E: Event + Clone>() {}
    event::<ProjectEvent>();

    let parts = ChangedParts::default();
    assert!(!parts.worktrees && !parts.active && !parts.files && !parts.packages);

    let key = ProjectKey::Git {
        common_dir: path("/r/.git"),
    };
    let events = [
        ProjectEvent::Opened {
            request: OpenRequest(7),
            result: Ok(key.clone()),
        },
        ProjectEvent::Opened {
            request: OpenRequest(8),
            result: Err(OpenError::RepoUnreadable(Arc::from("gitdir missing"))),
        },
        ProjectEvent::Changed {
            project: key.clone(),
            generation: 2,
            parts: ChangedParts {
                files: true,
                ..parts
            },
        },
        ProjectEvent::Closed {
            project: key.clone(),
        },
    ];
    // TestKernel::emitted needs `Clone`; listeners compare events.
    assert_eq!(events.clone(), events);
}

#[test]
fn project_keys_compare_by_kind_and_path() {
    let git = ProjectKey::Git {
        common_dir: path("/r/.git"),
    };
    let folder = ProjectKey::Folder {
        root: path("/r/.git"),
    };
    assert_ne!(git, folder, "the same path as a repository and a folder");
    assert_eq!(
        git,
        ProjectKey::Git {
            common_dir: path("/r/.git")
        }
    );
    assert_eq!(
        WorktreeKey(path("/r/.git/worktrees/a")),
        WorktreeKey(path("/r/.git/worktrees/a"))
    );
}

#[test]
fn manifest_kinds_by_file_name_and_ecosystem() {
    let cases = [
        ("package.json", ManifestKind::PackageJson, Ecosystem::Npm),
        (
            "pnpm-workspace.yaml",
            ManifestKind::PnpmWorkspace,
            Ecosystem::Npm,
        ),
        ("Cargo.toml", ManifestKind::CargoToml, Ecosystem::Cargo),
        ("pom.xml", ManifestKind::PomXml, Ecosystem::Maven),
        (
            "settings.gradle",
            ManifestKind::GradleSettings,
            Ecosystem::Gradle,
        ),
        (
            "settings.gradle.kts",
            ManifestKind::GradleSettings,
            Ecosystem::Gradle,
        ),
        ("build.gradle", ManifestKind::GradleBuild, Ecosystem::Gradle),
        (
            "build.gradle.kts",
            ManifestKind::GradleBuild,
            Ecosystem::Gradle,
        ),
        ("go.mod", ManifestKind::GoMod, Ecosystem::Go),
        ("go.work", ManifestKind::GoWork, Ecosystem::Go),
    ];
    for (name, kind, ecosystem) in cases {
        assert_eq!(ManifestKind::from_file_name(name), Some(kind), "{name}");
        assert_eq!(kind.ecosystem(), ecosystem, "{name}");
    }
    for name in ["Package.json", "cargo.toml", "a/pom.xml", "yarn.lock", ""] {
        assert_eq!(ManifestKind::from_file_name(name), None, "{name}");
    }
    assert_eq!(
        ManifestError::TooLarge.to_string(),
        format!("manifest is larger than {MAX_MANIFEST_BYTES} bytes")
    );
}
