//! small helpers shared across tui modules.

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
