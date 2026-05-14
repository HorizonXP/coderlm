use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, warn};
use tree_sitter::StreamingIterator;

use crate::index::file_entry::Language;
use crate::index::file_tree::FileTree;
use crate::symbols::SymbolTable;
use crate::symbols::queries::{self, QueryKind};
use crate::symbols::symbol::{Symbol, SymbolKind};

/// Extract symbols from a single file.
pub fn extract_symbols_from_file(
    root: &Path,
    rel_path: &str,
    language: Language,
) -> Result<Vec<Symbol>> {
    let config = match queries::get_language_config(language) {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };

    let abs_path = root.join(rel_path);
    let source = std::fs::read_to_string(&abs_path)?;

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language)?;

    let tree = match parser.parse(&source, None) {
        Some(t) => t,
        None => {
            warn!("Failed to parse {}", rel_path);
            return Ok(Vec::new());
        }
    };

    let query = queries::get_cached_query(
        language,
        QueryKind::Symbols,
        &config.language,
        config.symbols_query,
    )?;
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query.query(), tree.root_node(), source.as_bytes());
    let capture_names = query.capture_names();

    let mut symbols = Vec::new();
    let mut current_impl_type: Option<String> = None;

    while let Some(m) = matches.next() {
        let mut name: Option<String> = None;
        let mut kind: Option<SymbolKind> = None;
        let mut def_node: Option<tree_sitter::Node> = None;
        let mut parent: Option<String> = None;

        for cap in m.captures {
            let cap_name = &capture_names[cap.index as usize];
            let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");

            match cap_name.as_str() {
                "function.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Function);
                }
                "function.def" => {
                    def_node = Some(cap.node);
                }
                "method.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Method);
                    parent = current_impl_type.clone();
                }
                "method.def" => {
                    def_node = Some(cap.node);
                }
                "impl.type" => {
                    current_impl_type = Some(text.to_string());
                }
                "struct.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Struct);
                }
                "struct.def" => {
                    def_node = Some(cap.node);
                }
                "enum.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Enum);
                }
                "enum.def" => {
                    def_node = Some(cap.node);
                }
                "trait.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Trait);
                }
                "trait.def" => {
                    def_node = Some(cap.node);
                }
                "class.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Class);
                }
                "class.def" => {
                    def_node = Some(cap.node);
                }
                "interface.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Interface);
                }
                "interface.def" => {
                    def_node = Some(cap.node);
                }
                "type.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Type);
                }
                "type.def" => {
                    def_node = Some(cap.node);
                }
                "const.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Constant);
                }
                "const.def" => {
                    def_node = Some(cap.node);
                }
                "static.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Constant);
                }
                "static.def" => {
                    def_node = Some(cap.node);
                }
                "mod.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Module);
                }
                "mod.def" => {
                    def_node = Some(cap.node);
                }
                "import.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Import);
                }
                "import.def" => {
                    def_node = Some(cap.node);
                }
                "test.name" => {
                    name = Some(text.to_string());
                    kind = Some(SymbolKind::Test);
                }
                "test.def" => {
                    def_node = Some(cap.node);
                }
                _ => {}
            }
        }

        if let (Some(name), Some(kind), Some(node)) = (name, kind, def_node) {
            let start = node.start_position();
            let end = node.end_position();
            let byte_range = (node.start_byte(), node.end_byte());
            let line_range = (start.row + 1, end.row + 1); // 1-indexed

            // Extract signature (first line of the definition)
            let node_text = node.utf8_text(source.as_bytes()).unwrap_or("");
            let signature = node_text.lines().next().unwrap_or("").to_string();

            symbols.push(Symbol {
                name,
                kind,
                file: rel_path.to_string(),
                byte_range,
                line_range,
                language,
                signature,
                definition: None,
                parent,
            });
        }
    }

    if language == Language::Elixir {
        apply_elixir_function_identities(&mut symbols);
    }

    // Deduplicate symbols by (file, name): keep the one with the larger byte range
    // (the more complete definition). On equal ranges, prefer specific kinds
    // (Struct/Enum/Class/etc.) over the generic Constant/Variable/Other catch-alls
    // — some grammars (e.g. Zig) emit both for the same node.
    {
        use std::collections::HashMap;
        let kind_priority = |k: SymbolKind| -> u8 {
            match k {
                SymbolKind::Other
                | SymbolKind::Variable
                | SymbolKind::Constant
                | SymbolKind::Import => 1,
                _ => 10,
            }
        };
        let mut best: HashMap<String, usize> = HashMap::new();
        for (i, sym) in symbols.iter().enumerate() {
            let key = format!("{}::{}", sym.file, sym.name);
            let range_size = sym.byte_range.1.saturating_sub(sym.byte_range.0);
            if let Some(&prev_idx) = best.get(&key) {
                let prev_size = symbols[prev_idx]
                    .byte_range
                    .1
                    .saturating_sub(symbols[prev_idx].byte_range.0);
                if range_size > prev_size {
                    best.insert(key, i);
                } else if range_size == prev_size
                    && kind_priority(sym.kind) > kind_priority(symbols[prev_idx].kind)
                {
                    best.insert(key, i);
                }
            } else {
                best.insert(key, i);
            }
        }
        let keep: std::collections::HashSet<usize> = best.into_values().collect();
        let mut idx = 0;
        symbols.retain(|_| {
            let k = idx;
            idx += 1;
            keep.contains(&k)
        });
    }

    // For Python: associate methods with their parent class by checking if a
    // function's byte_range is contained within a class's byte_range.
    if language == Language::Python {
        // Collect class ranges
        let class_ranges: Vec<(String, usize, usize)> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .map(|s| (s.name.clone(), s.byte_range.0, s.byte_range.1))
            .collect();

        for sym in symbols.iter_mut() {
            if sym.kind == SymbolKind::Function && sym.parent.is_none() {
                for (class_name, start, end) in &class_ranges {
                    if sym.byte_range.0 >= *start && sym.byte_range.1 <= *end {
                        sym.kind = SymbolKind::Method;
                        sym.parent = Some(class_name.clone());
                        break;
                    }
                }
            }
        }
    }

    debug!("Extracted {} symbols from {}", symbols.len(), rel_path);
    Ok(symbols)
}

fn apply_elixir_function_identities(symbols: &mut [Symbol]) {
    use std::collections::HashMap;

    let mut clause_counts: HashMap<String, usize> = HashMap::new();

    for symbol in symbols.iter_mut() {
        if symbol.kind != SymbolKind::Function {
            continue;
        }

        let arity = elixir_arity_from_signature(&symbol.signature, &symbol.name);
        let name_with_arity = format!("{}/{}", symbol.name, arity);
        let clause_count = clause_counts.entry(name_with_arity.clone()).or_insert(0);
        *clause_count += 1;

        symbol.name = if *clause_count == 1 {
            name_with_arity
        } else {
            format!("{}#clause{}", name_with_arity, clause_count)
        };
    }
}

fn elixir_arity_from_signature(signature: &str, name: &str) -> usize {
    let Some(name_start) = signature.find(name) else {
        return 0;
    };
    let after_name = &signature[name_start + name.len()..];
    let Some(paren_offset) = after_name.find('(') else {
        return 0;
    };
    let args_start = name_start + name.len() + paren_offset;
    let Some(args_end) = find_matching_paren(signature, args_start) else {
        return 0;
    };

    count_elixir_arguments(&signature[args_start + 1..args_end])
}

fn find_matching_paren(text: &str, open_byte: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string: Option<char> = None;
    let mut escaped = false;

    for (idx, ch) in text[open_byte..].char_indices() {
        let byte_idx = open_byte + idx;

        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }

        match ch {
            '"' | '\'' => in_string = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(byte_idx);
                }
            }
            _ => {}
        }
    }

    None
}

fn count_elixir_arguments(args: &str) -> usize {
    if args.trim().is_empty() {
        return 0;
    }

    let mut count = 1usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut in_string: Option<char> = None;
    let mut escaped = false;

    for ch in args.chars() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }

        match ch {
            '"' | '\'' => in_string = Some(ch),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            ',' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => count += 1,
            _ => {}
        }
    }

    count
}

/// Extract symbols from all files in the tree. Runs on blocking threads
/// with bounded concurrency.
pub async fn extract_all_symbols(
    root: &Path,
    file_tree: &Arc<FileTree>,
    symbol_table: &Arc<SymbolTable>,
) -> Result<usize> {
    let root = root.to_path_buf();
    let file_tree = file_tree.clone();
    let symbol_table = symbol_table.clone();

    let count = tokio::task::spawn_blocking(move || -> Result<usize> {
        let mut total = 0;

        let paths: Vec<(String, Language)> = file_tree
            .files
            .iter()
            .filter(|e| e.value().language.has_tree_sitter_support())
            .map(|e| (e.key().clone(), e.value().language))
            .collect();

        for (rel_path, language) in paths {
            match extract_symbols_from_file(&root, &rel_path, language) {
                Ok(symbols) => {
                    let count = symbols.len();
                    for sym in symbols {
                        symbol_table.insert(sym);
                    }
                    // Mark file as having symbols extracted
                    if let Some(mut entry) = file_tree.files.get_mut(&rel_path) {
                        entry.symbols_extracted = true;
                    }
                    total += count;
                }
                Err(e) => {
                    debug!("Failed to extract symbols from {}: {}", rel_path, e);
                }
            }
        }

        Ok(total)
    })
    .await??;

    Ok(count)
}
