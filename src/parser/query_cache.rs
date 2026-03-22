use std::hash::{Hash, Hasher};
use std::sync::Arc;

use dashmap::DashMap;
use tree_sitter::{Language, Query, QueryError};

pub struct TsQueryCache {
    language: Language,
    cache: DashMap<u64, Arc<Query>>,
}

impl std::fmt::Debug for TsQueryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsQueryCache")
            .field("cached_queries", &self.cache.len())
            .finish()
    }
}

impl TsQueryCache {
    pub fn new(language: Language) -> Self {
        Self {
            language,
            cache: DashMap::new(),
        }
    }

    pub fn get_or_compile(&self, pattern: &str) -> Result<Arc<Query>, QueryError> {
        let key = Self::hash_pattern(pattern);
        if let Some(query) = self.cache.get(&key) {
            return Ok(Arc::clone(query.value()));
        }
        let query = Arc::new(Query::new(&self.language, pattern)?);
        self.cache.insert(key, Arc::clone(&query));
        Ok(query)
    }

    fn hash_pattern(pattern: &str) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        pattern.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_caches_query() {
        let language: Language = tree_sitter_cpp::LANGUAGE.into();
        let cache = TsQueryCache::new(language);
        let q1 = cache.get_or_compile("(comment) @c").expect("compile");
        let q2 = cache.get_or_compile("(comment) @c").expect("cached");
        assert!(Arc::ptr_eq(&q1, &q2));
    }

    #[test]
    fn invalid_query_error() {
        let language: Language = tree_sitter_cpp::LANGUAGE.into();
        let cache = TsQueryCache::new(language);
        assert!(cache.get_or_compile("(nonexistent_node_xyz) @x").is_err());
    }

    #[test]
    fn different_queries_cached() {
        let language: Language = tree_sitter_cpp::LANGUAGE.into();
        let cache = TsQueryCache::new(language);
        let q1 = cache.get_or_compile("(comment) @c").expect("compile");
        let q2 = cache
            .get_or_compile("(string_literal) @s")
            .expect("compile");
        assert!(!Arc::ptr_eq(&q1, &q2));
    }
}
