//! Дерево доменов для боковой панели: домен → разделы (первый сегмент
//! пути) со счётчиками обменов. Счётчики ведутся инкрементально
//! ([`DomainCounts`]: +строка / −вытесненная строка), отсортированный
//! снимок пересобирается только когда счётчики изменились.

use std::collections::{BTreeMap, HashMap};

use super::scope::first_segment;
use super::store::TrafficRow;

/// Раздел домена (`/v1`) и число обменов в нём.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathNode {
    pub prefix: String,
    pub count: usize,
}

/// Домен, число обменов и его разделы (по алфавиту).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainNode {
    pub host: String,
    pub count: usize,
    pub paths: Vec<PathNode>,
}

#[derive(Debug, Default)]
struct HostCount {
    count: usize,
    paths: HashMap<String, usize>,
}

/// Счётчики доменов и разделов + снимок для панели.
#[derive(Debug, Default)]
pub struct DomainCounts {
    hosts: HashMap<String, HostCount>,
    snapshot: Vec<DomainNode>,
    dirty: bool,
}

impl DomainCounts {
    pub fn add(&mut self, row: &TrafficRow) {
        let host = self.hosts.entry(row.summary.host.clone()).or_default();
        host.count += 1;
        *host
            .paths
            .entry(first_segment(&row.summary.path).to_string())
            .or_default() += 1;
        self.dirty = true;
    }

    pub fn remove(&mut self, row: &TrafficRow) {
        let Some(host) = self.hosts.get_mut(&row.summary.host) else {
            return;
        };
        host.count = host.count.saturating_sub(1);
        let prefix = first_segment(&row.summary.path);
        if let Some(count) = host.paths.get_mut(prefix) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                host.paths.remove(prefix);
            }
        }
        if host.count == 0 {
            self.hosts.remove(&row.summary.host);
        }
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        self.hosts.clear();
        self.snapshot.clear();
        self.dirty = false;
    }

    /// Пересобрать снимок, если счётчики менялись (по алфавиту).
    pub fn refresh(&mut self) {
        if !self.dirty {
            return;
        }
        let sorted: BTreeMap<&String, &HostCount> = self.hosts.iter().collect();
        self.snapshot = sorted
            .into_iter()
            .map(|(host, counts)| {
                let paths: BTreeMap<&String, &usize> = counts.paths.iter().collect();
                DomainNode {
                    host: host.clone(),
                    count: counts.count,
                    paths: paths
                        .into_iter()
                        .map(|(prefix, count)| PathNode {
                            prefix: prefix.clone(),
                            count: *count,
                        })
                        .collect(),
                }
            })
            .collect();
        self.dirty = false;
    }

    pub fn snapshot(&self) -> &[DomainNode] {
        &self.snapshot
    }
}

/// Дерево доменов по строкам целиком (тесты, сверка со счётчиками).
#[cfg(test)]
pub fn build_domains(rows: &[TrafficRow]) -> Vec<DomainNode> {
    let mut counts = DomainCounts::default();
    rows.iter().for_each(|row| counts.add(row));
    counts.refresh();
    counts.snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::traffic::store::test_row;

    #[test]
    fn groups_by_host_and_first_segment() {
        let rows = vec![
            test_row("b.test", "/v1/users"),
            test_row("a.test", "/"),
            test_row("b.test", "/v1/orders?x=1"),
            test_row("b.test", "/static/app.js"),
        ];
        let domains = build_domains(&rows);
        assert_eq!(
            domains
                .iter()
                .map(|d| (d.host.as_str(), d.count))
                .collect::<Vec<_>>(),
            [("a.test", 1), ("b.test", 3)]
        );
        assert_eq!(
            domains[1].paths,
            [
                PathNode {
                    prefix: "/static".into(),
                    count: 1
                },
                PathNode {
                    prefix: "/v1".into(),
                    count: 2
                },
            ]
        );
    }

    #[test]
    fn empty_rows_give_empty_tree() {
        assert!(build_domains(&[]).is_empty());
    }

    #[test]
    fn removing_rows_decrements_and_drops_empty_entries() {
        let rows = [
            test_row("a.test", "/v1/x"),
            test_row("a.test", "/v2/y"),
            test_row("b.test", "/"),
        ];
        let mut counts = DomainCounts::default();
        rows.iter().for_each(|r| counts.add(r));
        counts.remove(&rows[0]);
        counts.remove(&rows[2]);
        counts.refresh();
        assert_eq!(
            counts.snapshot(),
            [DomainNode {
                host: "a.test".into(),
                count: 1,
                paths: vec![PathNode {
                    prefix: "/v2".into(),
                    count: 1
                }],
            }]
        );
    }
}
