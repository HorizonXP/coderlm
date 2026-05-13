use super::{LanguageConfig, TestPattern};

pub const SYMBOLS_QUERY: &str = r#"
(call
  target: (identifier) @ignore
  (arguments (alias) @mod.name)
  (#any-of? @ignore "defmodule" "defprotocol" "defimpl")) @mod.def

(call
  target: (identifier) @ignore
  (arguments (alias) @import.name)
  (#any-of? @ignore "alias" "import" "require" "use")) @import.def

(call
  target: (identifier) @ignore
  (arguments
    [
      (identifier) @function.name
      (call target: (identifier) @function.name)
      (binary_operator
        left: (call target: (identifier) @function.name)
        operator: "when")
    ])
  (#any-of? @ignore "def" "defp" "defdelegate" "defguard" "defguardp" "defmacro" "defmacrop" "defn" "defnp")) @function.def
"#;

pub const CALLERS_QUERY: &str = r#"
(call
  target: (identifier) @callee)

(call
  target: (dot
    right: (identifier) @callee))

(binary_operator
  operator: "|>"
  right: (identifier) @callee)

(binary_operator
  operator: "|>"
  right: (call
    target: (dot
      right: (identifier) @callee)))
"#;

pub const VARIABLES_QUERY: &str = r#"
(call
  target: (identifier) @ignore
  (arguments
    (call
      target: (identifier)
      (arguments
        (identifier) @var.name)))
  (#any-of? @ignore "def" "defp" "defdelegate" "defguard" "defguardp" "defmacro" "defmacrop" "defn" "defnp"))

(call
  target: (identifier) @ignore
  (arguments
    (binary_operator
      left: (call
        target: (identifier)
        (arguments
          (identifier) @var.name))
      operator: "when"))
  (#any-of? @ignore "def" "defp" "defdelegate" "defguard" "defguardp" "defmacro" "defmacrop" "defn" "defnp"))

(binary_operator
  left: (identifier) @var.name
  "=")

(binary_operator
  left: (identifier) @var.name
  "<-")
"#;

pub fn config() -> LanguageConfig {
    LanguageConfig {
        language: tree_sitter_elixir::LANGUAGE.into(),
        symbols_query: SYMBOLS_QUERY,
        callers_query: CALLERS_QUERY,
        variables_query: VARIABLES_QUERY,
        test_patterns: vec![TestPattern::CallExpression("test")],
    }
}
