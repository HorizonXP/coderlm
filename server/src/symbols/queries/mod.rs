pub mod elixir;
pub mod go;
pub mod java;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod scala;
pub mod typescript;
pub mod zig;

use crate::index::file_entry::Language;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKind {
    Symbols,
    Callers,
    Variables,
    NonCode,
    ElixirExUnitBlocks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct QueryCacheKey {
    language: Language,
    kind: QueryKind,
}

pub struct CachedQuery {
    query: tree_sitter::Query,
    capture_names: Vec<String>,
}

impl CachedQuery {
    pub fn query(&self) -> &tree_sitter::Query {
        &self.query
    }

    pub fn capture_names(&self) -> &[String] {
        &self.capture_names
    }
}

type QueryCache = HashMap<QueryCacheKey, Arc<CachedQuery>>;

static QUERY_CACHE: OnceLock<Mutex<QueryCache>> = OnceLock::new();

fn query_cache() -> &'static Mutex<QueryCache> {
    QUERY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Compile or reuse a tree-sitter query for a stable language/query-kind pair.
///
/// Parsers remain per-operation. Only immutable compiled queries are shared,
/// which tree-sitter exposes as Send + Sync and is safe to use with fresh
/// QueryCursor instances on each call.
pub fn get_cached_query(
    language: Language,
    kind: QueryKind,
    tree_sitter_language: &tree_sitter::Language,
    query_source: &'static str,
) -> Result<Arc<CachedQuery>, tree_sitter::QueryError> {
    let key = QueryCacheKey { language, kind };
    let mut cache = query_cache()
        .lock()
        .expect("tree-sitter query cache mutex poisoned");

    if let Some(query) = cache.get(&key) {
        return Ok(Arc::clone(query));
    }

    let query = tree_sitter::Query::new(tree_sitter_language, query_source)?;
    let capture_names = query
        .capture_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let query = Arc::new(CachedQuery {
        query,
        capture_names,
    });
    cache.insert(key, Arc::clone(&query));

    Ok(query)
}

#[cfg(test)]
pub fn cached_query_count_for_tests(language: Language, kind: QueryKind) -> usize {
    let key = QueryCacheKey { language, kind };
    query_cache()
        .lock()
        .expect("tree-sitter query cache mutex poisoned")
        .contains_key(&key) as usize
}

/// Get the tree-sitter language and symbol query for a given language.
pub fn get_language_config(lang: Language) -> Option<LanguageConfig> {
    match lang {
        Language::Rust => Some(rust::config()),
        Language::Python => Some(python::config()),
        Language::TypeScript => Some(typescript::config()),
        Language::JavaScript => Some(typescript::js_config()),
        Language::Go => Some(go::config()),
        Language::Java => Some(java::config()),
        Language::Scala => Some(scala::config()),
        Language::Elixir => Some(elixir::config()),
        Language::Ruby => Some(ruby::config()),
        Language::Php => Some(php::config()),
        Language::Zig => Some(zig::config()),
        _ => None,
    }
}

#[allow(dead_code)]
pub struct LanguageConfig {
    pub language: tree_sitter::Language,
    pub symbols_query: &'static str,
    /// Tree-sitter query for call expressions. Captures `@callee` for the called name.
    pub callers_query: &'static str,
    /// Tree-sitter query for local variable bindings. Captures `@var.name`.
    pub variables_query: &'static str,
    pub test_patterns: Vec<TestPattern>,
}

#[allow(dead_code)]
pub enum TestPattern {
    /// Match functions whose name starts with a prefix (e.g., "test_" in Python)
    FunctionPrefix(&'static str),
    /// Match functions with a specific attribute/decorator (e.g., #[test] in Rust)
    Attribute(&'static str),
    /// Match call expressions (e.g., it(), test(), describe() in JS/TS)
    CallExpression(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn cached_query_is_send_and_sync() {
        assert_send_sync::<CachedQuery>();
    }

    #[test]
    fn cached_query_reuses_same_language_and_kind() {
        let config = get_language_config(Language::Rust).unwrap();

        let first = get_cached_query(
            Language::Rust,
            QueryKind::Symbols,
            &config.language,
            config.symbols_query,
        )
        .unwrap();
        let second = get_cached_query(
            Language::Rust,
            QueryKind::Symbols,
            &config.language,
            config.symbols_query,
        )
        .unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            cached_query_count_for_tests(Language::Rust, QueryKind::Symbols),
            1
        );
    }

    #[test]
    fn cached_query_keeps_languages_separate() {
        let rust = get_language_config(Language::Rust).unwrap();
        let python = get_language_config(Language::Python).unwrap();

        let rust_query = get_cached_query(
            Language::Rust,
            QueryKind::Symbols,
            &rust.language,
            rust.symbols_query,
        )
        .unwrap();
        let python_query = get_cached_query(
            Language::Python,
            QueryKind::Symbols,
            &python.language,
            python.symbols_query,
        )
        .unwrap();

        assert!(!Arc::ptr_eq(&rust_query, &python_query));
        assert_eq!(
            cached_query_count_for_tests(Language::Rust, QueryKind::Symbols),
            1
        );
        assert_eq!(
            cached_query_count_for_tests(Language::Python, QueryKind::Symbols),
            1
        );
    }

    #[test]
    fn cached_query_is_shared_across_threads() {
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let config = get_language_config(Language::Rust).unwrap();
                    barrier.wait();
                    get_cached_query(
                        Language::Rust,
                        QueryKind::Callers,
                        &config.language,
                        config.callers_query,
                    )
                    .unwrap()
                })
            })
            .collect();

        let mut queries = Vec::new();
        for handle in handles {
            queries.push(handle.join().unwrap());
        }

        for query in queries.iter().skip(1) {
            assert!(Arc::ptr_eq(&queries[0], query));
        }
        assert_eq!(
            cached_query_count_for_tests(Language::Rust, QueryKind::Callers),
            1
        );
    }
}
