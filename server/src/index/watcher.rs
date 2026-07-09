use anyhow::Result;
use chrono::{DateTime, Utc};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{Match, WalkBuilder};
use notify::event::{AccessKind, AccessMode, MetadataKind, ModifyKind};
use notify::{Event, EventKind, RecommendedWatcher, Watcher};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::config;
use crate::index::call_site_cache::extract_call_site_facts;
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
    last_indexed_at: Arc<Mutex<DateTime<Utc>>>,
) -> Result<WatcherHandle> {
    let root_buf = root.to_path_buf();
    let root_for_handler = root_buf.clone();
    let path_filter = Arc::new(WatchPathFilter::new(&root_buf));
    let (event_tx, event_rx) = mpsc::channel();
    let debounce_filter = path_filter.clone();
    let debounce_thread = start_debounce_loop(
        event_rx,
        root_for_handler,
        file_tree,
        symbol_table,
        max_file_size,
        last_indexed_at,
        debounce_filter,
    )?;

    let raw_filter = path_filter.clone();
    let raw_event_tx = event_tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(mut event) => {
                if !should_process_event_kind(&event.kind) {
                    return;
                }
                event.paths.retain(|path| raw_filter.should_process(path));
                if !event.paths.is_empty() {
                    let _ = raw_event_tx.send(WatcherMessage::Event(Ok(event)));
                }
            }
            Err(err) => {
                let _ = raw_event_tx.send(WatcherMessage::Event(Err(err)));
            }
        },
        notify::Config::default(),
    )?;

    let watched_dirs = watch_existing_source_dirs(&mut watcher, &root_buf)?;

    info!(
        "Filesystem watcher started for {} ({} directories watched)",
        root_buf.display(),
        watched_dirs
    );

    Ok(WatcherHandle {
        _watcher: Some(watcher),
        stop_tx: Some(event_tx),
        debounce_thread: Some(debounce_thread),
    })
}

pub struct WatcherHandle {
    _watcher: Option<RecommendedWatcher>,
    stop_tx: Option<Sender<WatcherMessage>>,
    debounce_thread: Option<JoinHandle<()>>,
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(WatcherMessage::Shutdown);
        }
        if let Some(thread) = self.debounce_thread.take() {
            let _ = thread.join();
        }
    }
}

enum WatcherMessage {
    Event(Result<Event, notify::Error>),
    Shutdown,
}

fn start_debounce_loop(
    event_rx: Receiver<WatcherMessage>,
    root: PathBuf,
    file_tree: Arc<FileTree>,
    symbol_table: Arc<SymbolTable>,
    max_file_size: u64,
    last_indexed_at: Arc<Mutex<DateTime<Utc>>>,
    path_filter: Arc<WatchPathFilter>,
) -> Result<JoinHandle<()>> {
    let thread = std::thread::Builder::new()
        .name("coderlm watcher debounce".to_string())
        .spawn(move || {
            let mut pending = BTreeMap::new();
            let debounce_timeout = Duration::from_millis(500);

            loop {
                match receive_watcher_message(&event_rx, pending.is_empty(), debounce_timeout) {
                    WatcherLoopAction::AddEvent(Ok(event)) => {
                        for path in event.paths {
                            if path_filter.should_process(&path) {
                                pending.insert(path, ());
                            }
                        }
                    }
                    WatcherLoopAction::AddEvent(Err(err)) => {
                        warn!("Filesystem watcher error: {}", err);
                    }
                    WatcherLoopAction::Flush => {
                        if pending.is_empty() {
                            continue;
                        }
                        let events = pending
                            .keys()
                            .cloned()
                            .map(|path| WatchEvent::new(path, WatchEventKind::Any))
                            .collect();
                        pending.clear();
                        let stats = handle_events(
                            &root,
                            &file_tree,
                            &symbol_table,
                            max_file_size,
                            events,
                            &path_filter,
                        );
                        if stats.index_changed() {
                            *last_indexed_at.lock() = Utc::now();
                        }
                    }
                    WatcherLoopAction::Shutdown => break,
                }
            }
        })?;

    Ok(thread)
}

enum WatcherLoopAction {
    AddEvent(Result<Event, notify::Error>),
    Flush,
    Shutdown,
}

fn receive_watcher_message(
    event_rx: &Receiver<WatcherMessage>,
    pending_is_empty: bool,
    timeout: Duration,
) -> WatcherLoopAction {
    if pending_is_empty {
        return match event_rx.recv() {
            Ok(WatcherMessage::Event(event)) => WatcherLoopAction::AddEvent(event),
            Ok(WatcherMessage::Shutdown) | Err(_) => WatcherLoopAction::Shutdown,
        };
    }

    match event_rx.recv_timeout(timeout) {
        Ok(WatcherMessage::Event(event)) => WatcherLoopAction::AddEvent(event),
        Ok(WatcherMessage::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
            WatcherLoopAction::Shutdown
        }
        Err(RecvTimeoutError::Timeout) => WatcherLoopAction::Flush,
    }
}

fn should_process_event_kind(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => false,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any => true,
        EventKind::Other => false,
    }
}

fn watch_existing_source_dirs(watcher: &mut RecommendedWatcher, root: &Path) -> Result<usize> {
    let mut watched = 0;

    for dir in collect_watchable_dirs(root) {
        match watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
            Ok(()) => watched += 1,
            Err(err) => warn!("Failed to watch {}: {}", dir.display(), err),
        }
    }

    if watched == 0 {
        watcher.watch(root, notify::RecursiveMode::NonRecursive)?;
        watched = 1;
    }

    Ok(watched)
}

fn collect_watchable_dirs(root: &Path) -> Vec<PathBuf> {
    let root_for_filter = root.to_path_buf();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(move |entry| should_watch_entry(&root_for_filter, entry.path()))
        .build();

    let mut dirs = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            dirs.push(entry.path().to_path_buf());
        }
    }

    dirs.sort();
    dirs
}

struct WatchPathFilter {
    root: PathBuf,
    gitignore: Gitignore,
}

impl WatchPathFilter {
    fn new(root: &Path) -> Self {
        let mut builder = GitignoreBuilder::new(root);
        let _ = builder.add(root.join(".gitignore"));
        let _ = builder.add(root.join(".git/info/exclude"));
        let gitignore = builder.build().unwrap_or_else(|err| {
            warn!("Failed to build watcher gitignore matcher: {}", err);
            GitignoreBuilder::new(root)
                .build()
                .expect("empty gitignore matcher should build")
        });

        Self {
            root: root.to_path_buf(),
            gitignore,
        }
    }

    fn should_process(&self, path: &Path) -> bool {
        let rel_path = match path.strip_prefix(&self.root) {
            Ok(rel_path) => rel_path,
            Err(_) => return false,
        };

        if rel_path.as_os_str().is_empty() {
            return false;
        }

        if path.is_dir() {
            return false;
        }

        let rel_path_string = rel_path.to_string_lossy();
        if should_skip(&rel_path_string) || has_hidden_component(rel_path) {
            return false;
        }

        if config::should_ignore_extension(&rel_path_string) {
            return false;
        }

        !matches!(
            self.gitignore
                .matched_path_or_any_parents(rel_path, path.is_dir()),
            Match::Ignore(_)
        )
    }
}

struct WatchEvent {
    path: PathBuf,
    kind: WatchEventKind,
}

impl WatchEvent {
    fn new(path: PathBuf, kind: WatchEventKind) -> Self {
        Self { path, kind }
    }
}

enum WatchEventKind {
    Any,
}

fn should_watch_entry(root: &Path, path: &Path) -> bool {
    if path == root {
        return true;
    }

    let rel_path = match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        Err(_) => return false,
    };

    !should_skip(&rel_path)
}

fn has_hidden_component(rel_path: &Path) -> bool {
    rel_path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| name.starts_with('.'))
    })
}

fn handle_events(
    root: &Path,
    file_tree: &Arc<FileTree>,
    symbol_table: &Arc<SymbolTable>,
    max_file_size: u64,
    events: Vec<WatchEvent>,
    path_filter: &WatchPathFilter,
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

        // Skip ignored paths. Raw watcher events are already filtered before
        // debouncing, but keep this guard for manually constructed events and
        // racey paths whose ignore state changed after enqueue.
        if !path_filter.should_process(path) {
            stats.skipped_events += 1;
            continue;
        }

        match event.kind {
            WatchEventKind::Any => {
                pending.insert(rel_path, path.clone());
            }
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
    root: &Path,
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
                if let Ok(source) = std::fs::read_to_string(abs_path) {
                    if let Some(entry) = file_tree.get(rel_path) {
                        if let Some(facts) = extract_call_site_facts(&source, language) {
                            let _ = file_tree.store_call_sites(&entry, facts);
                        }
                    }
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

    fn index_changed(&self) -> bool {
        self.changed_files > 0 || self.deleted_files > 0 || self.removed_oversize_files > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::call_site_cache::{CallSiteCacheLookup, CallSiteCacheMissReason};
    use crate::ops::symbol_ops::find_callers;
    use std::fs;
    use tempfile::tempdir;

    fn event(path: &Path) -> WatchEvent {
        WatchEvent::new(path.to_path_buf(), WatchEventKind::Any)
    }

    fn cached_callees(file_tree: &FileTree, rel_path: &str) -> Vec<String> {
        match file_tree.call_site_cache_lookup(rel_path, 1_000_000) {
            CallSiteCacheLookup::Hit(facts) => facts.into_iter().map(|fact| fact.callee).collect(),
            other => panic!("expected cached call sites for {rel_path}, got {other:?}"),
        }
    }

    fn handle_test_events(
        root: &Path,
        file_tree: &Arc<FileTree>,
        symbol_table: &Arc<SymbolTable>,
        max_file_size: u64,
        events: Vec<WatchEvent>,
    ) -> WatcherEventStats {
        let path_filter = WatchPathFilter::new(root);
        handle_events(
            root,
            file_tree,
            symbol_table,
            max_file_size,
            events,
            &path_filter,
        )
    }

    #[test]
    fn watchable_dirs_respect_gitignore_and_hidden_subtrees() {
        let temp = tempdir().unwrap();
        let root = temp.path().to_path_buf();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "generated/\ncache-output/\n").unwrap();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join("generated/dev/lib")).unwrap();
        fs::create_dir_all(root.join("cache-output/tmp")).unwrap();
        fs::create_dir_all(root.join(".hidden-work/cache")).unwrap();

        let dirs = collect_watchable_dirs(&root);
        let rel_dirs: Vec<String> = dirs
            .iter()
            .map(|dir| {
                dir.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();

        assert!(rel_dirs.contains(&"".to_string()));
        assert!(rel_dirs.contains(&"src".to_string()));
        assert!(rel_dirs.contains(&"src/nested".to_string()));
        assert!(!rel_dirs.iter().any(|dir| dir.starts_with("generated")));
        assert!(!rel_dirs.iter().any(|dir| dir.starts_with("cache-output")));
        assert!(!rel_dirs.iter().any(|dir| dir.starts_with(".hidden-work")));
    }

    #[test]
    fn path_filter_skips_gitignored_and_hidden_paths_before_indexing() {
        let temp = tempdir().unwrap();
        let root = temp.path().to_path_buf();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), ".runtime/\ngenerated/\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".runtime")).unwrap();
        fs::create_dir_all(root.join("generated")).unwrap();
        let source_file = root.join("src/main.rs");
        let hidden_file = root.join(".runtime/state.json");
        let generated_file = root.join("generated/out.rs");
        fs::write(&source_file, "pub fn indexed() {}\n").unwrap();
        fs::write(&hidden_file, "{}\n").unwrap();
        fs::write(&generated_file, "pub fn ignored() {}\n").unwrap();

        let file_tree = Arc::new(FileTree::new());
        let symbol_table = Arc::new(SymbolTable::new());
        let path_filter = WatchPathFilter::new(&root);

        assert!(!path_filter.should_process(&root));

        let stats = handle_test_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![
                event(&root),
                event(&source_file),
                event(&hidden_file),
                event(&generated_file),
            ],
        );

        assert_eq!(stats.raw_events, 4);
        assert_eq!(stats.changed_files, 1);
        assert_eq!(stats.skipped_events, 3);
        assert!(file_tree.get("src/main.rs").is_some());
        assert!(file_tree.get(".runtime/state.json").is_none());
        assert!(file_tree.get("generated/out.rs").is_none());
    }

    #[test]
    fn event_kind_filter_ignores_read_access_noise() {
        assert!(!should_process_event_kind(&EventKind::Access(
            AccessKind::Open(AccessMode::Read)
        )));
        assert!(!should_process_event_kind(&EventKind::Access(
            AccessKind::Close(AccessMode::Read)
        )));
        assert!(!should_process_event_kind(&EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::AccessTime)
        )));
        assert!(should_process_event_kind(&EventKind::Access(
            AccessKind::Close(AccessMode::Write)
        )));
        assert!(should_process_event_kind(&EventKind::Modify(
            ModifyKind::Data(notify::event::DataChange::Content)
        )));
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

        let stats = handle_test_events(
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

        let initial = handle_test_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![event(&file)],
        );
        assert_eq!(initial.reparsed_files, 1);
        assert!(symbol_table.get("src/main.rs", "stale").is_some());
        assert_eq!(
            cached_callees(&file_tree, "src/main.rs"),
            Vec::<String>::new()
        );

        fs::write(&file, "pub fn too_large() {}\n".repeat(20)).unwrap();
        let stats = handle_test_events(&root, &file_tree, &symbol_table, 10, vec![event(&file)]);

        assert_eq!(stats.changed_files, 0);
        assert_eq!(stats.reparsed_files, 0);
        assert_eq!(stats.removed_oversize_files, 1);
        assert!(file_tree.get("src/main.rs").is_none());
        assert_eq!(
            file_tree.call_site_cache_lookup("src/main.rs", 1_000_000),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Missing)
        );
        assert_eq!(symbol_table.len(), 0);
    }

    #[test]
    fn changed_file_refreshes_call_site_cache_and_drops_stale_callers() {
        let temp = tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let src = root.join("src");
        fs::create_dir(&src).unwrap();
        let file = src.join("main.rs");
        fs::write(
            &file,
            "\
fn target() {}
fn old_call() { target(); }
",
        )
        .unwrap();

        let file_tree = Arc::new(FileTree::new());
        let symbol_table = Arc::new(SymbolTable::new());

        let initial = handle_test_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![event(&file)],
        );
        assert_eq!(initial.reparsed_files, 1);
        assert_eq!(cached_callees(&file_tree, "src/main.rs"), vec!["target"]);

        fs::write(
            &file,
            "\
fn target() {}
fn marker() { let _text = \"fn old_call() { target(); }\"; }
fn fresh_call() { target(); }
",
        )
        .unwrap();

        let stats = handle_test_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![event(&file)],
        );

        assert_eq!(stats.changed_files, 1);
        assert_eq!(stats.reparsed_files, 1);
        assert_eq!(cached_callees(&file_tree, "src/main.rs"), vec!["target"]);

        let callers = find_callers(
            &root,
            &file_tree,
            &symbol_table,
            "target",
            "src/main.rs",
            10,
        )
        .unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].line, 3);
        assert_eq!(callers[0].text, "fn fresh_call() { target(); }");
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
        handle_test_events(
            &root,
            &file_tree,
            &symbol_table,
            1_000_000,
            vec![event(&rust_file)],
        );
        assert!(symbol_table.get("src/main.rs", "removed").is_some());
        assert_eq!(
            cached_callees(&file_tree, "src/main.rs"),
            Vec::<String>::new()
        );

        fs::remove_file(&rust_file).unwrap();
        fs::write(&text_file, "notes only\n").unwrap();

        let stats = handle_test_events(
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
        assert_eq!(
            file_tree.call_site_cache_lookup("src/main.rs", 1_000_000),
            CallSiteCacheLookup::Miss(CallSiteCacheMissReason::Missing)
        );
        assert!(file_tree.get("src/main.txt").is_some());
        assert_eq!(symbol_table.len(), 0);
    }
}
