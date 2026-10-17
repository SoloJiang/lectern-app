//! 按 cwd 归位（docs/design/agents.md「数据流与线程」第 3 步）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_kernel::ServiceRef;
use lectern_services::project::{
    Location, OpenError, PackageMatch, ProjectEvent, ProjectKey, Projects, ResolveError, Resolved,
};
use lectern_services::session::Place;

/// `dyn Projects` 从哪里来：插件里是 kernel 的服务引用；`testing::discover` 和单元测试可以直接给一个对象，
/// 或者不给（全部留在 `Pending`）。
#[derive(Clone)]
pub(crate) enum ProjectsRef {
    Service(ServiceRef<dyn Projects>),
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        expect(dead_code, reason = "testing only")
    )]
    Direct(Arc<dyn Projects>),
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        expect(dead_code, reason = "testing only")
    )]
    None,
}

impl ProjectsRef {
    /// 调用一次，持有服务的准入直到 `f` 返回。服务不可用时为 None。
    fn with<R>(&self, f: impl FnOnce(&dyn Projects) -> R) -> Option<R> {
        match self {
            ProjectsRef::Service(service) => service.get().map(|p| f(&*p)),
            ProjectsRef::Direct(p) => Some(f(&**p)),
            ProjectsRef::None => None,
        }
    }
}

/// 每个日志里的 cwd 的归位结果，以及按规范路径缓存的 `resolve` 结果（同一个目录只 `resolve` 一次）。
#[derive(Default)]
pub(crate) struct Places {
    /// 日志里的 cwd → 规范路径（`None`：目录不存在）与归位结果。
    by_cwd: HashMap<Arc<Path>, Placed>,
    resolved: HashMap<PathBuf, Result<Resolved, ResolveError>>,
}

struct Placed {
    canonical: Option<PathBuf>,
    place: Place,
}

impl Places {
    /// 已有的归位结果；还没有归位时为 None。
    pub(crate) fn get(&self, cwd: &Path) -> Option<&Place> {
        self.by_cwd.get(cwd).map(|p| &p.place)
    }

    /// 给还没有归位的 cwd 归位，返回是否有变化。阻塞（`canonicalize`、`resolve`），只在 executor 上调用。
    pub(crate) fn place_new<'a>(
        &mut self,
        projects: &ProjectsRef,
        cwds: impl IntoIterator<Item = &'a Arc<Path>>,
    ) -> bool {
        let mut changed = false;
        for cwd in cwds {
            if let Some(held) = self.by_cwd.get(&**cwd) {
                // 目录当时不存在（`Missing`）：之后可能又建好了，每次刷新再试一次规范化（一次 `stat`）。
                if held.canonical.is_none() {
                    changed |= self.recanonicalize(projects, cwd);
                }
                continue;
            }
            let canonical = std::fs::canonicalize(cwd).ok();
            let place = self.locate(projects, canonical.as_deref());
            changed |= place != Place::Pending;
            self.by_cwd.insert(cwd.clone(), Placed { canonical, place });
        }
        changed
    }

    /// 收到 `ProjectEvent` 后重新归位受影响的 cwd（打开、关闭：全部；变化：归到这个项目的），返回是否有变化。
    /// `resolve` 的结果照旧用缓存，`locate` 只查内存。
    pub(crate) fn replace(&mut self, projects: &ProjectsRef, event: &ProjectEvent) -> bool {
        let only = match event {
            ProjectEvent::Opened { result: Err(_), .. } => return false,
            ProjectEvent::Opened { .. } | ProjectEvent::Closed { .. } => None,
            ProjectEvent::Changed { project, .. } => Some(project.clone()),
        };
        let cwds: Vec<Arc<Path>> = self
            .by_cwd
            .iter()
            .filter(|(_, p)| match &only {
                None => true,
                Some(project) => project_of(&p.place) == Some(project) || p.place == Place::Pending,
            })
            .map(|(cwd, _)| cwd.clone())
            .collect();
        let mut changed = false;
        for cwd in cwds {
            if self.by_cwd[&cwd].canonical.is_none() {
                changed |= self.recanonicalize(projects, &cwd);
                continue;
            }
            let canonical = self.by_cwd[&cwd].canonical.clone();
            let place = self.locate(projects, canonical.as_deref());
            let held = self.by_cwd.get_mut(&cwd).expect("listed above");
            if held.place != place {
                held.place = place;
                changed = true;
            }
        }
        changed
    }

    /// 当时不存在的 cwd 再规范化一次；现在存在时重新归位。返回归位结果是否变了。
    fn recanonicalize(&mut self, projects: &ProjectsRef, cwd: &Arc<Path>) -> bool {
        let Ok(canonical) = std::fs::canonicalize(cwd) else {
            return false;
        };
        let place = self.locate(projects, Some(&canonical));
        let held = self.by_cwd.get_mut(&**cwd).expect("a held cwd");
        held.canonical = Some(canonical);
        let changed = held.place != place;
        held.place = place;
        changed
    }

    /// 只保留这些 cwd 的结果（会话被删除后不再需要的 cwd 不留在内存里）。
    pub(crate) fn retain(&mut self, keep: &dyn Fn(&Path) -> bool) {
        self.by_cwd.retain(|cwd, _| keep(cwd));
    }

    fn locate(&mut self, projects: &ProjectsRef, canonical: Option<&Path>) -> Place {
        let Some(canonical) = canonical else {
            return Place::Missing;
        };
        let located = projects.with(|p| {
            let mut location: Location = p.locate(canonical)?;
            if location.package == PackageMatch::NotLoaded
                && let Some(worktree) = &location.worktree
                && let Ok(found) = p.resolve_package(&location.project, worktree, &location.rel)
            {
                // 当前 worktree 的子项目还在加载时仍是 NotLoaded，下一次 `Changed` 后重新 `locate`。
                location.package = found;
            }
            Some(location)
        });
        match located {
            None => Place::Pending,
            Some(Some(location)) => Place::Located(location),
            Some(None) => {
                let resolved = match self.resolved.get(canonical) {
                    Some(r) => r.clone(),
                    None => {
                        let Some(r) = projects.with(|p| p.resolve(canonical)) else {
                            return Place::Pending;
                        };
                        self.resolved.insert(canonical.to_path_buf(), r.clone());
                        r
                    }
                };
                match resolved {
                    Ok(r) => Place::Resolved(r),
                    Err(OpenError::NotFound) => Place::Missing,
                    Err(e) => Place::Unresolvable(e),
                }
            }
        }
    }
}

fn project_of(place: &Place) -> Option<&ProjectKey> {
    match place {
        Place::Located(l) => Some(&l.project),
        Place::Resolved(r) => Some(&r.project),
        Place::Pending | Place::Missing | Place::Unresolvable(_) => None,
    }
}
