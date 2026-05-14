use dashmap::DashMap;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

use super::call_site_cache::{
    CachedCallSites, CallSiteCacheFreshness, CallSiteCacheLookup, CallSiteCacheMissReason,
    CallSiteFact,
};
use super::file_entry::{FileEntry, Language};

/// Thread-safe file tree backed by a DashMap for concurrent access.
pub struct FileTree {
    pub files: DashMap<String, FileEntry>,
    non_code_ranges: DashMap<String, CachedNonCodeRanges>,
    call_site_cache: DashMap<String, CachedCallSites>,
}

#[derive(Clone)]
pub struct CachedNonCodeRanges {
    pub size: u64,
    pub modified: chrono::DateTime<chrono::Utc>,
    pub language: Language,
    pub ranges: Vec<(usize, usize)>,
}

#[derive(Debug, Serialize)]
pub struct LanguageBreakdown {
    pub language: Language,
    pub count: usize,
}

impl FileTree {
    pub fn new() -> Self {
        Self {
            files: DashMap::new(),
            non_code_ranges: DashMap::new(),
            call_site_cache: DashMap::new(),
        }
    }

    pub fn insert(&self, entry: FileEntry) {
        self.non_code_ranges.remove(&entry.rel_path);
        self.purge_call_sites(&entry.rel_path);
        self.files.insert(entry.rel_path.clone(), entry);
    }

    pub fn remove(&self, rel_path: &str) -> Option<FileEntry> {
        self.non_code_ranges.remove(rel_path);
        self.purge_call_sites(rel_path);
        self.files.remove(rel_path).map(|(_, v)| v)
    }

    pub fn get(&self, rel_path: &str) -> Option<FileEntry> {
        self.files.get(rel_path).map(|r| r.value().clone())
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn language_breakdown(&self) -> Vec<LanguageBreakdown> {
        let mut counts: HashMap<Language, usize> = HashMap::new();
        for entry in self.files.iter() {
            *counts.entry(entry.value().language).or_insert(0) += 1;
        }
        let mut breakdown: Vec<_> = counts
            .into_iter()
            .map(|(language, count)| LanguageBreakdown { language, count })
            .collect();
        breakdown.sort_by(|a, b| b.count.cmp(&a.count));
        breakdown
    }

    pub fn all_paths(&self) -> Vec<String> {
        self.files.iter().map(|r| r.key().clone()).collect()
    }

    pub fn cached_non_code_ranges(
        &self,
        rel_path: &str,
        entry: &FileEntry,
    ) -> Option<Vec<(usize, usize)>> {
        self.non_code_ranges.get(rel_path).and_then(|cached| {
            let cached = cached.value();
            if cached.size == entry.size
                && cached.modified == entry.modified
                && cached.language == entry.language
            {
                Some(cached.ranges.clone())
            } else {
                None
            }
        })
    }

    pub fn store_non_code_ranges(
        &self,
        rel_path: &str,
        entry: &FileEntry,
        ranges: Vec<(usize, usize)>,
    ) {
        self.non_code_ranges.insert(
            rel_path.to_string(),
            CachedNonCodeRanges {
                size: entry.size,
                modified: entry.modified,
                language: entry.language,
                ranges,
            },
        );
    }

    pub fn store_call_sites(
        &self,
        entry: &FileEntry,
        facts: Vec<CallSiteFact>,
    ) -> Result<(), CallSiteCacheMissReason> {
        if !entry.language.has_tree_sitter_support() {
            return Err(CallSiteCacheMissReason::Unsupported);
        }

        self.call_site_cache
            .insert(entry.rel_path.clone(), CachedCallSites::new(entry, facts));
        Ok(())
    }

    #[allow(dead_code)]
    pub fn call_site_cache_lookup(
        &self,
        rel_path: &str,
        max_file_size: u64,
    ) -> CallSiteCacheLookup {
        let Some(entry) = self.get(rel_path) else {
            return CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Missing);
        };

        if entry.size > max_file_size {
            return CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Oversized);
        }

        if !entry.language.has_tree_sitter_support() {
            return CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Unsupported);
        }

        let Some(cached) = self.call_site_cache.get(rel_path) else {
            return CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Absent);
        };

        match cached.freshness_against(&entry, max_file_size) {
            CallSiteCacheFreshness::Fresh => CallSiteCacheLookup::Hit(cached.facts.clone()),
            CallSiteCacheFreshness::Miss(reason) => CallSiteCacheLookup::Miss(reason),
        }
    }

    pub fn purge_call_sites(&self, rel_path: &str) {
        self.call_site_cache.remove(rel_path);
    }

    /// Render a tree-like structure string, similar to the `tree` command.
    /// `depth` limits how many directory levels deep to show (0 = unlimited).
    pub fn render_tree(&self, depth: usize) -> String {
        // Collect all paths into a sorted tree structure
        let mut paths: Vec<String> = self.all_paths();
        paths.sort();

        // Build a tree from paths
        let mut root: BTreeMap<String, TreeNode> = BTreeMap::new();
        for path in &paths {
            let parts: Vec<&str> = path.split('/').collect();
            insert_into_tree(&mut root, &parts, 0);
        }

        let mut output = String::new();
        render_tree_node(&root, &mut output, "", depth, 0);
        output
    }
}

enum TreeNode {
    File,
    Dir(BTreeMap<String, TreeNode>),
}

fn insert_into_tree(tree: &mut BTreeMap<String, TreeNode>, parts: &[&str], idx: usize) {
    if idx >= parts.len() {
        return;
    }
    let name = parts[idx].to_string();
    if idx == parts.len() - 1 {
        // Leaf file
        tree.entry(name).or_insert(TreeNode::File);
    } else {
        // Directory
        let node = tree
            .entry(name)
            .or_insert_with(|| TreeNode::Dir(BTreeMap::new()));
        if let TreeNode::Dir(children) = node {
            insert_into_tree(children, parts, idx + 1);
        }
    }
}

fn render_tree_node(
    tree: &BTreeMap<String, TreeNode>,
    output: &mut String,
    prefix: &str,
    max_depth: usize,
    current_depth: usize,
) {
    if max_depth > 0 && current_depth >= max_depth {
        return;
    }

    let entries: Vec<_> = tree.iter().collect();
    for (i, (name, node)) in entries.iter().enumerate() {
        let is_last = i == entries.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };

        match node {
            TreeNode::File => {
                output.push_str(&format!("{}{}{}\n", prefix, connector, name));
            }
            TreeNode::Dir(children) => {
                output.push_str(&format!("{}{}{}/\n", prefix, connector, name));
                render_tree_node(
                    children,
                    output,
                    &format!("{}{}", prefix, child_prefix),
                    max_depth,
                    current_depth + 1,
                );
            }
        }
    }
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

    fn fact(callee: &str) -> CallSiteFact {
        CallSiteFact {
            callee: callee.to_string(),
            start_byte: 0,
            end_byte: callee.len(),
            line: 1,
            text: format!("{callee}();"),
            receiver: None,
        }
    }

    #[test]
    fn non_code_range_cache_is_tied_to_file_metadata() {
        let tree = FileTree::new();
        let original = entry("src/lib.rs", 10, 1);
        tree.insert(original.clone());
        tree.store_non_code_ranges("src/lib.rs", &original, vec![(1, 4)]);

        assert_eq!(
            tree.cached_non_code_ranges("src/lib.rs", &original),
            Some(vec![(1, 4)])
        );

        let changed = entry("src/lib.rs", 11, 2);
        assert_eq!(tree.cached_non_code_ranges("src/lib.rs", &changed), None);
    }

    #[test]
    fn inserting_or_removing_file_invalidates_non_code_range_cache() {
        let tree = FileTree::new();
        let original = entry("src/lib.rs", 10, 1);
        tree.insert(original.clone());
        tree.store_non_code_ranges("src/lib.rs", &original, vec![(1, 4)]);

        let changed = entry("src/lib.rs", 12, 3);
        tree.insert(changed.clone());
        assert_eq!(tree.cached_non_code_ranges("src/lib.rs", &changed), None);

        tree.store_non_code_ranges("src/lib.rs", &changed, vec![(2, 5)]);
        tree.remove("src/lib.rs");
        assert_eq!(tree.cached_non_code_ranges("src/lib.rs", &changed), None);
    }

    #[test]
    fn call_site_cache_uses_exact_project_relative_paths() {
        let tree = FileTree::new();
        let first = entry("crates/a/lib.rs", 10, 1);
        let second = entry("crates/b/lib.rs", 10, 1);
        tree.insert(first.clone());
        tree.insert(second.clone());

        tree.store_call_sites(&first, vec![fact("from_a")]).unwrap();
        tree.store_call_sites(&second, vec![fact("from_b")])
            .unwrap();

        assert_eq!(
            tree.call_site_cache_lookup("crates/a/lib.rs", 100),
            CallSiteCacheLookup::Hit(vec![fact("from_a")])
        );
        assert_eq!(
            tree.call_site_cache_lookup("crates/b/lib.rs", 100),
            CallSiteCacheLookup::Hit(vec![fact("from_b")])
        );
    }

    #[test]
    fn call_site_cache_reports_missing_absent_unsupported_stale_and_oversized() {
        let tree = FileTree::new();

        assert_eq!(
            tree.call_site_cache_lookup("src/missing.rs", 100),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Missing)
        );

        let absent = entry("src/absent.rs", 10, 1);
        tree.insert(absent);
        assert_eq!(
            tree.call_site_cache_lookup("src/absent.rs", 100),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Absent)
        );

        let unsupported = entry("docs/readme.md", 10, 1);
        tree.insert(unsupported.clone());
        assert_eq!(
            tree.store_call_sites(&unsupported, Vec::new()),
            Err(CallSiteCacheMissReason::Unsupported)
        );
        assert_eq!(
            tree.call_site_cache_lookup("docs/readme.md", 100),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Unsupported)
        );

        let original = entry("src/stale.rs", 10, 1);
        tree.insert(original.clone());
        tree.store_call_sites(&original, vec![fact("cached")])
            .unwrap();
        let changed = entry("src/stale.rs", 11, 1);
        tree.insert(changed);
        assert_eq!(
            tree.call_site_cache_lookup("src/stale.rs", 100),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Absent)
        );

        let oversized = entry("src/oversized.rs", 101, 1);
        tree.insert(oversized);
        assert_eq!(
            tree.call_site_cache_lookup("src/oversized.rs", 100),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Oversized)
        );
    }

    #[test]
    fn insert_invalidates_call_site_cache_for_metadata_and_language_changes() {
        let tree = FileTree::new();
        let original = entry("src/lib.rs", 10, 1);
        tree.insert(original.clone());
        tree.store_call_sites(&original, vec![fact("stale")])
            .unwrap();

        let metadata_changed = entry("src/lib.rs", 10, 2);
        tree.insert(metadata_changed);
        assert_eq!(
            tree.call_site_cache_lookup("src/lib.rs", 100),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Absent)
        );

        let python = entry("src/lib.py", 10, 1);
        tree.insert(python.clone());
        tree.store_call_sites(&python, vec![fact("python_stale")])
            .unwrap();
        let mut unsupported = entry("src/lib.py", 10, 2);
        unsupported.language = Language::Markdown;
        tree.insert(unsupported);
        assert_eq!(
            tree.call_site_cache_lookup("src/lib.py", 100),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Unsupported)
        );
    }
}
