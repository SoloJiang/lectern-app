//! 归位（`place_*`）：模拟的 `Projects`，`resolve` 只调一次、`Missing`、`Located` 带子项目、
//! 非当前 worktree 经 `resolve_package`、符号链接、项目打开与子项目加载完成后重新归位。

use std::path::Path;

use insta::assert_snapshot;
use lectern_services::project::{ChangedParts, OpenRequest, ProjectEvent, ProjectKey, Projects};
use lectern_services::session::Place;

use super::support::*;

/// `claude/projects/-p/<id>.jsonl`，cwd 为 `cwd`。
fn session_at(h: &Harness, n: u32, cwd: &Path) -> String {
    let id = uuid(n);
    let dir = h.claude_root().join("-p");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.jsonl")),
        claude_session(&id, cwd, &[&format!("prompt {n}")], n),
    )
    .unwrap();
    id
}

fn places(h: &Harness) -> String {
    let list = h.list(&claude_source());
    let mut lines: Vec<String> = list
        .sessions
        .iter()
        .map(|s| format!("{} {} {}", s.key.id, h.rel(&s.cwd), h.place(&s.place)))
        .collect();
    lines.sort();
    lines.join("\n")
}

#[test]
fn place_same_cwd_is_resolved_once() {
    let mut h = Harness::new();
    let work = h.base.join("work");
    std::fs::create_dir_all(&work).unwrap();
    for n in 1..=3 {
        session_at(&h, n, &work);
    }
    h.start();
    assert_eq!(h.projects.resolves(), std::slice::from_ref(&work));
    assert_snapshot!(places(&h), @r#"
    00000001-0000-4000-8000-000000000000 [base]/work Resolved(folder:[base]/work wt=- rel="")
    00000002-0000-4000-8000-000000000000 [base]/work Resolved(folder:[base]/work wt=- rel="")
    00000003-0000-4000-8000-000000000000 [base]/work Resolved(folder:[base]/work wt=- rel="")
    "#);

    // A refresh does not resolve again.
    let id = session_at(&h, 4, &work);
    h.emit(&h.claude_root(), &[(&format!("-p/{id}.jsonl"), F)]);
    assert_eq!(h.projects.resolves().len(), 1);
}

#[test]
fn place_missing_cwd_is_missing() {
    let mut h = Harness::new();
    session_at(&h, 1, &h.base.join("gone"));
    h.start();
    assert_eq!(h.list(&claude_source()).sessions[0].place, Place::Missing);
    assert!(h.projects.resolves().is_empty(), "nothing to resolve");
}

#[test]
fn place_open_project_is_located_with_its_package() {
    let mut h = Harness::new();
    let repo = h.base.join("repo");
    std::fs::create_dir_all(repo.join("apps/web/src")).unwrap();
    let mut project = git_project(&repo, &[]);
    project.trees[0].packages = Some(vec!["apps/web".into()]);
    h.projects.add(project, true);
    session_at(&h, 1, &repo.join("apps/web/src"));
    session_at(&h, 2, &repo);
    h.start();
    assert_snapshot!(places(&h), @r#"
    00000001-0000-4000-8000-000000000000 [base]/repo/apps/web/src Located(git:[base]/repo/.git wt=[base]/repo/.git rel="apps/web/src" Found("apps/web"))
    00000002-0000-4000-8000-000000000000 [base]/repo Located(git:[base]/repo/.git wt=[base]/repo/.git rel="" None)
    "#);
    assert!(h.projects.resolves().is_empty(), "locate only");
    assert!(h.projects.package_calls().is_empty());
}

#[test]
fn place_other_worktree_gets_its_own_package() {
    let mut h = Harness::new();
    let repo = h.base.join("repo");
    let linked = h.base.join("wt-feature");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(linked.join("pkg/src")).unwrap();
    let mut project = git_project(&repo, &[&linked]);
    project.trees[1].packages = Some(vec!["pkg".into()]);
    h.projects.add(project, true);
    session_at(&h, 1, &linked.join("pkg/src"));
    h.start();
    assert_snapshot!(places(&h), @r#"00000001-0000-4000-8000-000000000000 [base]/wt-feature/pkg/src Located(git:[base]/repo/.git wt=[base]/repo/.git/worktrees/wt-feature rel="pkg/src" Found("pkg"))"#);
    assert_eq!(
        h.projects.package_calls(),
        [(
            h.base.join("repo/.git/worktrees/wt-feature"),
            "pkg/src".to_owned()
        )]
    );
}

#[test]
fn place_symlinked_cwd_is_located_and_keeps_the_logged_path() {
    let mut h = Harness::new();
    let repo = h.base.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::os::unix::fs::symlink(&repo, h.base.join("link")).unwrap();
    h.projects.add(git_project(&repo, &[]), true);
    session_at(&h, 1, &h.base.join("link/src"));
    h.start();
    let s = h.list(&claude_source()).sessions[0].clone();
    assert_eq!(&*s.cwd, h.base.join("link/src").as_path());
    assert_snapshot!(h.place(&s.place), @r#"Located(git:[base]/repo/.git wt=[base]/repo/.git rel="src" None)"#);
}

#[test]
fn place_project_opened_later_is_located() {
    let mut h = Harness::new();
    let repo = h.base.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let project = git_project(&repo, &[]);
    let project_key = project.key.clone();
    h.projects.add(project, false);
    let id = session_at(&h, 1, &repo.join("src"));
    h.start();
    assert_snapshot!(places(&h), @r#"00000001-0000-4000-8000-000000000000 [base]/repo/src Resolved(git:[base]/repo/.git wt=[base]/repo/.git rel="src")"#);
    h.take_seen(&claude_source());

    h.projects.open(&project_key);
    h.run().kernel.emitter().emit(ProjectEvent::Opened {
        request: OpenRequest(1),
        result: Ok(project_key.clone()),
    });
    h.settle();
    assert_snapshot!(places(&h), @r#"00000001-0000-4000-8000-000000000000 [base]/repo/src Located(git:[base]/repo/.git wt=[base]/repo/.git rel="src" None)"#);
    let seen = h.take_seen(&claude_source());
    assert_eq!(&*seen[0].event.changed, [key(&claude_source(), &id)]);
    assert_eq!(h.projects.resolves().len(), 1, "resolve is not repeated");

    // Closing it goes back to the cached `resolve` result.
    Projects::close(&*h.projects, &project_key);
    h.run().kernel.emitter().emit(ProjectEvent::Closed {
        project: project_key,
    });
    h.settle();
    assert!(matches!(
        h.list(&claude_source()).sessions[0].place,
        Place::Resolved(_)
    ));
    assert_eq!(h.projects.resolves().len(), 1);
}

#[test]
fn place_pending_packages_are_filled_after_changed() {
    let mut h = Harness::new();
    let repo = h.base.join("repo");
    std::fs::create_dir_all(repo.join("apps/web")).unwrap();
    let mut project = git_project(&repo, &[]);
    project.trees[0].packages = None;
    let project_key: ProjectKey = project.key.clone();
    h.projects.add(project, true);
    session_at(&h, 1, &repo.join("apps/web"));
    h.start();
    assert_snapshot!(places(&h), @r#"00000001-0000-4000-8000-000000000000 [base]/repo/apps/web Located(git:[base]/repo/.git wt=[base]/repo/.git rel="apps/web" NotLoaded)"#);

    h.projects
        .set_packages(&repo, Some(vec!["apps/web".to_owned()]));
    h.run().kernel.emitter().emit(ProjectEvent::Changed {
        project: project_key,
        generation: 3,
        parts: ChangedParts {
            packages: true,
            ..ChangedParts::default()
        },
    });
    h.settle();
    assert_snapshot!(places(&h), @r#"00000001-0000-4000-8000-000000000000 [base]/repo/apps/web Located(git:[base]/repo/.git wt=[base]/repo/.git rel="apps/web" Found("apps/web"))"#);
}

#[test]
fn place_missing_cwd_is_placed_once_it_exists_again() {
    let mut h = Harness::new();
    let repo = h.base.join("repo");
    let linked = h.base.join("wt-later");
    std::fs::create_dir_all(&repo).unwrap();
    let id = session_at(&h, 1, &linked.join("src"));
    h.start();
    assert_eq!(h.list(&claude_source()).sessions[0].place, Place::Missing);

    // The worktree is created again and its project opened.
    std::fs::create_dir_all(linked.join("src")).unwrap();
    let mut project = git_project(&repo, &[&linked]);
    project.trees[1].packages = Some(Vec::new());
    let project_key = project.key.clone();
    h.projects.add(project, true);
    h.run().kernel.emitter().emit(ProjectEvent::Opened {
        request: OpenRequest(1),
        result: Ok(project_key),
    });
    h.settle();
    assert_snapshot!(places(&h), @r#"00000001-0000-4000-8000-000000000000 [base]/wt-later/src Located(git:[base]/repo/.git wt=[base]/repo/.git/worktrees/wt-later rel="src" None)"#);
    let seen = h.take_seen(&claude_source());
    assert!(
        seen.iter()
            .any(|s| s.event.changed.iter().any(|k| k.id.as_ref() == id))
    );
}

#[test]
fn place_missing_cwd_is_retried_on_the_next_refresh() {
    let mut h = Harness::new();
    let work = h.base.join("later");
    session_at(&h, 1, &work);
    h.start();
    assert_eq!(h.list(&claude_source()).sessions[0].place, Place::Missing);
    std::fs::create_dir_all(&work).unwrap();
    // Any refresh (here a new session elsewhere) canonicalizes it again.
    let other = session_at(&h, 2, &h.base);
    h.emit(&h.claude_root(), &[(&format!("-p/{other}.jsonl"), F)]);
    let list = h.list(&claude_source());
    let first = list
        .sessions
        .iter()
        .find(|s| s.key.id.as_ref() == uuid(1))
        .unwrap();
    assert!(matches!(first.place, Place::Resolved(_)));
}
