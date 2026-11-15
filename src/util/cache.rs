//! A small TTL cache for the per-message hot paths.
//!
//! Every message the bot sees used to cost a dozen or more SQLite round trips,
//! almost all of them re-reading configuration that changes once a month. The
//! caches built on this type sit in front of those reads. Writes go through
//! [`crate::db`]'s setters, which invalidate the matching entry, so a `/config`
//! change still takes effect immediately; the TTL is only a backstop for rows
//! that something else edited.

use std::borrow::Borrow;
use std::hash::Hash;
use std::time::{Duration, Instant};

use dashmap::DashMap;

pub struct TtlCache<K, V> {
    entries: DashMap<K, (V, Instant)>,
    ttl: Duration,
    /// Soft upper bound on entries. Reaching it triggers a prune, so a bot in a
    /// lot of guilds cannot grow this without limit.
    capacity: usize,
}

impl<K, V> TtlCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    pub fn new(ttl_secs: u64, capacity: usize) -> Self {
        Self {
            entries: DashMap::new(),
            ttl: Duration::from_secs(ttl_secs),
            capacity: capacity.max(1),
        }
    }

    /// The cached value, if there is one and it has not expired.
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let entry = self.entries.get(key)?;
        if entry.1.elapsed() >= self.ttl {
            return None;
        }
        Some(entry.0.clone())
    }

    pub fn put(&self, key: K, value: V) {
        if self.entries.len() >= self.capacity {
            self.prune();
        }
        self.entries.insert(key, (value, Instant::now()));
    }

    pub fn invalidate<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entries.remove(key);
    }

    pub fn clear(&self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop expired entries. If that frees nothing we are over capacity with
    /// live entries, so drop the lot rather than grow: everything here is
    /// re-readable from the database.
    pub fn prune(&self) {
        let ttl = self.ttl;
        self.entries.retain(|_, value| value.1.elapsed() < ttl);
        if self.entries.len() >= self.capacity {
            self.entries.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_then_expires() {
        let cache: TtlCache<i64, String> = TtlCache::new(0, 16);
        cache.put(1, "a".to_string());
        // A zero second TTL means every read is already stale.
        assert_eq!(cache.get(&1), None);

        let cache: TtlCache<i64, String> = TtlCache::new(60, 16);
        cache.put(1, "a".to_string());
        assert_eq!(cache.get(&1), Some("a".to_string()));
        cache.invalidate(&1);
        assert_eq!(cache.get(&1), None);
    }

    #[test]
    fn stays_within_capacity() {
        let cache: TtlCache<i64, i64> = TtlCache::new(600, 4);
        for i in 0..50 {
            cache.put(i, i);
        }
        assert!(cache.len() <= 4, "cache grew to {}", cache.len());
    }
}
