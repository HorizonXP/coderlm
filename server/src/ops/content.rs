use std::path::Path;
use std::sync::Arc;

use regex::Regex;
use serde::Serialize;

use crate::index::file_entry::{FileEntry, Language};
use crate::index::file_tree::FileTree;
use crate::symbols::queries;

#[derive(Debug, Serialize)]
pub struct PeekResponse {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub content: String,
}

pub fn peek(
    root: &Path,
    file_tree: &Arc<FileTree>,
    file: &str,
    start: usize,
    end: usize,
) -> Result<PeekResponse, String> {
    if file_tree.get(file).is_none() {
        return Err(format!("File '{}' not found in index", file));
    }

    let abs_path = root.join(file);
    let source = std::fs::read_to_string(&abs_path)
        .map_err(|e| format!("Failed to read '{}': {}", file, e))?;

    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len();
    let start = start.min(total_lines);
    let end = end.min(total_lines);

    let content: String = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6} │ {}", start + i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(PeekResponse {
        file: file.to_string(),
        start_line: start + 1,
        end_line: end,
        total_lines,
        content,
    })
}

#[derive(Debug, Serialize)]
pub struct GrepResponse {
    pub pattern: String,
    pub matches: Vec<GrepMatch>,
    pub total_matches: usize,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct GrepMatch {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Scope filter for grep: restrict matches to code only (skip comments/strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepScope {
    /// Match anywhere (default behavior).
    All,
    /// Only match in code — skip matches inside comment and string AST nodes.
    Code,
}

/// File filter matching mode for grep `file=` requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMatchMode {
    /// Match only the exact project-relative file path.
    Exact,
    /// Match a project-relative file path suffix.
    Suffix,
    /// Match any project-relative file path containing the filter.
    Contains,
}

impl FileMatchMode {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "exact" => Some(FileMatchMode::Exact),
            "suffix" => Some(FileMatchMode::Suffix),
            "contains" => Some(FileMatchMode::Contains),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            FileMatchMode::Exact => "exact",
            FileMatchMode::Suffix => "suffix",
            FileMatchMode::Contains => "contains",
        }
    }

    fn matches(self, path: &str, filter: &str) -> bool {
        match self {
            FileMatchMode::Exact => path == filter,
            FileMatchMode::Suffix => path.ends_with(filter),
            FileMatchMode::Contains => path.contains(filter),
        }
    }
}

impl GrepScope {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "all" => Some(GrepScope::All),
            "code" => Some(GrepScope::Code),
            _ => None,
        }
    }
}

/// Grep with default scope (matches anywhere). Convenience wrapper.
#[allow(dead_code)]
pub fn grep(
    root: &Path,
    file_tree: &Arc<FileTree>,
    pattern: &str,
    max_matches: usize,
    context_lines: usize,
) -> Result<GrepResponse, String> {
    grep_with_scope(
        root,
        file_tree,
        pattern,
        max_matches,
        context_lines,
        GrepScope::All,
        None,
        None,
    )
}

pub fn grep_with_scope(
    root: &Path,
    file_tree: &Arc<FileTree>,
    pattern: &str,
    max_matches: usize,
    context_lines: usize,
    scope: GrepScope,
    file_filter: Option<&str>,
    file_match: Option<FileMatchMode>,
) -> Result<GrepResponse, String> {
    let re = Regex::new(pattern).map_err(|e| format!("Invalid regex: {}", e))?;

    let mut matches = Vec::new();
    let mut total = 0;

    let mut paths: Vec<(String, FileEntry)> = file_tree
        .files
        .iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    paths = filter_grep_paths(paths, file_filter, file_match)?;

    for (rel_path, entry) in &paths {
        let abs_path = root.join(rel_path);
        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // For scope=code, build a set of byte ranges that are inside comments/strings
        let excluded_ranges =
            if scope == GrepScope::Code && entry.language.has_tree_sitter_support() {
                if let Some(ranges) = file_tree.cached_non_code_ranges(rel_path, entry) {
                    ranges
                } else {
                    let ranges = compute_non_code_ranges(&source, entry.language);
                    file_tree.store_non_code_ranges(rel_path, entry, ranges.clone());
                    ranges
                }
            } else {
                Vec::new()
            };

        let lines: Vec<&str> = source.lines().collect();

        // Pre-compute line byte offsets for scope filtering
        let line_offsets: Vec<usize> = if scope == GrepScope::Code {
            let mut offsets = Vec::with_capacity(lines.len());
            let mut offset = 0;
            for line in &lines {
                offsets.push(offset);
                offset += line.len() + 1; // +1 for newline
            }
            offsets
        } else {
            Vec::new()
        };

        for (i, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                // If scope=code, check that the match byte offset is not inside an excluded range
                if scope == GrepScope::Code && !excluded_ranges.is_empty() {
                    let line_start = line_offsets[i];
                    // Find where in the line the regex matched
                    if let Some(m) = re.find(line) {
                        let match_byte = line_start + m.start();
                        if is_in_excluded_range(match_byte, &excluded_ranges) {
                            continue;
                        }
                    }
                }

                total += 1;
                if matches.len() < max_matches {
                    let ctx_start = i.saturating_sub(context_lines);
                    let ctx_end = (i + context_lines + 1).min(lines.len());

                    let context_before: Vec<String> =
                        lines[ctx_start..i].iter().map(|l| l.to_string()).collect();
                    let context_after: Vec<String> = lines[(i + 1)..ctx_end]
                        .iter()
                        .map(|l| l.to_string())
                        .collect();

                    matches.push(GrepMatch {
                        file: rel_path.clone(),
                        line: i + 1,
                        text: line.to_string(),
                        context_before,
                        context_after,
                    });
                }
            }
        }
    }

    Ok(GrepResponse {
        pattern: pattern.to_string(),
        matches,
        total_matches: total,
        truncated: total > max_matches,
    })
}

fn filter_grep_paths(
    mut paths: Vec<(String, FileEntry)>,
    file_filter: Option<&str>,
    file_match: Option<FileMatchMode>,
) -> Result<Vec<(String, FileEntry)>, String> {
    let Some(filter) = file_filter else {
        if file_match.is_some() {
            return Err("file_match requires the file parameter".to_string());
        }
        return Ok(paths);
    };

    if let Some(mode) = file_match {
        paths.retain(|(path, _)| mode.matches(path, filter));

        if paths.is_empty() {
            return Err(format!(
                "No file matched '{}' with file_match={}",
                filter,
                mode.as_str()
            ));
        }

        if paths.len() > 1 {
            let matched_paths: Vec<&str> = paths.iter().map(|(path, _)| path.as_str()).collect();
            return Err(format!(
                "Ambiguous file filter '{}' with file_match={} matched {} files: {}",
                filter,
                mode.as_str(),
                matched_paths.len(),
                matched_paths.join(", ")
            ));
        }

        return Ok(paths);
    }

    paths.retain(|(path, _)| path == filter || path.contains(filter) || path.ends_with(filter));
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn add_file(root: &Path, file_tree: &Arc<FileTree>, rel_path: &str, source: &str) {
        let abs_path = root.join(rel_path);
        std::fs::create_dir_all(abs_path.parent().unwrap()).unwrap();
        std::fs::write(&abs_path, source).unwrap();
        let metadata = std::fs::metadata(&abs_path).unwrap();
        let modified = metadata.modified().unwrap().into();
        file_tree.insert(FileEntry::new(
            rel_path.to_string(),
            metadata.len(),
            modified,
        ));
    }

    fn fixture() -> (TempDir, Arc<FileTree>) {
        let temp = TempDir::new().unwrap();
        let file_tree = Arc::new(FileTree::new());
        add_file(temp.path(), &file_tree, "src/lib.rs", "needle_exact\n");
        add_file(
            temp.path(),
            &file_tree,
            "tests/src/lib.rs",
            "needle_tests\n",
        );
        add_file(temp.path(), &file_tree, "lib.rs", "needle_root\n");
        add_file(temp.path(), &file_tree, "src/main.rs", "needle_main\n");
        add_file(temp.path(), &file_tree, "docs/feature.md", "needle_docs\n");
        (temp, file_tree)
    }

    #[test]
    fn exact_file_match_only_searches_the_project_relative_path() {
        let (temp, file_tree) = fixture();

        let response = grep_with_scope(
            temp.path(),
            &file_tree,
            "needle_",
            10,
            0,
            GrepScope::All,
            Some("src/lib.rs"),
            Some(FileMatchMode::Exact),
        )
        .unwrap();

        assert_eq!(response.total_matches, 1);
        assert_eq!(response.matches[0].file, "src/lib.rs");
    }

    #[test]
    fn suffix_file_match_searches_the_unique_matching_suffix() {
        let (temp, file_tree) = fixture();

        let response = grep_with_scope(
            temp.path(),
            &file_tree,
            "needle_",
            10,
            0,
            GrepScope::All,
            Some("main.rs"),
            Some(FileMatchMode::Suffix),
        )
        .unwrap();

        assert_eq!(response.total_matches, 1);
        assert_eq!(response.matches[0].file, "src/main.rs");
    }

    #[test]
    fn contains_file_match_searches_the_unique_containing_path() {
        let (temp, file_tree) = fixture();

        let response = grep_with_scope(
            temp.path(),
            &file_tree,
            "needle_",
            10,
            0,
            GrepScope::All,
            Some("feature"),
            Some(FileMatchMode::Contains),
        )
        .unwrap();

        assert_eq!(response.total_matches, 1);
        assert_eq!(response.matches[0].file, "docs/feature.md");
    }

    #[test]
    fn explicit_suffix_file_match_reports_ambiguous_paths() {
        let (temp, file_tree) = fixture();

        let err = grep_with_scope(
            temp.path(),
            &file_tree,
            "needle_",
            10,
            0,
            GrepScope::All,
            Some("src/lib.rs"),
            Some(FileMatchMode::Suffix),
        )
        .unwrap_err();

        assert!(err.contains("Ambiguous file filter"));
        assert!(err.contains("src/lib.rs"));
        assert!(err.contains("tests/src/lib.rs"));
    }

    #[test]
    fn explicit_contains_file_match_reports_ambiguous_paths() {
        let (temp, file_tree) = fixture();

        let err = grep_with_scope(
            temp.path(),
            &file_tree,
            "needle_",
            10,
            0,
            GrepScope::All,
            Some("lib.rs"),
            Some(FileMatchMode::Contains),
        )
        .unwrap_err();

        assert!(err.contains("Ambiguous file filter"));
        assert!(err.contains("lib.rs"));
    }

    #[test]
    fn explicit_file_match_reports_no_matching_path() {
        let (temp, file_tree) = fixture();

        let err = grep_with_scope(
            temp.path(),
            &file_tree,
            "needle_",
            10,
            0,
            GrepScope::All,
            Some("missing.rs"),
            Some(FileMatchMode::Exact),
        )
        .unwrap_err();

        assert!(err.contains("No file matched"));
    }

    #[test]
    fn default_file_filter_preserves_broad_compatibility_matching() {
        let (temp, file_tree) = fixture();

        let response = grep_with_scope(
            temp.path(),
            &file_tree,
            "needle_",
            10,
            0,
            GrepScope::All,
            Some("lib.rs"),
            None,
        )
        .unwrap();

        let files: Vec<&str> = response
            .matches
            .iter()
            .map(|grep_match| grep_match.file.as_str())
            .collect();
        assert_eq!(files, vec!["lib.rs", "src/lib.rs", "tests/src/lib.rs"]);
    }

    #[test]
    fn file_match_rejects_unsupported_values() {
        assert_eq!(FileMatchMode::from_str("glob"), None);
    }
}

/// Compute byte ranges of comment and string nodes using tree-sitter.
fn compute_non_code_ranges(source: &str, language: Language) -> Vec<(usize, usize)> {
    use tree_sitter::StreamingIterator;

    let config = match queries::get_language_config(language) {
        Some(c) => c,
        None => return Vec::new(),
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&config.language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    // Query for comment and string nodes
    let query_str = match language {
        Language::Rust => {
            r#"
            (line_comment) @skip
            (block_comment) @skip
            (string_literal) @skip
            (raw_string_literal) @skip
        "#
        }
        Language::Python => {
            r#"
            (comment) @skip
            (string) @skip
        "#
        }
        Language::TypeScript | Language::JavaScript => {
            r#"
            (comment) @skip
            (string) @skip
            (template_string) @skip
        "#
        }
        Language::Go => {
            r#"
            (comment) @skip
            (raw_string_literal) @skip
            (interpreted_string_literal) @skip
        "#
        }
        Language::Java => {
            r#"
            (line_comment) @skip
            (block_comment) @skip
            (string_literal) @skip
        "#
        }
        Language::Scala => {
            r#"
            (comment) @skip
            (block_comment) @skip
            (string) @skip
            (interpolated_string_expression) @skip
        "#
        }
        Language::Ruby => {
            r#"
            (comment) @skip
            (string) @skip
            (heredoc_body) @skip
            (regex) @skip
        "#
        }
        Language::Php => {
            r#"
            (comment) @skip
            (string) @skip
            (encapsed_string) @skip
            (heredoc) @skip
            (nowdoc) @skip
        "#
        }
        Language::Zig => {
            r#"
            (comment) @skip
            (string) @skip
            (multiline_string) @skip
        "#
        }
        Language::Elixir => {
            r#"
            (comment) @skip
            (string) @skip
            (charlist) @skip
            (sigil) @skip
            (quoted_atom) @skip
            (quoted_keyword) @skip
        "#
        }
        _ => return Vec::new(),
    };

    let query = match tree_sitter::Query::new(&config.language, query_str) {
        Ok(q) => q,
        Err(_) => return Vec::new(),
    };

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut ranges = Vec::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            ranges.push((cap.node.start_byte(), cap.node.end_byte()));
        }
    }

    // Sort and merge overlapping ranges
    ranges.sort_by_key(|r| r.0);
    ranges
}

fn is_in_excluded_range(byte_offset: usize, ranges: &[(usize, usize)]) -> bool {
    // Binary search for efficiency
    ranges
        .binary_search_by(|&(start, end)| {
            if byte_offset < start {
                std::cmp::Ordering::Greater
            } else if byte_offset >= end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

#[derive(Debug, Serialize)]
pub struct ChunkIndicesResponse {
    pub file: String,
    pub total_bytes: usize,
    pub chunk_size: usize,
    pub overlap: usize,
    pub chunks: Vec<ChunkInfo>,
}

#[derive(Debug, Serialize)]
pub struct ChunkInfo {
    pub index: usize,
    pub start: usize,
    pub end: usize,
}

pub fn chunk_indices(
    root: &Path,
    file_tree: &Arc<FileTree>,
    file: &str,
    size: usize,
    overlap: usize,
) -> Result<ChunkIndicesResponse, String> {
    if size == 0 {
        return Err("Chunk size must be > 0".to_string());
    }
    if overlap >= size {
        return Err("Overlap must be < chunk size".to_string());
    }
    if file_tree.get(file).is_none() {
        return Err(format!("File '{}' not found in index", file));
    }

    let abs_path = root.join(file);
    let source = std::fs::read_to_string(&abs_path)
        .map_err(|e| format!("Failed to read '{}': {}", file, e))?;

    let total_bytes = source.len();
    let step = size - overlap;
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut index = 0;

    while start < total_bytes {
        let end = (start + size).min(total_bytes);
        chunks.push(ChunkInfo { index, start, end });
        index += 1;
        start += step;
        if end >= total_bytes {
            break;
        }
    }

    Ok(ChunkIndicesResponse {
        file: file.to_string(),
        total_bytes,
        chunk_size: size,
        overlap,
        chunks,
    })
}
