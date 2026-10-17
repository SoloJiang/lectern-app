//! The plugin set, the dependency graph check, and prepared sets with their
//! dependency closures.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::panic::contain;
use crate::plugin::{Activation, GraphError, Manifest, Name, Plugin, PluginError};
use crate::service::ServiceKey;

/// The plugins of one profile, chosen by `app`.
#[derive(Default)]
pub struct PluginSet {
    pub(crate) plugins: Vec<(Name, Arc<dyn Plugin>)>,
}

impl PluginSet {
    pub fn new() -> Self {
        PluginSet::default()
    }

    /// Adds a plugin. `id` is globally unique, such as `"lang-java"`.
    pub fn with(mut self, id: impl Into<Name>, plugin: impl Plugin) -> Self {
        self.plugins.push((id.into(), Arc::new(plugin)));
        self
    }

    /// Checks the dependency graph: unique plugin IDs and command names, one
    /// type and one `MULTIPLE` per stable service ID, a provider for every
    /// requirement, at most one provider of each single-provider service, a
    /// non-empty activation list and no cycles.
    ///
    /// `app` calls this in a unit test for each profile, so a broken graph
    /// fails CI. Calls `manifest()` on this thread.
    pub fn validate(&self) -> Result<(), Vec<GraphError>> {
        let manifests: Vec<(Name, Result<Manifest, String>)> = self
            .plugins
            .iter()
            .map(|(id, plugin)| (id.clone(), contain(|| plugin.manifest())))
            .collect();
        check(
            manifests
                .iter()
                .map(|(id, manifest)| (id, manifest.as_ref().map_err(String::as_str))),
        )
    }

    /// Calls `manifest()` once on every plugin, on this thread, timed and
    /// under `catch_unwind` the same way [`Kernel::start`](crate::Kernel::start)
    /// does on the kernel thread. A panic is kept, and the plugin starts as
    /// `Failed` from [`Kernel::start_prepared`](crate::Kernel::start_prepared).
    ///
    /// For callers that need the manifests before the kernel starts (the
    /// bench probe prunes the set by them); `app` uses `Kernel::start`.
    pub fn prepare(self) -> PreparedSet {
        PreparedSet {
            plugins: self
                .plugins
                .into_iter()
                .map(|(id, plugin)| Prepared::read(id, plugin))
                .collect(),
        }
    }
}

/// Calls `manifest()` under `contain` and times it. `Kernel::start` and
/// `PluginSet::prepare` both go through here, so their timings agree.
pub(crate) fn read_manifest(plugin: &dyn Plugin) -> (Result<Manifest, String>, Duration) {
    let begin = Instant::now();
    let result = contain(|| plugin.manifest());
    (result, begin.elapsed())
}

/// How a panicking `manifest()` shows in the plugin's state.
pub(crate) fn manifest_failure(message: &str) -> PluginError {
    PluginError::Panicked(format!("manifest panicked: {message}"))
}

/// One plugin of a [`PreparedSet`].
pub(crate) struct Prepared {
    pub(crate) id: Name,
    pub(crate) plugin: Arc<dyn Plugin>,
    /// The manifest, or the panic message.
    pub(crate) manifest: Result<Manifest, String>,
    /// Time spent in that one `manifest()` call.
    pub(crate) time: Duration,
}

impl Prepared {
    fn read(id: Name, plugin: Arc<dyn Plugin>) -> Prepared {
        let (manifest, time) = read_manifest(plugin.as_ref());
        Prepared {
            id,
            plugin,
            manifest,
            time,
        }
    }
}

/// A plugin set whose manifests have been read, each exactly once, by
/// [`PluginSet::prepare`] or [`PreparedSet::with`]. Nothing here calls
/// `manifest()` again; [`Kernel::start_prepared`](crate::Kernel::start_prepared)
/// starts the kernel with these manifests and timings.
pub struct PreparedSet {
    pub(crate) plugins: Vec<Prepared>,
}

impl PreparedSet {
    /// The manifests read, in set order. Plugins whose `manifest()` panicked
    /// are left out.
    pub fn manifests(&self) -> impl Iterator<Item = (&Name, &Manifest)> {
        self.plugins
            .iter()
            .filter_map(|p| p.manifest.as_ref().ok().map(|m| (&p.id, m)))
    }

    /// The providers of the services `id` requires and of the services those
    /// require, transitively, in set order and without `id` itself. Every
    /// provider of a `MULTIPLE` service is included, so keeping the closure
    /// and `id` of a set that passes `validate` leaves a set that passes
    /// `validate` too. Keeping the closure alone does as well, unless a
    /// plugin in it requires a service of `id` (a `MULTIPLE` provider that
    /// depends back on `id`); `validate` on that set reports it.
    ///
    /// Errors: `id` is not in the set or its manifest panicked; a requirement
    /// in the closure has no provider (a plugin whose manifest panicked
    /// provides nothing); a single-provider service has several providers.
    pub fn dependency_closure(&self, id: &str) -> Result<Vec<Name>, Vec<GraphError>> {
        let Some(root) = self.plugins.iter().position(|p| p.id == id) else {
            return Err(vec![GraphError::UnknownPlugin(Name::Owned(id.to_owned()))]);
        };
        let n = self.plugins.len();
        let mut errors = Vec::new();
        let mut reached = vec![false; n];
        reached[root] = true;
        // Breadth first; `next` is the head of the queue.
        let mut queue = vec![root];
        let mut next = 0;
        let mut reported: Vec<ServiceKey> = Vec::new();
        while let Some(&index) = queue.get(next) {
            next += 1;
            let entry = &self.plugins[index];
            let manifest = match &entry.manifest {
                Ok(manifest) => manifest,
                Err(message) => {
                    errors.push(GraphError::ManifestPanicked {
                        plugin: entry.id.clone(),
                        message: message.clone(),
                    });
                    continue;
                }
            };
            for key in &manifest.requires {
                // A plugin does not meet its own requirement, as in `validate`.
                let providers: Vec<usize> = (0..n)
                    .filter(|&p| {
                        p != index
                            && self.plugins[p]
                                .manifest
                                .as_ref()
                                .is_ok_and(|m| m.provides.contains(key))
                    })
                    .collect();
                if providers.is_empty() {
                    errors.push(GraphError::MissingProvider {
                        plugin: entry.id.clone(),
                        service: key.id(),
                    });
                }
                if !key.multiple() && providers.len() > 1 && !reported.contains(key) {
                    reported.push(*key);
                    errors.push(GraphError::MultipleProviders {
                        service: key.id(),
                        plugins: providers
                            .iter()
                            .map(|&p| self.plugins[p].id.clone())
                            .collect(),
                    });
                }
                for p in providers {
                    if !reached[p] {
                        reached[p] = true;
                        queue.push(p);
                    }
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok((0..n)
            .filter(|&i| reached[i] && i != root)
            .map(|i| self.plugins[i].id.clone())
            .collect())
    }

    /// Keeps only the plugins for which `keep(id)` returns true.
    pub fn retain(mut self, mut keep: impl FnMut(&str) -> bool) -> Self {
        self.plugins.retain(|p| keep(&p.id));
        self
    }

    /// Adds a plugin and calls its `manifest()` once, here, timed like
    /// [`prepare`](PluginSet::prepare).
    pub fn with(mut self, id: impl Into<Name>, plugin: impl Plugin) -> Self {
        self.plugins
            .push(Prepared::read(id.into(), Arc::new(plugin)));
        self
    }

    /// [`PluginSet::validate`] over the manifests already read, without
    /// calling `manifest()`.
    pub fn validate(&self) -> Result<(), Vec<GraphError>> {
        check(
            self.plugins
                .iter()
                .map(|p| (&p.id, p.manifest.as_ref().map_err(String::as_str))),
        )
    }
}

/// What the kernel thread starts from: a plain set, whose manifests it reads
/// itself, or a prepared one.
pub(crate) enum Plugins {
    Set(PluginSet),
    Prepared(PreparedSet),
}

/// The checks of `validate` over manifests already read (`Err` holds the
/// panic message).
fn check<'a>(
    manifests: impl Iterator<Item = (&'a Name, Result<&'a Manifest, &'a str>)>,
) -> Result<(), Vec<GraphError>> {
    let mut errors = Vec::new();
    let entries: Vec<(Name, Option<&Manifest>)> = manifests
        .map(|(id, manifest)| match manifest {
            Ok(manifest) => (id.clone(), Some(manifest)),
            Err(message) => {
                errors.push(GraphError::ManifestPanicked {
                    plugin: id.clone(),
                    message: message.to_owned(),
                });
                (id.clone(), None)
            }
        })
        .collect();
    errors.extend(analyze(&entries).errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub(crate) struct GraphReport {
    pub(crate) errors: Vec<GraphError>,
    /// Plugins the kernel fails at startup, with the reason. Missing providers
    /// are not blamed: the dependent waits instead.
    pub(crate) blamed: Vec<(usize, GraphError)>,
    /// Topological order over the plugins with a manifest and not in a cycle:
    /// providers before dependents, ties in `PluginSet` order.
    pub(crate) order: Vec<usize>,
}

pub(crate) fn analyze(entries: &[(Name, Option<&Manifest>)]) -> GraphReport {
    let mut errors = Vec::new();
    let mut blamed = Vec::new();

    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (index, (id, _)) in entries.iter().enumerate() {
        if seen.insert(id.as_ref(), index).is_some() {
            let error = GraphError::DuplicatePlugin(id.clone());
            blamed.push((index, error.clone()));
            errors.push(error);
        }
    }

    let mut commands: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut ids: HashMap<&'static str, ServiceKey> = HashMap::new();
    let mut conflicting: Vec<&'static str> = Vec::new();
    for (index, (id, manifest)) in entries.iter().enumerate() {
        let Some(manifest) = manifest else { continue };
        if manifest.activation.is_empty() {
            let error = GraphError::EmptyActivation(id.clone());
            blamed.push((index, error.clone()));
            errors.push(error);
        }
        for activation in &manifest.activation {
            if let Activation::Command(name) = activation {
                commands.entry(name.as_ref()).or_default().push(index);
            }
        }
        for key in manifest.provides.iter().chain(&manifest.requires) {
            match ids.get(key.id()) {
                Some(existing) if existing != key => {
                    if !conflicting.contains(&key.id()) {
                        conflicting.push(key.id());
                    }
                }
                Some(_) => {}
                None => {
                    ids.insert(key.id(), *key);
                }
            }
        }
    }

    let mut command_names: Vec<&&str> = commands.keys().collect();
    command_names.sort();
    for name in command_names {
        let users = &commands[*name];
        if users.len() > 1 {
            let error = GraphError::DuplicateCommand {
                command: Name::Owned((**name).to_owned()),
                plugins: users.iter().map(|&i| entries[i].0.clone()).collect(),
            };
            for &user in &users[1..] {
                blamed.push((user, error.clone()));
            }
            errors.push(error);
        }
    }

    for id in conflicting {
        let error = GraphError::ConflictingServiceId(id);
        for (index, (_, manifest)) in entries.iter().enumerate() {
            if manifest.is_some_and(|m| m.provides.iter().chain(&m.requires).any(|k| k.id() == id))
            {
                blamed.push((index, error.clone()));
            }
        }
        errors.push(error);
    }

    let providers = |key: &ServiceKey| -> Vec<usize> {
        entries
            .iter()
            .enumerate()
            .filter(|(_, (_, m))| m.is_some_and(|m| m.provides.contains(key)))
            .map(|(i, _)| i)
            .collect()
    };

    let mut reported_multiple: Vec<ServiceKey> = Vec::new();
    for (index, (id, manifest)) in entries.iter().enumerate() {
        let Some(manifest) = manifest else { continue };
        for key in &manifest.requires {
            if providers(key).iter().all(|&p| p == index) {
                errors.push(GraphError::MissingProvider {
                    plugin: id.clone(),
                    service: key.id(),
                });
            }
        }
        for key in &manifest.provides {
            let all = providers(key);
            if !key.multiple() && all.len() > 1 && !reported_multiple.contains(key) {
                reported_multiple.push(*key);
                let error = GraphError::MultipleProviders {
                    service: key.id(),
                    plugins: all.iter().map(|&i| entries[i].0.clone()).collect(),
                };
                for &p in &all[1..] {
                    blamed.push((p, error.clone()));
                }
                errors.push(error);
            }
        }
    }

    // Topological order. Each requirement lists its providers; providers of
    // one service are alternatives, so a requirement is met once any of them
    // is placed. Nodes whose every provider is placed go first, which keeps
    // providers ahead of their dependents whenever the graph allows it; the
    // weaker rule only breaks ties that would otherwise look like a cycle
    // (a `MULTIPLE` provider that itself depends on the consumer).
    let n = entries.len();
    let requirements: Vec<Vec<Vec<usize>>> = entries
        .iter()
        .enumerate()
        .map(|(index, (_, manifest))| {
            manifest
                .map(|m| {
                    m.requires
                        .iter()
                        .map(|key| providers(key).into_iter().filter(|&p| p != index).collect())
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    let mut placed = vec![false; n];
    let mut order = Vec::new();
    loop {
        let candidate = |i: usize, strict: bool| {
            !placed[i]
                && entries[i].1.is_some()
                && requirements[i].iter().all(|alternatives| {
                    let usable: Vec<usize> = alternatives
                        .iter()
                        .copied()
                        .filter(|&p| entries[p].1.is_some())
                        .collect();
                    usable.is_empty()
                        || if strict {
                            usable.iter().all(|&p| placed[p])
                        } else {
                            usable.iter().any(|&p| placed[p])
                        }
                })
        };
        let next = (0..n)
            .find(|&i| candidate(i, true))
            .or_else(|| (0..n).find(|&i| candidate(i, false)));
        match next {
            Some(i) => {
                placed[i] = true;
                order.push(i);
            }
            None => break,
        }
    }
    let cyclic: Vec<usize> = (0..n)
        .filter(|&i| !placed[i] && entries[i].1.is_some())
        .collect();
    if !cyclic.is_empty() {
        let error = GraphError::Cycle(cyclic.iter().map(|&i| entries[i].0.clone()).collect());
        for &i in &cyclic {
            blamed.push((i, error.clone()));
        }
        errors.push(error);
    }

    GraphReport {
        errors,
        blamed,
        order,
    }
}
