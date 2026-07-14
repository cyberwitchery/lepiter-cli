//! small helpers shared across tui modules.

use std::borrow::Borrow;
use std::env;
use std::hash::Hash;

use indexmap::IndexMap;

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

/// Map a byte offset in `raw.to_lowercase()` to the corresponding byte offset
/// in `raw`. Walks both strings' characters in tandem so the result is correct
/// even when lowercasing changes byte lengths (e.g. multi-byte characters).
pub fn lower_byte_to_raw_byte(raw: &str, lower_pos: usize) -> usize {
    let mut raw_byte = 0usize;
    let mut lower_byte = 0usize;
    for ch in raw.chars() {
        if lower_byte >= lower_pos {
            return raw_byte;
        }
        raw_byte += ch.len_utf8();
        for lch in ch.to_lowercase() {
            lower_byte += lch.len_utf8();
        }
    }
    raw_byte
}

pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    let mut chars = input.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(c) = chars.next() else {
            return out;
        };
        out.push(c);
    }
    if chars.next().is_some() && max_chars >= 1 {
        out.pop();
        out.push('…');
    }
    out
}

/// Build a short context snippet around the first case-insensitive occurrence
/// of `needle_lower` within `raw`.
///
/// `needle_lower` must already be lowercased. The window spans roughly 40 bytes
/// before the match and 80 after, snapped to `char` boundaries, with newlines
/// flattened to spaces and the result trimmed and capped at 120 characters.
/// Returns `None` when `needle_lower` is empty, does not appear in `raw`, or the
/// surrounding window is blank after trimming.
pub fn matching_snippet(raw: &str, needle_lower: &str) -> Option<String> {
    if needle_lower.is_empty() {
        return None;
    }
    let lower = raw.to_lowercase();
    let lower_idx = lower.find(needle_lower)?;

    // Map byte offsets from the lowered text back to the raw text — lowercasing
    // can change byte lengths for non-ASCII characters.
    let raw_match = lower_byte_to_raw_byte(raw, lower_idx);
    let raw_end = lower_byte_to_raw_byte(raw, lower_idx + needle_lower.len());

    let start = raw.floor_char_boundary(raw_match.saturating_sub(40));
    let end = raw.ceil_char_boundary((raw_end + 80).min(raw.len()));
    let fragment = raw[start..end].replace('\n', " ");
    let fragment = fragment.trim();
    if fragment.is_empty() {
        None
    } else {
        Some(truncate_chars(fragment, 120))
    }
}

pub struct LruCache<K, V> {
    map: IndexMap<K, V>,
    max_entries: usize,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash,
{
    pub fn new(max_entries: usize) -> Self {
        Self {
            map: IndexMap::new(),
            max_entries,
        }
    }

    /// Look up a key and touch it in the LRU order.
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        if let Some(index) = self.map.get_index_of(key) {
            self.move_to_back(index);
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
        if let Some(index) = self.map.get_index_of(key) {
            self.move_to_back(index);
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

        if let Some(index) = self.map.get_index_of(&key) {
            *self.map.get_index_mut(index).unwrap().1 = value;
            self.move_to_back(index);
            return;
        }

        if self.map.len() >= self.max_entries {
            self.map.shift_remove_index(0);
        }

        self.map.insert(key, value);
    }

    /// Check whether a key is present without touching LRU order.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.map.contains_key(key)
    }

    /// Iterate over all entries without touching LRU order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.map.iter()
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn capacity(&self) -> usize {
        self.max_entries
    }

    fn move_to_back(&mut self, index: usize) {
        let last = self.map.len() - 1;
        if index != last {
            self.map.move_index(index, last);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── cache_limit_from_env ───────────────────────────────────────

    #[test]
    fn cache_limit_from_env_uses_default_when_unset() {
        // Use a variable name unlikely to be set in the environment.
        let val = cache_limit_from_env("__LEPITER_TEST_UNSET_12345__", 42);
        assert_eq!(val, 42);
    }

    #[test]
    fn cache_limit_from_env_parses_valid_value() {
        unsafe { env::set_var("__LEPITER_TEST_CACHE_VALID__", "64") };
        let val = cache_limit_from_env("__LEPITER_TEST_CACHE_VALID__", 10);
        assert_eq!(val, 64);
        unsafe { env::remove_var("__LEPITER_TEST_CACHE_VALID__") };
    }

    #[test]
    fn cache_limit_from_env_clamps_zero_to_one() {
        unsafe { env::set_var("__LEPITER_TEST_CACHE_ZERO__", "0") };
        let val = cache_limit_from_env("__LEPITER_TEST_CACHE_ZERO__", 10);
        assert_eq!(val, 1);
        unsafe { env::remove_var("__LEPITER_TEST_CACHE_ZERO__") };
    }

    #[test]
    fn cache_limit_from_env_falls_back_on_non_numeric() {
        unsafe { env::set_var("__LEPITER_TEST_CACHE_BAD__", "not_a_number") };
        let val = cache_limit_from_env("__LEPITER_TEST_CACHE_BAD__", 99);
        assert_eq!(val, 99);
        unsafe { env::remove_var("__LEPITER_TEST_CACHE_BAD__") };
    }

    #[test]
    fn cache_limit_from_env_clamps_default_zero_to_one() {
        let val = cache_limit_from_env("__LEPITER_TEST_UNSET_99999__", 0);
        assert_eq!(val, 1);
    }

    // ── cache_limit_from_env_allow_zero ────────────────────────────

    #[test]
    fn cache_limit_allow_zero_uses_default_when_unset() {
        let val = cache_limit_from_env_allow_zero("__LEPITER_TEST_AZ_UNSET__", 50);
        assert_eq!(val, 50);
    }

    #[test]
    fn cache_limit_allow_zero_permits_zero() {
        unsafe { env::set_var("__LEPITER_TEST_AZ_ZERO__", "0") };
        let val = cache_limit_from_env_allow_zero("__LEPITER_TEST_AZ_ZERO__", 10);
        assert_eq!(val, 0);
        unsafe { env::remove_var("__LEPITER_TEST_AZ_ZERO__") };
    }

    #[test]
    fn cache_limit_allow_zero_parses_valid_value() {
        unsafe { env::set_var("__LEPITER_TEST_AZ_VALID__", "256") };
        let val = cache_limit_from_env_allow_zero("__LEPITER_TEST_AZ_VALID__", 10);
        assert_eq!(val, 256);
        unsafe { env::remove_var("__LEPITER_TEST_AZ_VALID__") };
    }

    #[test]
    fn cache_limit_allow_zero_default_zero_stays_zero() {
        let val = cache_limit_from_env_allow_zero("__LEPITER_TEST_AZ_DEF0__", 0);
        assert_eq!(val, 0);
    }

    // ── LruCache::new / capacity / len ─────────────────────────────

    #[test]
    fn new_cache_is_empty() {
        let cache: LruCache<String, i32> = LruCache::new(10);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), 10);
    }

    #[test]
    fn zero_capacity_cache() {
        let cache: LruCache<String, i32> = LruCache::new(0);
        assert_eq!(cache.capacity(), 0);
        assert_eq!(cache.len(), 0);
    }

    // ── insert / get / peek ────────────────────────────────────────

    #[test]
    fn insert_and_get() {
        let mut cache = LruCache::new(10);
        cache.insert("a".to_string(), 1);
        assert_eq!(cache.get("a"), Some(&1));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let mut cache: LruCache<String, i32> = LruCache::new(10);
        cache.insert("a".to_string(), 1);
        assert_eq!(cache.get("b"), None);
    }

    #[test]
    fn peek_does_not_change_order() {
        let mut cache = LruCache::new(3);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        cache.insert("c".to_string(), 3);

        // Peek at "a" — should NOT move it to back.
        assert_eq!(cache.peek("a"), Some(&1));

        // Insert "d" — should evict "a" (oldest, since peek didn't touch it).
        cache.insert("d".to_string(), 4);
        assert_eq!(cache.peek("a"), None);
        assert_eq!(cache.peek("b"), Some(&2));
    }

    #[test]
    fn get_updates_lru_order() {
        let mut cache = LruCache::new(3);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        cache.insert("c".to_string(), 3);

        // Access "a" via get — moves it to most-recently-used.
        cache.get("a");

        // Insert "d" — should evict "b" (now the oldest).
        cache.insert("d".to_string(), 4);
        assert_eq!(cache.peek("a"), Some(&1));
        assert_eq!(cache.peek("b"), None);
        assert_eq!(cache.peek("c"), Some(&3));
        assert_eq!(cache.peek("d"), Some(&4));
    }

    // ── insert: update existing key ────────────────────────────────

    #[test]
    fn insert_existing_key_updates_value() {
        let mut cache = LruCache::new(10);
        cache.insert("key".to_string(), 1);
        cache.insert("key".to_string(), 2);
        assert_eq!(cache.get("key"), Some(&2));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn insert_existing_key_refreshes_order() {
        let mut cache = LruCache::new(3);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        cache.insert("c".to_string(), 3);

        // Re-insert "a" with a new value — should move it to back.
        cache.insert("a".to_string(), 10);

        // Insert "d" — should evict "b" (now oldest).
        cache.insert("d".to_string(), 4);
        assert_eq!(cache.peek("a"), Some(&10));
        assert_eq!(cache.peek("b"), None);
    }

    // ── eviction ───────────────────────────────────────────────────

    #[test]
    fn eviction_at_capacity() {
        let mut cache = LruCache::new(2);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        assert_eq!(cache.len(), 2);

        // Third insert should evict "a".
        cache.insert("c".to_string(), 3);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.peek("a"), None);
        assert_eq!(cache.peek("b"), Some(&2));
        assert_eq!(cache.peek("c"), Some(&3));
    }

    #[test]
    fn eviction_chain() {
        let mut cache = LruCache::new(1);
        cache.insert("a".to_string(), 1);
        assert_eq!(cache.get("a"), Some(&1));

        cache.insert("b".to_string(), 2);
        assert_eq!(cache.get("a"), None);
        assert_eq!(cache.get("b"), Some(&2));
        assert_eq!(cache.len(), 1);

        cache.insert("c".to_string(), 3);
        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("c"), Some(&3));
    }

    // ── zero capacity: inserts rejected ────────────────────────────

    #[test]
    fn zero_capacity_rejects_inserts() {
        let mut cache = LruCache::new(0);
        cache.insert("a".to_string(), 1);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.get("a"), None);
        assert_eq!(cache.peek("a"), None);
    }

    // ── touch ──────────────────────────────────────────────────────

    #[test]
    fn touch_present_key_returns_true() {
        let mut cache = LruCache::new(10);
        cache.insert("a".to_string(), 1);
        assert!(cache.touch("a"));
    }

    #[test]
    fn touch_absent_key_returns_false() {
        let mut cache: LruCache<String, i32> = LruCache::new(10);
        assert!(!cache.touch("missing"));
    }

    #[test]
    fn touch_refreshes_lru_order() {
        let mut cache = LruCache::new(3);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        cache.insert("c".to_string(), 3);

        // Touch "a" — makes it most-recently-used.
        cache.touch("a");

        // Insert "d" — should evict "b".
        cache.insert("d".to_string(), 4);
        assert_eq!(cache.peek("a"), Some(&1));
        assert_eq!(cache.peek("b"), None);
    }

    // ── borrowed key lookups ───────────────────────────────────────

    #[test]
    fn get_with_str_key() {
        let mut cache = LruCache::new(10);
        cache.insert("hello".to_string(), 42);
        // Look up with &str, not String.
        assert_eq!(cache.get("hello"), Some(&42));
    }

    #[test]
    fn peek_with_str_key() {
        let mut cache = LruCache::new(10);
        cache.insert("hello".to_string(), 42);
        assert_eq!(cache.peek("hello"), Some(&42));
    }

    #[test]
    fn touch_with_str_key() {
        let mut cache = LruCache::new(10);
        cache.insert("hello".to_string(), 42);
        assert!(cache.touch("hello"));
    }

    // ── contains_key ───────────────────────────────────────────────

    #[test]
    fn contains_key_present() {
        let mut cache = LruCache::new(10);
        cache.insert("a".to_string(), 1);
        assert!(cache.contains_key("a"));
    }

    #[test]
    fn contains_key_absent() {
        let cache: LruCache<String, i32> = LruCache::new(10);
        assert!(!cache.contains_key("missing"));
    }

    #[test]
    fn contains_key_after_eviction() {
        let mut cache = LruCache::new(2);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        cache.insert("c".to_string(), 3);
        assert!(!cache.contains_key("a"));
        assert!(cache.contains_key("b"));
        assert!(cache.contains_key("c"));
    }

    // ── iter ──────────────────────────────────────────────────────

    #[test]
    fn iter_empty_cache() {
        let cache: LruCache<String, i32> = LruCache::new(10);
        assert_eq!(cache.iter().count(), 0);
    }

    #[test]
    fn iter_returns_all_entries() {
        let mut cache = LruCache::new(10);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        cache.insert("c".to_string(), 3);
        let mut entries: Vec<_> = cache.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        entries.sort();
        assert_eq!(entries, vec![("a", 1), ("b", 2), ("c", 3)]);
    }

    #[test]
    fn iter_does_not_touch_order() {
        let mut cache = LruCache::new(3);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        cache.insert("c".to_string(), 3);

        // Iterate (should not change order).
        let _: Vec<_> = cache.iter().collect();

        // Insert "d" — should evict "a" (still oldest).
        cache.insert("d".to_string(), 4);
        assert!(!cache.contains_key("a"));
        assert!(cache.contains_key("b"));
    }

    // ── integer keys ───────────────────────────────────────────────

    #[test]
    fn integer_keys_work() {
        let mut cache = LruCache::new(3);
        cache.insert(1, "one");
        cache.insert(2, "two");
        cache.insert(3, "three");
        assert_eq!(cache.get(&1), Some(&"one"));

        cache.insert(4, "four");
        assert_eq!(cache.peek(&1), Some(&"one")); // 1 was touched by get
        assert_eq!(cache.peek(&2), None); // 2 was evicted
    }

    // ── matching_snippet ───────────────────────────────────────────

    #[test]
    fn matching_snippet_returns_context_around_match() {
        let raw = "the quick brown fox jumps over the lazy dog";
        let snippet = matching_snippet(raw, "fox").unwrap();
        assert!(snippet.contains("fox"));
        assert!(snippet.contains("quick"));
    }

    #[test]
    fn matching_snippet_is_case_insensitive() {
        let raw = "The Quick Brown FOX";
        let snippet = matching_snippet(raw, "fox").unwrap();
        assert!(snippet.contains("FOX"));
    }

    #[test]
    fn matching_snippet_absent_needle_returns_none() {
        assert!(matching_snippet("the quick brown fox", "zzz").is_none());
    }

    #[test]
    fn matching_snippet_empty_needle_returns_none() {
        assert!(matching_snippet("some text", "").is_none());
    }

    #[test]
    fn matching_snippet_flattens_newlines() {
        let snippet = matching_snippet("alpha\nbeta needle gamma", "needle").unwrap();
        assert!(!snippet.contains('\n'));
        assert!(snippet.contains("beta needle gamma"));
    }

    #[test]
    fn matching_snippet_truncates_to_120_chars() {
        // A long run after the match should be capped (window is +80 bytes and
        // the final cap is 120 chars, so a long tail is truncated).
        let raw = format!("needle {}", "x".repeat(500));
        let snippet = matching_snippet(&raw, "needle").unwrap();
        assert!(snippet.chars().count() <= 120);
    }

    #[test]
    fn matching_snippet_preserves_accented_content() {
        let raw = "café münchen straße — needle here";
        let snippet = matching_snippet(raw, "needle").unwrap();
        assert!(snippet.contains("needle"));
        assert!(snippet.contains("straße"));
    }

    #[test]
    fn matching_snippet_multibyte_before_match_stays_on_boundary() {
        // '\u{212A}' (Kelvin sign, 3 bytes) lowercases to ASCII 'k' (1 byte),
        // so lowered byte offsets diverge from raw ones; the mapping must land
        // on a char boundary or slicing the raw text would panic.
        let raw = "\u{212A}elvin scale, needle tail";
        let snippet = matching_snippet(raw, "needle").unwrap();
        assert!(snippet.contains("needle"));
    }
}
