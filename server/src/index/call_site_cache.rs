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
    pub start_byte: usize,
    pub end_byte: usize,
    pub line: usize,
    pub text: String,
    pub receiver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_kind: Option<CallSiteKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallSiteKind {
    Bare,
    Method,
    Qualified,
    Macro,
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

pub fn extract_call_site_facts(source: &str, language: Language) -> Option<Vec<CallSiteFact>> {
    let Some(config) = queries::get_language_config(language) else {
        return None;
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&config.language).is_err() {
        return None;
    }

    let Some(tree) = parser.parse(source, None) else {
        return None;
    };

    let Ok(query) = tree_sitter::Query::new(&config.language, config.callers_query) else {
        return None;
    };

    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let Some(callee_idx) = capture_names.iter().position(|n| n == "callee") else {
        return None;
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
            let receiver = call_site_receiver(source, language, cap.node);
            let call_kind = call_site_kind(language, cap.node);
            let line = cap.node.start_position().row + 1;
            let text = source
                .lines()
                .nth(line - 1)
                .map(|l| l.trim().to_string())
                .unwrap_or_default();

            facts.push(CallSiteFact {
                callee,
                start_byte: cap.node.start_byte(),
                end_byte: cap.node.end_byte(),
                line,
                text,
                receiver,
                call_kind,
            });
        }
    }

    Some(facts)
}

pub(crate) fn call_site_receiver(
    source: &str,
    language: Language,
    callee_node: tree_sitter::Node,
) -> Option<String> {
    match language {
        Language::Rust => rust_call_receiver(source, callee_node),
        Language::Elixir => elixir_call_site_receiver(source, callee_node),
        Language::TypeScript | Language::JavaScript => {
            typescript_call_site_receiver(source, callee_node)
        }
        _ => None,
    }
}

fn elixir_call_site_receiver(source: &str, callee_node: tree_sitter::Node) -> Option<String> {
    let dot = callee_node.parent()?;
    if dot.kind() != "dot" {
        return None;
    }

    let receiver = dot.child_by_field_name("left")?;
    receiver
        .utf8_text(source.as_bytes())
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn typescript_call_site_receiver(source: &str, callee_node: tree_sitter::Node) -> Option<String> {
    let member = callee_node.parent()?;
    if member.kind() != "member_expression" {
        return None;
    }

    let property = member.child_by_field_name("property")?;
    if property.start_byte() != callee_node.start_byte()
        || property.end_byte() != callee_node.end_byte()
    {
        return None;
    }

    let receiver = member.child_by_field_name("object")?;
    receiver
        .utf8_text(source.as_bytes())
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn call_site_kind(language: Language, callee_node: tree_sitter::Node) -> Option<CallSiteKind> {
    if language != Language::Rust {
        return None;
    }

    let parent = callee_node.parent()?;
    match parent.kind() {
        "call_expression" => Some(CallSiteKind::Bare),
        "field_expression" => Some(CallSiteKind::Method),
        "scoped_identifier" => Some(CallSiteKind::Qualified),
        "macro_invocation" => Some(CallSiteKind::Macro),
        _ => None,
    }
}

fn rust_call_receiver(source: &str, callee_node: tree_sitter::Node) -> Option<String> {
    let parent = callee_node.parent()?;
    let receiver = match parent.kind() {
        "field_expression" => parent.child_by_field_name("value"),
        "scoped_identifier" => parent.child_by_field_name("path"),
        _ => None,
    }?;

    receiver
        .utf8_text(source.as_bytes())
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
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
