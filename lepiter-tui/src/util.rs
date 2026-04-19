//! small helpers shared across tui modules.

use std::collections::{HashMap, VecDeque};
use std::env;

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

pub struct LruCache<V> {
    map: HashMap<String, V>,
    order: VecDeque<String>,
    max_entries: usize,
}

impl<V> LruCache<V> {
    pub fn new(max_entries: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            max_entries,
        }
    }

    /// Look up a key and touch it in the LRU order.
    pub fn get(&mut self, key: &str) -> Option<&V> {
        if self.map.contains_key(key) {
            self.touch_order(key);
            self.map.get(key)
        } else {
            None
        }
    }

    /// Look up a key without touching the LRU order.
    pub fn peek(&self, key: &str) -> Option<&V> {
        self.map.get(key)
    }

    /// Mark a key as recently used. Returns whether the key was present.
    pub fn touch(&mut self, key: &str) -> bool {
        if self.map.contains_key(key) {
            self.touch_order(key);
            true
        } else {
            false
        }
    }

    /// Insert a value, evicting the oldest entry if at capacity.
    pub fn insert(&mut self, key: String, value: V) {
        if self.map.contains_key(&key) {
            self.map.insert(key.clone(), value);
            self.touch_order(&key);
            return;
        }

        if self.max_entries > 0
            && self.map.len() >= self.max_entries
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

    fn touch_order(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|x| x == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_string());
    }
}
