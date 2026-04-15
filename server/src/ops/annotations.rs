use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::index::file_entry::FileMark;
use crate::index::file_tree::FileTree;
use crate::symbols::SymbolTable;

const ANNOTATIONS_FILE: &str = ".coderlm/annotations.json";

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AnnotationData {
    /// File definitions: rel_path -> definition string
    #[serde(default)]
    pub file_definitions: HashMap<String, String>,
    /// File marks: rel_path -> list of mark strings
    #[serde(default)]
    pub file_marks: HashMap<String, Vec<String>>,
    /// Symbol definitions: "file::name" -> definition string
    #[serde(default)]
    pub symbol_definitions: HashMap<String, String>,
}

/// Save all annotations (file definitions, marks, symbol definitions)
/// to `.coderlm/annotations.json` in the project root.
pub fn save_annotations(
    root: &Path,
    file_tree: &Arc<FileTree>,
    symbol_table: &Arc<SymbolTable>,
) -> Result<(), String> {
    let mut data = AnnotationData::default();

    // Collect file definitions and marks
    for entry in file_tree.files.iter() {
        let fe = entry.value();
        if let Some(def) = &fe.definition {
            data.file_definitions
                .insert(fe.rel_path.clone(), def.clone());
        }
        if !fe.marks.is_empty() {
            let mark_strs: Vec<String> = fe
                .marks
                .iter()
                .map(|m| format!("{:?}", m).to_lowercase())
                .collect();
            data.file_marks.insert(fe.rel_path.clone(), mark_strs);
        }
    }

    // Collect symbol definitions
    for entry in symbol_table.symbols.iter() {
        let sym = entry.value();
        if let Some(def) = &sym.definition {
            let key = SymbolTable::make_key(&sym.file, &sym.name);
            data.symbol_definitions.insert(key, def.clone());
        }
    }

    let annotations_path = root.join(ANNOTATIONS_FILE);
    if let Some(parent) = annotations_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create annotations dir: {}", e))?;
    }

    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| format!("Failed to serialize annotations: {}", e))?;
    std::fs::write(&annotations_path, json)
        .map_err(|e| format!("Failed to write annotations: {}", e))?;

    debug!(
        "Saved annotations: {} file defs, {} file marks, {} symbol defs",
        data.file_definitions.len(),
        data.file_marks.len(),
        data.symbol_definitions.len()
    );

    Ok(())
}

/// Load annotations from `.coderlm/annotations.json` and apply them
/// to the file tree and symbol table.
pub fn load_annotations(
    root: &Path,
    file_tree: &Arc<FileTree>,
    symbol_table: &Arc<SymbolTable>,
) -> Result<AnnotationData, String> {
    let annotations_path = root.join(ANNOTATIONS_FILE);
    if !annotations_path.exists() {
        return Ok(AnnotationData::default());
    }

    let json = std::fs::read_to_string(&annotations_path)
        .map_err(|e| format!("Failed to read annotations: {}", e))?;
    let data: AnnotationData =
        serde_json::from_str(&json).map_err(|e| format!("Failed to parse annotations: {}", e))?;

    // Apply file definitions
    for (path, def) in &data.file_definitions {
        if let Some(mut entry) = file_tree.files.get_mut(path.as_str()) {
            entry.definition = Some(def.clone());
        } else {
            debug!("Annotation for missing file: {}", path);
        }
    }

    // Apply file marks
    for (path, marks) in &data.file_marks {
        if let Some(mut entry) = file_tree.files.get_mut(path.as_str()) {
            for mark_str in marks {
                if let Some(mark) = FileMark::from_str(mark_str) {
                    if !entry.marks.contains(&mark) {
                        entry.marks.push(mark);
                    }
                } else {
                    warn!("Unknown mark '{}' for file '{}'", mark_str, path);
                }
            }
        }
    }

    // Apply symbol definitions
    for (key, def) in &data.symbol_definitions {
        if let Some(mut sym) = symbol_table.symbols.get_mut(key) {
            sym.definition = Some(def.clone());
        } else {
            debug!("Annotation for missing symbol: {}", key);
        }
    }

    debug!(
        "Loaded annotations: {} file defs, {} file marks, {} symbol defs",
        data.file_definitions.len(),
        data.file_marks.len(),
        data.symbol_definitions.len()
    );

    Ok(data)
}
