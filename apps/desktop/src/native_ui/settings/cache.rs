//! 设置页有界缓存。

use super::contracts::{SettingsPage, SettingsSnapshot};
use std::collections::VecDeque;

/// 只缓存最近访问的少量设置页。页面切换不会保留无限增长的 JSON/日志投影，
/// domain 事件也可以按页失效，避免把过期快照当作已确认状态。
#[derive(Clone, Debug)]
pub struct BoundedSettingsCache {
    capacity: usize,
    entries: VecDeque<(SettingsPage, SettingsSnapshot)>,
}

impl BoundedSettingsCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: VecDeque::new(),
        }
    }

    pub fn get(&mut self, page: SettingsPage) -> Option<SettingsSnapshot> {
        let index = self.entries.iter().position(|(key, _)| *key == page)?;
        let item = self.entries.remove(index)?;
        let snapshot = item.1.clone();
        self.entries.push_back(item);
        Some(snapshot)
    }

    pub fn insert(&mut self, page: SettingsPage, snapshot: SettingsSnapshot) {
        self.entries.retain(|(key, _)| *key != page);
        self.entries.push_back((page, snapshot));
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }

    pub fn invalidate(&mut self, page: SettingsPage) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(key, _)| *key != page);
        before != self.entries.len()
    }
}

impl Default for BoundedSettingsCache {
    fn default() -> Self {
        Self::new(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(page: SettingsPage, revision: u64) -> SettingsSnapshot {
        SettingsSnapshot {
            page,
            revision,
            ..SettingsSnapshot::default()
        }
    }

    #[test]
    fn keeps_recent_pages_with_a_bounded_capacity() {
        let mut cache = BoundedSettingsCache::new(2);
        cache.insert(SettingsPage::General, snapshot(SettingsPage::General, 1));
        cache.insert(
            SettingsPage::Providers,
            snapshot(SettingsPage::Providers, 2),
        );
        assert_eq!(cache.get(SettingsPage::General).unwrap().revision, 1);
        cache.insert(
            SettingsPage::Resources,
            snapshot(SettingsPage::Resources, 3),
        );
        assert!(cache.get(SettingsPage::Providers).is_none());
        assert_eq!(cache.get(SettingsPage::General).unwrap().revision, 1);
        assert_eq!(cache.get(SettingsPage::Resources).unwrap().revision, 3);
    }

    #[test]
    fn invalidate_only_removes_the_target_page() {
        let mut cache = BoundedSettingsCache::default();
        cache.insert(SettingsPage::General, snapshot(SettingsPage::General, 1));
        cache.insert(SettingsPage::Agents, snapshot(SettingsPage::Agents, 2));
        assert!(cache.invalidate(SettingsPage::General));
        assert!(!cache.invalidate(SettingsPage::General));
        assert!(cache.get(SettingsPage::Agents).is_some());
    }
}
