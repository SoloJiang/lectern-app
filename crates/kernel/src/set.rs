//! The plugin set and the dependency graph check.

use std::collections::HashMap;
use std::sync::Arc;

use crate::panic::contain;
use crate::plugin::{Activation, GraphError, Manifest, Name, Plugin};
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
        let mut errors = Vec::new();
        let manifests: Vec<(Name, Option<Manifest>)> = self
            .plugins
            .iter()
            .map(|(id, plugin)| match contain(|| plugin.manifest()) {
                Ok(manifest) => (id.clone(), Some(manifest)),
                Err(message) => {
                    errors.push(GraphError::ManifestPanicked {
                        plugin: id.clone(),
                        message,
                    });
                    (id.clone(), None)
                }
            })
            .collect();
        let entries: Vec<(Name, Option<&Manifest>)> = manifests
            .iter()
            .map(|(id, manifest)| (id.clone(), manifest.as_ref()))
            .collect();
        errors.extend(analyze(&entries).errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
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
