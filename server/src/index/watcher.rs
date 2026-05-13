use anyhow::Result;
use chrono::{DateTime, Utc};
use notify_debouncer_mini::{DebouncedEventKind, new_debouncer};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::config;
use crate::index::file_entry::FileEntry;
use crate::index::file_tree::FileTree;
use crate::symbols::SymbolTable;
use crate::symbols::parser::extract_symbols_from_file;

/// Start the filesystem watcher. Returns a handle that keeps the watcher alive.
/// Drop the handle to stop watching.
pub fn start_watcher(
    root: &Path,
    file_tree: Arc<FileTree>,
    symbol_table: Arc<SymbolTable>,
    max_file_size: u64,
) -> Result<WatcherHandle> {
    let root_buf = root.to_path_buf();
    let root_for_handler = root_buf.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(500),
        move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            match result {
                Ok(events) => {
                    handle_events(
                        &root_for_handler,
                        &file_tree,
                        &symbol_table,
                        max_file_size,
                        events,
                    );
                }
                Err(e) => {
                    warn!("Filesystem watcher error: {}", e);
                }
            }
        },
    )?;

    debouncer
        .watcher()
        .watch(&root_buf, notify::RecursiveMode::Recursive)?;

    info!("Filesystem watcher started for {}", root_buf.display());

    Ok(WatcherHandle {
        _debouncer: Some(debouncer),
    })
}

pub struct WatcherHandle {
    _debouncer: Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>,
}

fn handle_events(
    root: &PathBuf,
    file_tree: &Arc<FileTree>,
    symbol_table: &Arc<SymbolTable>,
    max_file_size: u64,
    events: Vec<notify_debouncer_mini::DebouncedEvent>,
) -> WatcherEventStats {
    let mut pending = BTreeMap::new();
    let mut stats = WatcherEventStats {
        raw_events: events.len(),
        ..WatcherEventStats::default()
    };

    for event in events {
        let path = &event.path;

        // Get relative path
        let rel_path = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => {
                stats.skipped_events += 1;
                continue;
            }
        };

        // Skip ignored paths
        if should_skip(&rel_path) {
            stats.skipped_events += 1;
            continue;
        }

        match event.kind {
            DebouncedEventKind::Any => {
                pending.insert(rel_path, path.clone());
            }
            DebouncedEventKind::AnyContinuous => {
                // Ignore continuous events (they'll be followed by a final Any)
                stats.skipped_events += 1;
            }
            _ => {}
        }
    }

    for (rel_path, path) in pending {
        if path.is_file() {
            handle_file_change(
                root,
                file_tree,
                symbol_table,
                max_file_size,
                &rel_path,
                &path,
                &mut stats,
            );
        } else if !path.exists() {
            handle_file_delete(file_tree, symbol_table, &rel_path);
            stats.deleted_files += 1;
        } else {
            stats.skipped_events += 1;
        }
    }

    debug!(
        "Processed {} watcher events as {} unique paths (changed={}, reparsed={}, deleted={}, removed_oversize={}, skipped={})",
        stats.raw_events,
        stats.unique_paths(),
        stats.changed_files,
        stats.reparsed_files,
        stats.deleted_files,
        stats.removed_oversize_files,
        stats.skipped_events
    );

    stats
}

fn handle_file_change(
    root: &PathBuf,
    file_tree: &Arc<FileTree>,
    symbol_table: &Arc<SymbolTable>,
    max_file_size: u64,
    rel_path: &str,
    abs_path: &Path,
    stats: &mut WatcherEventStats,
) {
    // Check extension-based ignoring
    if config::should_ignore_extension(rel_path) {
        handle_file_delete(file_tree, symbol_table, rel_path);
        stats.deleted_files += 1;
        return;
    }

    let metadata = match std::fs::metadata(abs_path) {
        Ok(m) => m,
        Err(_) => {
            stats.skipped_events += 1;
            return;
        }
    };

    let size = metadata.len();
    if size > max_file_size {
        handle_file_delete(file_tree, symbol_table, rel_path);
        stats.removed_oversize_files += 1;
        return;
    }

    let modified: DateTime<Utc> = metadata
        .modified()
        .map(DateTime::from)
        .unwrap_or_else(|_| Utc::now());

    // Update file tree
    let entry = FileEntry::new(rel_path.to_string(), size, modified);
    let language = entry.language;
    file_tree.insert(entry);
    stats.changed_files += 1;

    // Re-extract symbols
    symbol_table.remove_file(rel_path);
    if language.has_tree_sitter_support() {
        match extract_symbols_from_file(root, rel_path, language) {
            Ok(symbols) => {
                let count = symbols.len();
                for sym in symbols {
                    symbol_table.insert(sym);
                }
                if let Some(mut entry) = file_tree.files.get_mut(rel_path) {
                    entry.symbols_extracted = true;
                }
                stats.reparsed_files += 1;
                debug!("Re-extracted {} symbols from {}", count, rel_path);
            }
            Err(e) => {
                debug!("Failed to re-extract symbols from {}: {}", rel_path, e);
            }
        }
    }
}

fn handle_file_delete(file_tree: &Arc<FileTree>, symbol_table: &Arc<SymbolTable>, rel_path: &str) {
    if file_tree.remove(rel_path).is_some() {
        symbol_table.remove_file(rel_path);
        debug!("Removed {} from index", rel_path);
    }
}

fn should_skip(rel_path: &str) -> bool {
    for component in rel_path.split('/') {
        if config::should_ignore_dir(component) {
            return true;
        }
    }
    false
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct WatcherEventStats {
    raw_events: usize,
    changed_files: usize,
    reparsed_files: usize,
    deleted_files: usize,
    removed_oversize_files: usize,
    skipped_events: usize,
}

impl WatcherEventStats {
    fn unique_paths(&self) -> usize {
        self.changed_files + self.deleted_files + self.removed_oversize_files + self.skipped_events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_mini::DebouncedEvent;
    use std::fs;
    use tempfile::tempdir;

    fn event(path: &Path) -> DebouncedEvent {
        DebouncedEvent::new(path.to_path_buf(), DebouncedEventKind::Any)
    }

    #[test]
    fn duplicate_events_reparse_supported_file_once() {
        let temp = tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let src = root.join("src");
        fs::create_dir(&src).unwrap();
        let file = src.join("main.rs");
        fs::write(&file, "pub fn first() -> usize { 1 }\n").unwrap();

        let file_tree = Arc::new(FileTree::new());
        let symbol_table = Arc::new(SymbolTable::new());

        let stats = handle_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![event(&file), event(&file), event(&file)],
        );

        assert_eq!(stats.raw_events, 3);
        assert_eq!(stats.changed_files, 1);
        assert_eq!(stats.reparsed_files, 1);
        assert_eq!(file_tree.len(), 1);
        assert_eq!(symbol_table.len(), 1);
        assert!(symbol_table.get("src/main.rs", "first").is_some());
    }

    #[test]
    fn oversize_edit_removes_file_and_stale_symbols() {
        let temp = tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let src = root.join("src");
        fs::create_dir(&src).unwrap();
        let file = src.join("main.rs");
        fs::write(&file, "pub fn stale() -> usize { 1 }\n").unwrap();

        let file_tree = Arc::new(FileTree::new());
        let symbol_table = Arc::new(SymbolTable::new());

        let initial = handle_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![event(&file)],
        );
        assert_eq!(initial.reparsed_files, 1);
        assert!(symbol_table.get("src/main.rs", "stale").is_some());

        fs::write(&file, "pub fn too_large() {}\n".repeat(20)).unwrap();
        let stats = handle_events(&root, &file_tree, &symbol_table, 10, vec![event(&file)]);

        assert_eq!(stats.changed_files, 0);
        assert_eq!(stats.reparsed_files, 0);
        assert_eq!(stats.removed_oversize_files, 1);
        assert!(file_tree.get("src/main.rs").is_none());
        assert_eq!(symbol_table.len(), 0);
    }

    #[test]
    fn delete_then_recreate_with_unsupported_extension_matches_disk() {
        let temp = tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let src = root.join("src");
        fs::create_dir(&src).unwrap();
        let rust_file = src.join("main.rs");
        let text_file = src.join("main.txt");
        fs::write(&rust_file, "pub fn removed() -> usize { 1 }\n").unwrap();

        let file_tree = Arc::new(FileTree::new());
        let symbol_table = Arc::new(SymbolTable::new());
        handle_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![event(&rust_file)],
        );
        assert!(symbol_table.get("src/main.rs", "removed").is_some());

        fs::remove_file(&rust_file).unwrap();
        fs::write(&text_file, "notes only\n").unwrap();

        let stats = handle_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![event(&rust_file), event(&text_file), event(&text_file)],
        );

        assert_eq!(stats.deleted_files, 1);
        assert_eq!(stats.changed_files, 1);
        assert_eq!(stats.reparsed_files, 0);
        assert!(file_tree.get("src/main.rs").is_none());
        assert!(file_tree.get("src/main.txt").is_some());
        assert_eq!(symbol_table.len(), 0);
    }
}
