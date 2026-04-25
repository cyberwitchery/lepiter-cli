//! small helpers shared across tui modules.

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::hash::Hash;

pub fn cache_limit_from_env(var: &str, default: usize) -> usize {
    env::var(var)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
        .max(1)
}

pub fn cache_limit_from_env_allow_zero(var: &str, default: usize) -> usize {
    env::var(var)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

pub struct LruCache<K, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    max_entries: usize,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash + Clone,
{
    pub fn new(max_entries: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            max_entries,
        }
    }

    /// Look up a key and touch it in the LRU order.
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        if self.map.contains_key(key) {
            self.touch_order(key);
            self.map.get(key)
        } else {
            None
        }
    }

    /// Look up a key without touching the LRU order.
    pub fn peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.map.get(key)
    }

    /// Mark a key as recently used. Returns whether the key was present.
    pub fn touch<Q>(&mut self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        if self.map.contains_key(key) {
            self.touch_order(key);
            true
        } else {
            false
        }
    }

    /// Insert a value, evicting the oldest entry if at capacity.
    /// A capacity of zero means caching is disabled; inserts are rejected.
    pub fn insert(&mut self, key: K, value: V) {
        if self.max_entries == 0 {
            return;
        }

        if self.map.contains_key(&key) {
            self.map.insert(key.clone(), value);
            self.touch_order(&key);
            return;
        }

        if self.map.len() >= self.max_entries
            && let Some(oldest) = self.order.pop_front()
        {
            self.map.remove(&oldest);
        }

        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn capacity(&self) -> usize {
        self.max_entries
    }

    fn touch_order<Q>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        if let Some(pos) = self.order.iter().position(|x| x.borrow() == key) {
            let k = self.order.remove(pos).unwrap();
            self.order.push_back(k);
        }
    }
}
