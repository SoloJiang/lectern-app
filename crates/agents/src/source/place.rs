//! 按 cwd 归位（docs/design/agents.md「数据流与线程」第 3 步）。

use std::collections::{HashMap, HashSet};
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

    /// 一轮刷新之后的归位，返回是否有变化。阻塞（`canonicalize`、`resolve`），只在 executor 上调用。
    ///
    /// 新的 cwd 归位；已有的只在可能变了时重新核对：这一轮重新读过摘要的会话的 cwd（`touched`），以及当时
    /// 不存在的 cwd（`Missing`）。核对是一次规范化：目录消失、出现或解析到别处时，丢掉缓存的 `resolve`
    /// 结果重新归位；没变时什么都不做，所以同一个目录只 `resolve` 一次。
    pub(crate) fn place_new<'a>(
        &mut self,
        projects: &ProjectsRef,
        cwds: impl IntoIterator<Item = &'a Arc<Path>>,
        touched: &HashSet<Arc<Path>>,
    ) -> bool {
        let mut changed = false;
        for cwd in cwds {
            match self.by_cwd.get(&**cwd) {
                Some(held) => {
                    if held.canonical.is_none() || touched.contains(cwd) {
                        changed |= self.revalidate(projects, cwd, false);
                    }
                }
                None => {
                    let canonical = std::fs::canonicalize(cwd).ok();
                    let place = self.locate(projects, canonical.as_deref());
                    changed |= place != Place::Pending;
                    self.by_cwd.insert(cwd.clone(), Placed { canonical, place });
                }
            }
        }
        changed
    }

    /// 收到 `ProjectEvent` 后重新归位可能受影响的 cwd，返回是否有变化。归在这个项目里的（它的 worktree
    /// 可能增减、被删除或重建）丢掉缓存的 `resolve` 结果，重新规范化和归位；还在 `Pending`、`Missing` 的
    /// 同样重新核对；项目打开时，其余没有 `Located` 的 cwd 只重新 `locate`（只查内存）。其他项目里的不动。
    pub(crate) fn replace(&mut self, projects: &ProjectsRef, event: &ProjectEvent) -> bool {
        let (project, opened) = match event {
            ProjectEvent::Opened { result: Err(_), .. } => return false,
            ProjectEvent::Opened {
                result: Ok(project),
                ..
            } => (project, true),
            ProjectEvent::Closed { project } => (project, false),
            ProjectEvent::Changed { project, .. } => (project, false),
        };
        let work: Vec<(Arc<Path>, bool)> = self
            .by_cwd
            .iter()
            .filter_map(|(cwd, p)| {
                let in_project = project_of(&p.place) == Some(project);
                let unplaced = matches!(p.place, Place::Pending | Place::Missing);
                let maybe_inside = opened && !matches!(p.place, Place::Located(_));
                (in_project || unplaced || maybe_inside).then(|| (cwd.clone(), in_project))
            })
            .collect();
        let mut changed = false;
        for (cwd, in_project) in work {
            changed |= self.revalidate(projects, &cwd, in_project);
        }
        changed
    }

    /// 重新规范化一个 cwd；规范路径变了（目录消失、出现、解析到别处），或 `stale`（它所在项目的 worktree
    /// 集合可能变了）时丢掉缓存的 `resolve` 结果，然后重新归位。返回归位结果是否变了。
    fn revalidate(&mut self, projects: &ProjectsRef, cwd: &Arc<Path>, stale: bool) -> bool {
        let canonical = std::fs::canonicalize(cwd).ok();
        let held = self.by_cwd.get(&**cwd).expect("a held cwd");
        let moved = held.canonical != canonical;
        if !moved && !stale && held.place != Place::Pending {
            return false;
        }
        if moved || stale {
            for path in [held.canonical.clone(), canonical.clone()]
                .into_iter()
                .flatten()
            {
                self.resolved.remove(&path);
            }
        }
        let place = self.locate(projects, canonical.as_deref());
        let held = self.by_cwd.get_mut(&**cwd).expect("a held cwd");
        held.canonical = canonical;
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
