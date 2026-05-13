use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use tree_sitter::StreamingIterator;

use crate::index::file_entry::Language;
use crate::index::file_tree::FileTree;
use crate::symbols::SymbolTable;
use crate::symbols::queries;
use crate::symbols::symbol::{Symbol, SymbolKind};

pub fn list_symbols(
    symbol_table: &Arc<SymbolTable>,
    kind_filter: Option<SymbolKind>,
    file_filter: Option<&str>,
    limit: usize,
) -> Vec<Symbol> {
    let mut results: Vec<Symbol> = if let Some(file) = file_filter {
        symbol_table.list_by_file(file)
    } else {
        symbol_table.all_symbols()
    };

    if let Some(kind) = kind_filter {
        results.retain(|s| s.kind == kind);
    }

    results.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line_range.0.cmp(&b.line_range.0))
    });
    results.truncate(limit);
    results
}

pub fn search_symbols(
    symbol_table: &Arc<SymbolTable>,
    query: &str,
    limit: usize,
    file_filter: Option<&str>,
) -> Vec<Symbol> {
    let mut results = symbol_table.search(
        query,
        if file_filter.is_some() {
            limit * 5
        } else {
            limit
        },
    );

    // Apply file filter if provided
    if let Some(file) = file_filter {
        results.retain(|s| s.file == file || s.file.contains(file));
    }

    // If few results, also search by signature substring
    if results.len() < limit {
        let query_lower = query.to_lowercase();
        let existing_keys: std::collections::HashSet<String> = results
            .iter()
            .map(|s| format!("{}::{}", s.file, s.name))
            .collect();

        for entry in symbol_table.symbols.iter() {
            let sym = entry.value();
            if existing_keys.contains(&format!("{}::{}", sym.file, sym.name)) {
                continue;
            }
            if sym.signature.to_lowercase().contains(&query_lower) {
                if let Some(file) = file_filter {
                    if sym.file != file && !sym.file.contains(file) {
                        continue;
                    }
                }
                results.push(sym.clone());
                if results.len() >= limit {
                    break;
                }
            }
        }
    }

    results.truncate(limit);
    results
}

pub fn get_implementation(
    root: &Path,
    symbol_table: &Arc<SymbolTable>,
    symbol_name: &str,
    file: &str,
) -> Result<String, String> {
    // Try exact lookup first
    let sym = symbol_table.get(file, symbol_name).or_else(|| {
        // Fuzzy fallback: search by name and filter by file
        let candidates = symbol_table.search(symbol_name, 50);
        candidates
            .into_iter()
            .find(|s| s.file == file && symbol_matches_query(s, symbol_name))
            .or_else(|| {
                // Broader: any symbol in the file whose name contains the query
                let file_symbols = symbol_table.list_by_file(file);
                file_symbols
                    .into_iter()
                    .find(|s| symbol_matches_query(s, symbol_name))
            })
    });

    let sym = sym.ok_or_else(|| format!("Symbol '{}' not found in '{}'", symbol_name, file))?;

    let abs_path = root.join(&sym.file);
    let source = std::fs::read_to_string(&abs_path)
        .map_err(|e| format!("Failed to read '{}': {}", sym.file, e))?;

    let start = sym.byte_range.0;
    let end = sym.byte_range.1.min(source.len());
    Ok(source[start..end].to_string())
}

pub fn define_symbol(
    symbol_table: &Arc<SymbolTable>,
    symbol_name: &str,
    file: &str,
    definition: &str,
) -> Result<(), String> {
    let key = SymbolTable::make_key(file, symbol_name);
    if let Some(mut sym) = symbol_table.symbols.get_mut(&key) {
        if sym.definition.is_some() {
            return Err(format!(
                "Symbol '{}' in '{}' already has a definition. Use redefine.",
                symbol_name, file
            ));
        }
        sym.definition = Some(definition.to_string());
        Ok(())
    } else {
        Err(format!("Symbol '{}' not found in '{}'", symbol_name, file))
    }
}

pub fn redefine_symbol(
    symbol_table: &Arc<SymbolTable>,
    symbol_name: &str,
    file: &str,
    definition: &str,
) -> Result<(), String> {
    let key = SymbolTable::make_key(file, symbol_name);
    if let Some(mut sym) = symbol_table.symbols.get_mut(&key) {
        sym.definition = Some(definition.to_string());
        Ok(())
    } else {
        Err(format!("Symbol '{}' not found in '{}'", symbol_name, file))
    }
}

/// Find callers of a symbol using tree-sitter call-expression queries.
/// Falls back to regex for files without tree-sitter support.
pub fn find_callers(
    root: &Path,
    file_tree: &Arc<FileTree>,
    symbol_table: &Arc<SymbolTable>,
    symbol_name: &str,
    file: &str,
    limit: usize,
) -> Result<Vec<CallerInfo>, String> {
    // Verify symbol exists
    let sym = find_symbol_for_lookup(symbol_table, file, symbol_name)
        .ok_or_else(|| format!("Symbol '{}' not found in '{}'", symbol_name, file))?;
    let call_name = callable_name_for_symbol(&sym);

    let mut callers = Vec::new();

    for entry in file_tree.files.iter() {
        let rel_path = entry.key().clone();
        let language = entry.value().language;
        let abs_path = root.join(&rel_path);

        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if !source.contains(&call_name) {
            continue;
        }

        let file_callers = if language.has_tree_sitter_support() {
            find_callers_ast(&source, &rel_path, language, &call_name, file)
        } else {
            find_callers_regex(&source, &rel_path, &call_name, file)
        };

        for caller in file_callers {
            callers.push(caller);
            if callers.len() >= limit {
                return Ok(callers);
            }
        }
    }

    Ok(callers)
}

/// AST-aware caller detection: parse the file, run the callers query,
/// and check if any call-expression callee matches the target symbol name.
fn find_callers_ast(
    source: &str,
    rel_path: &str,
    language: Language,
    symbol_name: &str,
    definition_file: &str,
) -> Vec<CallerInfo> {
    let config = match queries::get_language_config(language) {
        Some(c) => c,
        None => return find_callers_regex(source, rel_path, symbol_name, definition_file),
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&config.language).is_err() {
        return find_callers_regex(source, rel_path, symbol_name, definition_file);
    }

    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return find_callers_regex(source, rel_path, symbol_name, definition_file),
    };

    let query = match tree_sitter::Query::new(&config.language, config.callers_query) {
        Ok(q) => q,
        Err(_) => return find_callers_regex(source, rel_path, symbol_name, definition_file),
    };

    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let callee_idx = capture_names.iter().position(|n| n == "callee");

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut callers = Vec::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if Some(cap.index as usize) == callee_idx {
                let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");
                if text == symbol_name {
                    let line_num = cap.node.start_position().row + 1;
                    // Skip the definition itself
                    if rel_path == definition_file {
                        let line_text = source.lines().nth(line_num - 1).unwrap_or("");
                        if is_definition_line(line_text, symbol_name, language) {
                            continue;
                        }
                    }
                    let line_text = source
                        .lines()
                        .nth(line_num - 1)
                        .map(|l| l.trim().to_string())
                        .unwrap_or_default();
                    callers.push(CallerInfo {
                        file: rel_path.to_string(),
                        line: line_num,
                        text: line_text,
                    });
                }
            }
        }
    }

    callers
}

/// Regex fallback for files without tree-sitter support.
fn find_callers_regex(
    source: &str,
    rel_path: &str,
    symbol_name: &str,
    definition_file: &str,
) -> Vec<CallerInfo> {
    let pattern = match regex::Regex::new(&regex::escape(symbol_name)) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let mut callers = Vec::new();

    for (line_num, line) in source.lines().enumerate() {
        if pattern.is_match(line) {
            // Skip the definition itself
            if rel_path == definition_file
                && (line.contains(&format!("fn {}", symbol_name))
                    || line.contains(&format!("def {}", symbol_name))
                    || line.contains(&format!("defp {}", symbol_name))
                    || line.contains(&format!("defmacro {}", symbol_name))
                    || line.contains(&format!("defmacrop {}", symbol_name))
                    || line.contains(&format!("defguard {}", symbol_name))
                    || line.contains(&format!("defguardp {}", symbol_name))
                    || line.contains(&format!("defmodule {}", symbol_name))
                    || line.contains(&format!("defprotocol {}", symbol_name))
                    || line.contains(&format!("defimpl {}", symbol_name))
                    || line.contains(&format!("function {}", symbol_name))
                    || line.contains(&format!("func {}", symbol_name))
                    || line.contains(&format!("class {}", symbol_name))
                    || line.contains(&format!("interface {}", symbol_name))
                    || line.contains(&format!("object {}", symbol_name))
                    || line.contains(&format!("trait {}", symbol_name)))
            {
                continue;
            }

            callers.push(CallerInfo {
                file: rel_path.to_string(),
                line: line_num + 1,
                text: line.trim().to_string(),
            });
        }
    }

    callers
}

fn is_definition_line(line: &str, name: &str, language: Language) -> bool {
    match language {
        Language::Rust => line.contains(&format!("fn {}", name)),
        Language::Python => line.contains(&format!("def {}", name)),
        Language::TypeScript | Language::JavaScript => {
            line.contains(&format!("function {}", name)) || line.contains(&format!("{} =", name))
        }
        Language::Go => line.contains(&format!("func {}", name)),
        Language::Java => {
            line.contains(&format!("class {}", name))
                || line.contains(&format!("interface {}", name))
                || line.contains(&format!("enum {}", name))
                || (line.contains(name)
                    && (line.contains("void ")
                        || line.contains("int ")
                        || line.contains("String ")
                        || line.contains("boolean ")
                        || line.contains("long ")
                        || line.contains("double ")
                        || line.contains("float ")
                        || line.contains("public ")
                        || line.contains("private ")
                        || line.contains("protected ")))
        }
        Language::Scala => {
            line.contains(&format!("def {}", name))
                || line.contains(&format!("object {}", name))
                || line.contains(&format!("class {}", name))
                || line.contains(&format!("trait {}", name))
        }
        Language::Elixir => {
            line.contains(&format!("def {}", name))
                || line.contains(&format!("defp {}", name))
                || line.contains(&format!("defmacro {}", name))
                || line.contains(&format!("defmacrop {}", name))
                || line.contains(&format!("defguard {}", name))
                || line.contains(&format!("defguardp {}", name))
                || line.contains(&format!("defmodule {}", name))
                || line.contains(&format!("defprotocol {}", name))
                || line.contains(&format!("defimpl {}", name))
        }
        Language::Ruby => {
            line.contains(&format!("def {}", name))
                || line.contains(&format!("def self.{}", name))
                || line.contains(&format!("class {}", name))
                || line.contains(&format!("module {}", name))
                || line.contains(&format!("alias {}", name))
                || (line.contains(name)
                    && (line.contains("attr_reader")
                        || line.contains("attr_writer")
                        || line.contains("attr_accessor")
                        || line.contains("define_method")
                        || line.contains("alias_method")))
        }
        Language::Php => {
            line.contains(&format!("function {}", name))
                || line.contains(&format!("class {}", name))
                || line.contains(&format!("interface {}", name))
                || line.contains(&format!("trait {}", name))
                || line.contains(&format!("enum {}", name))
                || line.contains(&format!("namespace {}", name))
                || (line.contains(name) && (line.contains("const ") || line.contains("case ")))
        }
        Language::Zig => {
            line.contains(&format!("fn {}", name))
                || line.contains(&format!("const {}", name))
                || line.contains(&format!("var {}", name))
                || (line.starts_with("test ") && line.contains(name))
        }
        Language::Sql => {
            let lower = line.to_lowercase();
            let lower_name = name.to_lowercase();
            lower.contains("create") && lower.contains(&lower_name)
        }
        _ => false,
    }
}

#[derive(Debug, serde::Serialize)]
pub struct CallerInfo {
    pub file: String,
    pub line: usize,
    pub text: String,
}

/// Find test functions that reference a given symbol.
pub fn find_tests(
    root: &Path,
    file_tree: &Arc<FileTree>,
    symbol_table: &Arc<SymbolTable>,
    symbol_name: &str,
    file: &str,
    limit: usize,
) -> Result<Vec<TestInfo>, String> {
    let sym = find_symbol_for_lookup(symbol_table, file, symbol_name)
        .ok_or_else(|| format!("Symbol '{}' not found in '{}'", symbol_name, file))?;

    if sym.language == Language::Elixir {
        return Ok(find_exunit_tests(
            root,
            file_tree,
            &callable_name_for_symbol(&sym),
            limit,
        ));
    }

    let mut tests = Vec::new();

    // Look through all symbols for test functions
    for entry in symbol_table.symbols.iter() {
        let sym = entry.value();
        if !is_test_symbol(sym) {
            continue;
        }

        // Read the test function body and check if it references the target symbol
        let abs_path = root.join(&sym.file);
        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let start = sym.byte_range.0;
        let end = sym.byte_range.1.min(source.len());
        let body = &source[start..end];

        if body.contains(symbol_name) {
            tests.push(TestInfo {
                name: sym.name.clone(),
                file: sym.file.clone(),
                line: sym.line_range.0,
                signature: sym.signature.clone(),
            });

            if tests.len() >= limit {
                break;
            }
        }
    }

    Ok(tests)
}

fn find_symbol_for_lookup(
    symbol_table: &Arc<SymbolTable>,
    file: &str,
    symbol_name: &str,
) -> Option<Symbol> {
    symbol_table.get(file, symbol_name).or_else(|| {
        let mut file_symbols = symbol_table.list_by_file(file);
        file_symbols.sort_by_key(|s| s.line_range.0);
        file_symbols
            .into_iter()
            .find(|s| symbol_matches_query(s, symbol_name))
    })
}

fn symbol_matches_query(symbol: &Symbol, query: &str) -> bool {
    let symbol_name = symbol.name.to_lowercase();
    let query = query.to_lowercase();

    symbol_name == query
        || (symbol.language == Language::Elixir && elixir_bare_symbol_name(&symbol_name) == query)
}

fn callable_name_for_symbol(symbol: &Symbol) -> String {
    if symbol.language == Language::Elixir {
        elixir_bare_symbol_name(&symbol.name).to_string()
    } else {
        symbol.name.clone()
    }
}

fn elixir_bare_symbol_name(name: &str) -> &str {
    let without_clause = name.split_once("#clause").map_or(name, |(base, _)| base);
    without_clause
        .split_once('/')
        .map_or(without_clause, |(bare, _)| bare)
}

fn is_test_symbol(sym: &Symbol) -> bool {
    if sym.kind == crate::symbols::symbol::SymbolKind::Test {
        return true;
    }
    match sym.language {
        Language::Rust => sym.name.starts_with("test") || sym.file.contains("/tests/"),
        Language::Python => {
            sym.name.starts_with("test_")
                || sym.file.contains("test_")
                || sym.file.contains("_test.")
        }
        Language::TypeScript | Language::JavaScript => {
            sym.file.contains(".test.")
                || sym.file.contains(".spec.")
                || sym.file.contains("__tests__")
        }
        Language::Go => sym.name.starts_with("Test") || sym.file.ends_with("_test.go"),
        Language::Java => sym.file.contains("Test") || sym.file.contains("/test/"),
        Language::Scala => {
            sym.file.contains("Spec") || sym.file.contains("Test") || sym.file.contains("/test/")
        }
        Language::Elixir => {
            sym.file.ends_with("_test.exs")
                || sym.file.contains("/test/")
                || sym.file.contains("/test/support/")
        }
        Language::Ruby => {
            sym.name.starts_with("test_")
                || sym.file.ends_with("_spec.rb")
                || sym.file.ends_with("_test.rb")
                || sym.file.contains("/spec/")
                || sym.file.contains("/test/")
        }
        Language::Php => {
            sym.name.starts_with("test")
                || sym.file.ends_with("Test.php")
                || sym.file.contains("/tests/")
                || sym.file.contains("/Tests/")
        }
        Language::Zig => false, // handled by SymbolKind::Test short-circuit above
        Language::Sql => false,
        _ => false,
    }
}

#[derive(Debug, serde::Serialize)]
pub struct TestInfo {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub signature: String,
}

/// List local variables within a function using tree-sitter queries.
/// Falls back to regex for languages without tree-sitter support.
pub fn list_variables(
    root: &Path,
    symbol_table: &Arc<SymbolTable>,
    function_name: &str,
    file: &str,
) -> Result<Vec<VariableInfo>, String> {
    let sym = find_symbol_for_lookup(symbol_table, file, function_name)
        .ok_or_else(|| format!("Symbol '{}' not found in '{}'", function_name, file))?;

    let abs_path = root.join(&sym.file);
    let source = std::fs::read_to_string(&abs_path)
        .map_err(|e| format!("Failed to read '{}': {}", sym.file, e))?;

    let start = sym.byte_range.0;
    let end = sym.byte_range.1.min(source.len());

    let variables = if sym.language.has_tree_sitter_support() {
        list_variables_ast(
            &source,
            sym.language,
            start,
            end,
            &callable_name_for_symbol(&sym),
        )
    } else {
        list_variables_regex(
            &source[start..end],
            sym.language,
            &callable_name_for_symbol(&sym),
        )
    };

    Ok(variables)
}

/// AST-aware variable extraction: parse the function body slice, run the
/// variables query, and collect all @var.name captures within the byte range.
fn list_variables_ast(
    source: &str,
    language: Language,
    fn_start: usize,
    fn_end: usize,
    function_name: &str,
) -> Vec<VariableInfo> {
    let config = match queries::get_language_config(language) {
        Some(c) => c,
        None => return list_variables_regex(&source[fn_start..fn_end], language, function_name),
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&config.language).is_err() {
        return list_variables_regex(&source[fn_start..fn_end], language, function_name);
    }

    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return list_variables_regex(&source[fn_start..fn_end], language, function_name),
    };

    let query = match tree_sitter::Query::new(&config.language, config.variables_query) {
        Ok(q) => q,
        Err(_) => return list_variables_regex(&source[fn_start..fn_end], language, function_name),
    };

    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let var_name_idx = capture_names.iter().position(|n| n == "var.name");

    let mut cursor = tree_sitter::QueryCursor::new();
    // Restrict matches to the function's byte range
    cursor.set_byte_range(fn_start..fn_end);
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut variables = Vec::new();
    let mut seen = std::collections::HashSet::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if Some(cap.index as usize) == var_name_idx {
                let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");
                if !text.is_empty()
                    && text != "self"
                    && text != "_"
                    && seen.insert(text.to_string())
                {
                    variables.push(VariableInfo {
                        name: text.to_string(),
                        function: function_name.to_string(),
                    });
                }
            }
        }
    }

    variables
}

/// Regex fallback for variable extraction.
fn list_variables_regex(body: &str, language: Language, function_name: &str) -> Vec<VariableInfo> {
    let mut variables = Vec::new();

    match language {
        Language::Rust => {
            let let_re = regex::Regex::new(r"let\s+(mut\s+)?(\w+)").unwrap();
            for cap in let_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[2].to_string(),
                    function: function_name.to_string(),
                });
            }
        }
        Language::Python => {
            let assign_re = regex::Regex::new(r"^\s+(\w+)\s*=").unwrap();
            for cap in assign_re.captures_iter(body) {
                let name = cap[1].to_string();
                if name != "self" && !name.starts_with('_') {
                    variables.push(VariableInfo {
                        name,
                        function: function_name.to_string(),
                    });
                }
            }
        }
        Language::TypeScript | Language::JavaScript => {
            let var_re = regex::Regex::new(r"(?:let|const|var)\s+(\w+)").unwrap();
            for cap in var_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
        }
        Language::Go => {
            let short_re = regex::Regex::new(r"(\w+)\s*:=").unwrap();
            for cap in short_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
            let var_re = regex::Regex::new(r"var\s+(\w+)").unwrap();
            for cap in var_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
        }
        Language::Java => {
            let var_re = regex::Regex::new(r"\b(?:int|long|float|double|boolean|char|byte|short|String|var|final\s+\w+)\s+(\w+)\s*[=;,)]").unwrap();
            for cap in var_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
        }
        Language::Scala => {
            let val_re = regex::Regex::new(r"\b(?:val|var)\s+(\w+)").unwrap();
            for cap in val_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
        }
        Language::Elixir => {
            let param_re =
                regex::Regex::new(r"\bdef(?:p|macro|macrop|guard|guardp)?\s+\w+\s*\(([^)]*)\)")
                    .unwrap();
            let assign_re = regex::Regex::new(r"\b([a-z_][a-zA-Z0-9_]*)\s*=").unwrap();
            let bind_re = regex::Regex::new(r"\b([a-z_][a-zA-Z0-9_]*)\s*<-").unwrap();
            let identifier_re = regex::Regex::new(r"\b([a-z_][a-zA-Z0-9_]*)\b").unwrap();

            for cap in param_re.captures_iter(body) {
                for segment in cap[1].split(',') {
                    if let Some(id) = identifier_re.captures(segment) {
                        variables.push(VariableInfo {
                            name: id[1].to_string(),
                            function: function_name.to_string(),
                        });
                    }
                }
            }

            for cap in assign_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }

            for cap in bind_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
        }
        Language::Ruby => {
            let assign_re = regex::Regex::new(r"^\s+(\w+)\s*=").unwrap();
            for cap in assign_re.captures_iter(body) {
                let name = cap[1].to_string();
                if name != "self" && !name.chars().next().is_some_and(|c| c.is_uppercase()) {
                    variables.push(VariableInfo {
                        name,
                        function: function_name.to_string(),
                    });
                }
            }
        }
        Language::Php => {
            let var_re = regex::Regex::new(r"\$(\w+)").unwrap();
            for cap in var_re.captures_iter(body) {
                let name = cap[1].to_string();
                if name != "this" {
                    variables.push(VariableInfo {
                        name,
                        function: function_name.to_string(),
                    });
                }
            }
        }
        Language::Zig => {
            let var_re = regex::Regex::new(r"\b(?:var|const)\s+(\w+)").unwrap();
            for cap in var_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
        }
        Language::Sql => {
            let declare_re = regex::Regex::new(r"(?i)DECLARE\s+@?(\w+)").unwrap();
            for cap in declare_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
            let plsql_re = regex::Regex::new(r"(\w+)\s+\w+\s*:=").unwrap();
            for cap in plsql_re.captures_iter(body) {
                variables.push(VariableInfo {
                    name: cap[1].to_string(),
                    function: function_name.to_string(),
                });
            }
        }
        _ => {}
    }

    // Deduplicate
    variables.sort_by(|a, b| a.name.cmp(&b.name));
    variables.dedup_by(|a, b| a.name == b.name);

    variables
}

fn find_exunit_tests(
    root: &Path,
    file_tree: &Arc<FileTree>,
    symbol_name: &str,
    limit: usize,
) -> Vec<TestInfo> {
    let mut tests = Vec::new();

    for entry in file_tree.files.iter() {
        let rel_path = entry.key();
        let file = entry.value();
        if file.language != Language::Elixir || !is_elixir_test_file(rel_path) {
            continue;
        }

        let abs_path = root.join(rel_path);
        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let language = tree_sitter_elixir::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&language).is_err() {
            continue;
        }

        let tree = match parser.parse(&source, None) {
            Some(t) => t,
            None => continue,
        };

        let blocks = find_exunit_blocks(&source, &tree, &language);
        let describes: Vec<_> = blocks
            .iter()
            .filter(|block| block.kind == "describe")
            .collect();

        for block in blocks.iter().filter(|block| block.kind == "test") {
            if !range_contains_elixir_call(&source, &language, block.start..block.end, symbol_name)
            {
                continue;
            }

            let mut context: Vec<&str> = describes
                .iter()
                .filter(|describe| describe.start <= block.start && block.end <= describe.end)
                .map(|describe| describe.label.as_str())
                .collect();
            context.push(block.label.as_str());

            tests.push(TestInfo {
                name: format!("test {}", context.join(" > ")),
                file: rel_path.clone(),
                line: block.line,
                signature: block.signature.clone(),
            });

            if tests.len() >= limit {
                return tests;
            }
        }
    }

    tests
}

struct ExUnitBlock {
    kind: String,
    label: String,
    start: usize,
    end: usize,
    line: usize,
    signature: String,
}

fn find_exunit_blocks(
    source: &str,
    tree: &tree_sitter::Tree,
    language: &tree_sitter::Language,
) -> Vec<ExUnitBlock> {
    let query = match tree_sitter::Query::new(
        language,
        r#"
(call
  target: (identifier) @test.kind
  (arguments
    [
      (string) @test.name
      (charlist) @test.name
      (sigil) @test.name
    ])
  (#any-of? @test.kind "test" "describe")) @test.def
"#,
    ) {
        Ok(q) => q,
        Err(_) => return Vec::new(),
    };

    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let kind_idx = capture_names.iter().position(|n| n == "test.kind");
    let name_idx = capture_names.iter().position(|n| n == "test.name");
    let def_idx = capture_names.iter().position(|n| n == "test.def");

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut blocks = Vec::new();

    while let Some(m) = matches.next() {
        let mut kind = None;
        let mut name = None;
        let mut def_node = None;

        for cap in m.captures {
            let idx = cap.index as usize;
            if Some(idx) == kind_idx {
                kind = cap
                    .node
                    .utf8_text(source.as_bytes())
                    .ok()
                    .map(str::to_string);
            } else if Some(idx) == name_idx {
                name = cap
                    .node
                    .utf8_text(source.as_bytes())
                    .ok()
                    .map(str::to_string);
            } else if Some(idx) == def_idx {
                def_node = Some(cap.node);
            }
        }

        let Some(def_node) = def_node else {
            continue;
        };
        let kind = kind.unwrap_or_else(|| "test".to_string());
        let label = clean_elixir_test_name(name.as_deref().unwrap_or(&kind));

        blocks.push(ExUnitBlock {
            kind,
            label,
            start: def_node.start_byte(),
            end: def_node.end_byte().min(source.len()),
            line: def_node.start_position().row + 1,
            signature: source
                .lines()
                .nth(def_node.start_position().row)
                .map(|line| line.trim().to_string())
                .unwrap_or_default(),
        });
    }

    blocks.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    blocks
}

fn range_contains_elixir_call(
    source: &str,
    language: &tree_sitter::Language,
    range: Range<usize>,
    symbol_name: &str,
) -> bool {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(language).is_err() {
        return false;
    }

    let Some(tree) = parser.parse(source, None) else {
        return false;
    };

    let query = match tree_sitter::Query::new(language, queries::elixir::CALLERS_QUERY) {
        Ok(q) => q,
        Err(_) => return false,
    };

    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let callee_idx = capture_names.iter().position(|n| n == "callee");

    let mut cursor = tree_sitter::QueryCursor::new();
    cursor.set_byte_range(range);
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if Some(cap.index as usize) == callee_idx
                && cap.node.utf8_text(source.as_bytes()).unwrap_or("") == symbol_name
            {
                return true;
            }
        }
    }

    false
}

fn is_elixir_test_file(path: &str) -> bool {
    path.ends_with("_test.exs") || path.contains("/test/")
}

fn clean_elixir_test_name(name: &str) -> String {
    name.trim().trim_matches('"').trim_matches('\'').to_string()
}

#[derive(Debug, serde::Serialize)]
pub struct VariableInfo {
    pub name: String,
    pub function: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::index::file_entry::FileEntry;

    const SAMPLE_FILE: &str = "lib/sample.ex";
    const TEST_FILE: &str = "test/sample_test.exs";

    fn fixture_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("elixir")
    }

    fn index_elixir_fixture() -> (std::path::PathBuf, Arc<FileTree>, Arc<SymbolTable>) {
        let root = fixture_root();
        let file_tree = Arc::new(FileTree::new());
        let symbol_table = Arc::new(SymbolTable::new());

        for rel_path in [SAMPLE_FILE, TEST_FILE] {
            let abs_path = root.join(rel_path);
            let size = std::fs::metadata(&abs_path).unwrap().len();
            let entry = FileEntry::new(rel_path.to_string(), size, Utc::now());
            file_tree.insert(entry);

            let language = file_tree.get(rel_path).unwrap().language;
            for symbol in
                crate::symbols::parser::extract_symbols_from_file(&root, rel_path, language)
                    .unwrap()
            {
                symbol_table.insert(symbol);
            }
        }

        (root, file_tree, symbol_table)
    }

    #[test]
    fn elixir_fixture_extracts_modules_public_private_and_guarded_functions() {
        let (_root, _file_tree, symbol_table) = index_elixir_fixture();

        let sample = symbol_table.get(SAMPLE_FILE, "Fixture.Sample").unwrap();
        assert_eq!(sample.kind, SymbolKind::Module);
        assert_eq!(sample.line_range, (1, 48));
        assert_eq!(sample.signature, "defmodule Fixture.Sample do");

        let public_fun = symbol_table.get(SAMPLE_FILE, "public_fun/2").unwrap();
        assert_eq!(public_fun.kind, SymbolKind::Function);
        assert_eq!(public_fun.line_range, (12, 20));
        assert_eq!(public_fun.signature, "def public_fun(user, opts) do");

        let guarded = symbol_table.get(SAMPLE_FILE, "guarded/1").unwrap();
        assert_eq!(guarded.kind, SymbolKind::Function);
        assert_eq!(guarded.line_range, (22, 24));
        assert_eq!(
            guarded.signature,
            "def guarded(value) when is_integer(value) do"
        );

        let private = symbol_table.get(SAMPLE_FILE, "normalize/1").unwrap();
        assert_eq!(private.kind, SymbolKind::Function);
        assert_eq!(private.line_range, (26, 28));
        assert_eq!(private.signature, "defp normalize(user) do");
    }

    #[test]
    fn elixir_fixture_extracts_static_alias_import_require_and_use_relationships() {
        let (root, file_tree, symbol_table) = index_elixir_fixture();

        let user_alias = symbol_table
            .get(SAMPLE_FILE, "Fixture.Accounts.User")
            .unwrap();
        assert_eq!(user_alias.kind, SymbolKind::Import);
        assert_eq!(user_alias.signature, "alias Fixture.Accounts.User");

        let aliased_as = symbol_table
            .get(SAMPLE_FILE, "Fixture.Accounts.UserProfile")
            .unwrap();
        assert_eq!(aliased_as.kind, SymbolKind::Import);
        assert_eq!(
            aliased_as.signature,
            "alias Fixture.Accounts.UserProfile, as: AccountUserProfile"
        );

        let imported = symbol_table.get(SAMPLE_FILE, "Ecto.Query").unwrap();
        assert_eq!(imported.kind, SymbolKind::Import);
        assert_eq!(imported.signature, "import Ecto.Query");

        let required = symbol_table.get(SAMPLE_FILE, "Logger").unwrap();
        assert_eq!(required.kind, SymbolKind::Import);
        assert_eq!(required.signature, "require Logger");

        let used = symbol_table.get(SAMPLE_FILE, "GenServer").unwrap();
        assert_eq!(used.kind, SymbolKind::Import);
        assert_eq!(used.signature, "use GenServer");

        let imports = list_symbols(
            &symbol_table,
            Some(SymbolKind::Import),
            Some(SAMPLE_FILE),
            10,
        );
        let import_names: Vec<_> = imports.iter().map(|symbol| symbol.name.as_str()).collect();
        assert_eq!(
            import_names,
            [
                "Fixture.Accounts.User",
                "Fixture.Accounts.UserProfile",
                "Ecto.Query",
                "Logger",
                "GenServer"
            ]
        );

        let basic_structure = crate::ops::structure::get_structure_with_detail(
            &root,
            &file_tree,
            &symbol_table,
            2,
            0,
        );
        assert!(basic_structure.file_symbols.is_none());

        let detailed_structure = crate::ops::structure::get_structure_with_detail(
            &root,
            &file_tree,
            &symbol_table,
            2,
            1,
        );
        let sample_symbols = detailed_structure
            .file_symbols
            .unwrap()
            .into_iter()
            .find(|file| file.file == SAMPLE_FILE)
            .unwrap()
            .symbols;
        assert!(
            sample_symbols
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Import && symbol.name == "GenServer")
        );
    }

    #[test]
    fn elixir_fixture_ignores_relationships_in_comments_strings_and_dynamic_calls() {
        let (_root, _file_tree, symbol_table) = index_elixir_fixture();

        assert!(
            symbol_table
                .get(SAMPLE_FILE, "Fixture.Commented.Out")
                .is_none()
        );
        assert!(
            symbol_table
                .get(SAMPLE_FILE, "Fixture.StringNoise")
                .is_none()
        );
        assert!(symbol_table.get(SAMPLE_FILE, "dynamic_module").is_none());
    }

    #[test]
    fn elixir_fixture_distinguishes_arities_and_multi_clauses() {
        let (_root, _file_tree, symbol_table) = index_elixir_fixture();

        assert_eq!(
            symbol_table.get(SAMPLE_FILE, "add/1").unwrap().signature,
            "def add(value), do: value + 1"
        );
        assert_eq!(
            symbol_table.get(SAMPLE_FILE, "add/2").unwrap().signature,
            "def add(left, right), do: left + right"
        );
        assert_eq!(
            symbol_table
                .get(SAMPLE_FILE, "multi_clause/1")
                .unwrap()
                .signature,
            "def multi_clause(:ok), do: :ok"
        );
        assert_eq!(
            symbol_table
                .get(SAMPLE_FILE, "multi_clause/1#clause2")
                .unwrap()
                .signature,
            "def multi_clause(:error), do: :error"
        );
        assert_eq!(
            symbol_table
                .get(SAMPLE_FILE, "pattern_count/3")
                .unwrap()
                .signature,
            "def pattern_count({left, right}, [head | _tail], %{flag: flag}) when flag do"
        );
        assert_eq!(
            symbol_table
                .get(SAMPLE_FILE, "with_default/2")
                .unwrap()
                .signature,
            "def with_default(value, opts \\\\ []), do: {value, opts}"
        );
        assert!(symbol_table.get(SAMPLE_FILE, "with_default/1").is_none());
        assert_eq!(
            symbol_table
                .get(SAMPLE_FILE, "delegated/1")
                .unwrap()
                .signature,
            "defdelegate delegated(value), to: Fixture.Remote, as: :touch"
        );
    }

    #[test]
    fn elixir_fixture_bare_name_search_remains_useful() {
        let (_root, _file_tree, symbol_table) = index_elixir_fixture();

        let mut names: Vec<_> = search_symbols(&symbol_table, "add", 10, Some(SAMPLE_FILE))
            .into_iter()
            .map(|symbol| symbol.name)
            .collect();
        names.sort();

        assert_eq!(names, ["add/1", "add/2"]);
    }

    #[test]
    fn elixir_fixture_returns_implementation_by_stable_byte_range() {
        let (root, _file_tree, symbol_table) = index_elixir_fixture();

        let implementation =
            get_implementation(&root, &symbol_table, "public_fun", SAMPLE_FILE).unwrap();

        assert!(implementation.starts_with("def public_fun(user, opts) do"));
        assert!(implementation.contains("normalized = normalize(user)"));
        assert!(implementation.contains("remote = Fixture.Remote.touch(item)"));
        assert!(implementation.ends_with("  end"));
        assert!(!implementation.contains("def guarded"));
    }

    #[test]
    fn elixir_fixture_finds_code_callers_without_comment_or_string_hits() {
        let (root, file_tree, symbol_table) = index_elixir_fixture();

        let callers = find_callers(
            &root,
            &file_tree,
            &symbol_table,
            "normalize/1",
            SAMPLE_FILE,
            10,
        )
        .unwrap();

        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].file, SAMPLE_FILE);
        assert_eq!(callers[0].line, 13);
        assert_eq!(callers[0].text, "normalized = normalize(user)");
    }

    #[test]
    fn elixir_fixture_extracts_params_assignments_and_generator_bindings_once() {
        let (root, _file_tree, symbol_table) = index_elixir_fixture();

        let variables = list_variables(&root, &symbol_table, "public_fun", SAMPLE_FILE).unwrap();
        let mut names: Vec<_> = variables.iter().map(|var| var.name.as_str()).collect();
        names.sort_unstable();

        assert_eq!(
            names,
            [
                "dynamic_module",
                "item",
                "normalized",
                "opts",
                "remote",
                "user"
            ]
        );
    }

    #[test]
    fn elixir_fixture_finds_exunit_test_block_referencing_symbol() {
        let (root, file_tree, symbol_table) = index_elixir_fixture();

        let tests = find_tests(
            &root,
            &file_tree,
            &symbol_table,
            "public_fun/2",
            SAMPLE_FILE,
            10,
        )
        .unwrap();

        assert_eq!(tests.len(), 1);
        assert_eq!(
            tests[0].name,
            "test public_fun behavior > nested context > calls normalize directly"
        );
        assert_eq!(tests[0].file, TEST_FILE);
        assert_eq!(tests[0].line, 11);
        assert_eq!(tests[0].signature, "test \"calls normalize directly\" do");
    }

    #[test]
    fn elixir_fixture_ignores_setup_describe_and_description_only_mentions() {
        let (root, file_tree, symbol_table) = index_elixir_fixture();

        let tests =
            find_tests(&root, &file_tree, &symbol_table, "guarded", SAMPLE_FILE, 10).unwrap();

        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "test guarded can be referenced separately");
        assert_eq!(tests[0].file, TEST_FILE);
        assert_eq!(tests[0].line, 27);
        assert_eq!(
            tests[0].signature,
            "test \"guarded can be referenced separately\" do"
        );
    }

    #[test]
    fn elixir_fixture_keeps_helper_tests_source_line_grounded_and_bounded() {
        let (root, file_tree, symbol_table) = index_elixir_fixture();

        let tests = find_tests(
            &root,
            &file_tree,
            &symbol_table,
            "helper_for_public_fun",
            TEST_FILE,
            1,
        )
        .unwrap();

        assert_eq!(tests.len(), 1);
        assert_eq!(
            tests[0].name,
            "test guarded context without matching test body > uses a different helper"
        );
        assert_eq!(tests[0].file, TEST_FILE);
        assert_eq!(tests[0].line, 22);
        assert_eq!(tests[0].signature, "test \"uses a different helper\" do");
    }
}
