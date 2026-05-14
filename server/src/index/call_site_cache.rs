use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tree_sitter::StreamingIterator;

use crate::index::file_entry::{FileEntry, Language};
use crate::symbols::queries;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSiteCacheIdentity {
    pub rel_path: String,
    pub size: u64,
    pub modified: DateTime<Utc>,
    pub language: Language,
}

impl CallSiteCacheIdentity {
    pub fn from_entry(entry: &FileEntry) -> Self {
        Self {
            rel_path: entry.rel_path.clone(),
            size: entry.size,
            modified: entry.modified,
            language: entry.language,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSiteFact {
    pub callee: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedCallSites {
    pub identity: CallSiteCacheIdentity,
    pub facts: Vec<CallSiteFact>,
}

impl CachedCallSites {
    pub fn new(entry: &FileEntry, facts: Vec<CallSiteFact>) -> Self {
        Self {
            identity: CallSiteCacheIdentity::from_entry(entry),
            facts,
        }
    }

    #[allow(dead_code)]
    pub fn freshness_against(
        &self,
        current: &FileEntry,
        max_file_size: u64,
    ) -> CallSiteCacheFreshness {
        if current.size > max_file_size {
            return CallSiteCacheFreshness::Miss(CallSiteCacheMissReason::Oversized);
        }

        if !current.language.has_tree_sitter_support() {
            return CallSiteCacheFreshness::Miss(CallSiteCacheMissReason::Unsupported);
        }

        if self.identity == CallSiteCacheIdentity::from_entry(current) {
            CallSiteCacheFreshness::Fresh
        } else {
            CallSiteCacheFreshness::Miss(CallSiteCacheMissReason::Stale)
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSiteCacheFreshness {
    Fresh,
    Miss(CallSiteCacheMissReason),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSiteCacheMissReason {
    Unsupported,
    Missing,
    Absent,
    Stale,
    Oversized,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallSiteCacheLookup {
    Hit(Vec<CallSiteFact>),
    Miss(CallSiteCacheMissReason),
}

pub fn extract_call_site_facts(source: &str, language: Language) -> Vec<CallSiteFact> {
    let Some(config) = queries::get_language_config(language) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&config.language).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let Ok(query) = tree_sitter::Query::new(&config.language, config.callers_query) else {
        return Vec::new();
    };

    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let Some(callee_idx) = capture_names.iter().position(|n| n == "callee") else {
        return Vec::new();
    };

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut facts = Vec::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index as usize != callee_idx {
                continue;
            }

            let callee = cap
                .node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .to_string();
            let line = cap.node.start_position().row + 1;
            let text = source
                .lines()
                .nth(line - 1)
                .map(|l| l.trim().to_string())
                .unwrap_or_default();

            facts.push(CallSiteFact { callee, line, text });
        }
    }

    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn entry(rel_path: &str, size: u64, timestamp: i64) -> FileEntry {
        FileEntry::new(
            rel_path.to_string(),
            size,
            Utc.timestamp_opt(timestamp, 0).unwrap(),
        )
    }

    #[test]
    fn identity_is_fresh_for_matching_file_metadata() {
        let current = entry("src/lib.rs", 10, 1);
        let cached = CachedCallSites::new(&current, Vec::new());

        assert_eq!(
            cached.freshness_against(&current, 100),
            CallSiteCacheFreshness::Fresh
        );
    }

    #[test]
    fn metadata_differences_make_cache_stale() {
        let original = entry("src/lib.rs", 10, 1);
        let cached = CachedCallSites::new(&original, Vec::new());

        let timestamp_changed = entry("src/lib.rs", 10, 2);
        let size_changed = entry("src/lib.rs", 11, 1);
        let path_changed = entry("other/lib.rs", 10, 1);
        let mut language_changed = entry("src/lib.rs", 10, 1);
        language_changed.language = Language::Python;

        for current in [
            timestamp_changed,
            size_changed,
            path_changed,
            language_changed,
        ] {
            assert_eq!(
                cached.freshness_against(&current, 100),
                CallSiteCacheFreshness::Miss(CallSiteCacheMissReason::Stale)
            );
        }
    }

    #[test]
    fn unsupported_and_oversized_files_are_not_fresh() {
        let markdown = entry("README.md", 10, 1);
        let cached = CachedCallSites::new(&markdown, Vec::new());
        assert_eq!(
            cached.freshness_against(&markdown, 100),
            CallSiteCacheFreshness::Miss(CallSiteCacheMissReason::Unsupported)
        );

        let oversized = entry("src/lib.rs", 101, 1);
        let cached = CachedCallSites::new(&oversized, Vec::new());
        assert_eq!(
            cached.freshness_against(&oversized, 100),
            CallSiteCacheFreshness::Miss(CallSiteCacheMissReason::Oversized)
        );
    }
}
