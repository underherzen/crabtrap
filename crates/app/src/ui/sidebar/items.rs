//! Плоский список строк боковой панели (для виртуализированного списка):
//! секции, «All»/«Pinned», домены и их раздел.

use std::collections::{BTreeSet, HashSet};

use crate::ui::traffic::apps::AppNode;
use crate::ui::traffic::domains::DomainNode;
use crate::ui::traffic::scope::Scope;

/// Строка боковой панели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarItem {
    Section(&'static str),
    /// Подсказка вместо пустого списка доменов.
    Hint(&'static str),
    All {
        count: usize,
    },
    Pinned {
        count: usize,
    },
    Host {
        host: String,
        count: usize,
        pinned: bool,
        expanded: bool,
        has_paths: bool,
    },
    Path {
        host: String,
        prefix: String,
        count: usize,
    },
    App {
        name: String,
        count: usize,
        icon_key: Option<String>,
        remote: bool,
    },
}

impl SidebarItem {
    /// Пункт выбирает этот scope (секции не выбираются).
    pub fn scope(&self) -> Option<Scope> {
        match self {
            Self::Section(_) | Self::Hint(_) => None,
            Self::All { .. } => Some(Scope::All),
            Self::Pinned { .. } => Some(Scope::Pinned),
            Self::Host { host, .. } => Some(Scope::Host(host.clone())),
            Self::Path { host, prefix, .. } => Some(Scope::Path {
                host: host.clone(),
                prefix: prefix.clone(),
            }),
            Self::App { name, .. } => Some(Scope::App(name.clone())),
        }
    }
}

fn push_host(
    items: &mut Vec<SidebarItem>,
    node: Option<&DomainNode>,
    host: &str,
    pinned: bool,
    expanded: &HashSet<String>,
) {
    let is_expanded = expanded.contains(host);
    let paths = node.map(|n| n.paths.as_slice()).unwrap_or_default();
    items.push(SidebarItem::Host {
        host: host.to_string(),
        count: node.map_or(0, |n| n.count),
        pinned,
        expanded: is_expanded,
        has_paths: !paths.is_empty(),
    });
    if is_expanded {
        items.extend(paths.iter().map(|p| SidebarItem::Path {
            host: host.to_string(),
            prefix: p.prefix.clone(),
            count: p.count,
        }));
    }
}

/// Строки панели: Library (All, Pinned) → Pinned (закреплённые домены,
/// даже без трафика) → Apps (клиенты) → Domains (остальные).
pub fn sidebar_items(
    apps: &[AppNode],
    domains: &[DomainNode],
    total: usize,
    pinned: &BTreeSet<String>,
    expanded: &HashSet<String>,
) -> Vec<SidebarItem> {
    let pinned_count = domains
        .iter()
        .filter(|d| pinned.contains(&d.host))
        .map(|d| d.count)
        .sum();
    let mut items = vec![
        SidebarItem::Section("Library"),
        SidebarItem::All { count: total },
        SidebarItem::Pinned {
            count: pinned_count,
        },
    ];
    if !pinned.is_empty() {
        items.push(SidebarItem::Section("Pinned"));
        for host in pinned {
            let node = domains.iter().find(|d| &d.host == host);
            push_host(&mut items, node, host, true, expanded);
        }
    }
    if !apps.is_empty() {
        items.push(SidebarItem::Section("Apps"));
        items.extend(apps.iter().map(|app| SidebarItem::App {
            name: app.name.clone(),
            count: app.count,
            icon_key: app.icon_key.clone(),
            remote: app.remote,
        }));
    }
    let rest: Vec<&DomainNode> = domains
        .iter()
        .filter(|d| !pinned.contains(&d.host))
        .collect();
    if domains.is_empty() {
        items.push(SidebarItem::Section("Domains"));
        items.push(SidebarItem::Hint("Domains appear as traffic arrives"));
    } else if !rest.is_empty() {
        items.push(SidebarItem::Section("Domains"));
        for node in rest {
            push_host(&mut items, Some(node), &node.host, false, expanded);
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::traffic::domains::PathNode;

    fn node(host: &str, paths: &[(&str, usize)]) -> DomainNode {
        DomainNode {
            host: host.into(),
            count: paths.iter().map(|(_, c)| c).sum(),
            paths: paths
                .iter()
                .map(|(p, c)| PathNode {
                    prefix: (*p).into(),
                    count: *c,
                })
                .collect(),
        }
    }

    #[test]
    fn library_then_pinned_then_other_domains() {
        let domains = vec![node("a.test", &[("/v1", 2)]), node("b.test", &[("/", 1)])];
        let pinned = BTreeSet::from(["b.test".to_string(), "c.test".to_string()]);
        let items = sidebar_items(&[], &domains, 3, &pinned, &HashSet::new());
        assert_eq!(
            items,
            vec![
                SidebarItem::Section("Library"),
                SidebarItem::All { count: 3 },
                SidebarItem::Pinned { count: 1 },
                SidebarItem::Section("Pinned"),
                SidebarItem::Host {
                    host: "b.test".into(),
                    count: 1,
                    pinned: true,
                    expanded: false,
                    has_paths: true
                },
                SidebarItem::Host {
                    host: "c.test".into(),
                    count: 0,
                    pinned: true,
                    expanded: false,
                    has_paths: false
                },
                SidebarItem::Section("Domains"),
                SidebarItem::Host {
                    host: "a.test".into(),
                    count: 2,
                    pinned: false,
                    expanded: false,
                    has_paths: true
                },
            ]
        );
    }

    #[test]
    fn empty_traffic_shows_a_hint() {
        let items = sidebar_items(&[], &[], 0, &BTreeSet::new(), &HashSet::new());
        assert_eq!(
            items.last(),
            Some(&SidebarItem::Hint("Domains appear as traffic arrives"))
        );
    }

    #[test]
    fn expanded_host_lists_its_paths() {
        let domains = vec![node("a.test", &[("/static", 1), ("/v1", 2)])];
        let expanded = HashSet::from(["a.test".to_string()]);
        let items = sidebar_items(&[], &domains, 3, &BTreeSet::new(), &expanded);
        let paths: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                SidebarItem::Path { prefix, count, .. } => Some((prefix.as_str(), *count)),
                _ => None,
            })
            .collect();
        assert_eq!(paths, [("/static", 1), ("/v1", 2)]);
    }

    #[test]
    fn apps_section_sits_between_pinned_and_domains() {
        let apps = vec![AppNode {
            name: "Safari".into(),
            count: 2,
            icon_key: None,
            remote: false,
        }];
        let domains = vec![node("a.test", &[("/", 2)])];
        let items = sidebar_items(&apps, &domains, 2, &BTreeSet::new(), &HashSet::new());
        let sections: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                SidebarItem::Section(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(sections, ["Library", "Apps", "Domains"]);
        assert!(
            items
                .iter()
                .any(|i| i.scope() == Some(Scope::App("Safari".into())))
        );
    }

    #[test]
    fn items_map_to_scopes() {
        assert_eq!(SidebarItem::Section("x").scope(), None);
        assert_eq!(SidebarItem::All { count: 0 }.scope(), Some(Scope::All));
        let path = SidebarItem::Path {
            host: "a".into(),
            prefix: "/v1".into(),
            count: 1,
        };
        assert_eq!(
            path.scope(),
            Some(Scope::Path {
                host: "a".into(),
                prefix: "/v1".into()
            })
        );
    }
}
