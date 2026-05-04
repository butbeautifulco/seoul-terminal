use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;

use gpui::Hsla;
use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, TextProvider, Tree};
use tree_sitter_language::LanguageFn;

// Force the kotlin crate to be linked (its Rust API targets tree-sitter <0.23,
// but the C grammar ABI is stable so we access it directly via FFI).
extern crate tree_sitter_kotlin;

unsafe extern "C" {
    fn tree_sitter_kotlin() -> *const ();
}

fn kotlin_language() -> Language {
    let lang_fn = unsafe { LanguageFn::from_raw(tree_sitter_kotlin) };
    lang_fn.into()
}

use crate::theme::ThemeColors;

#[derive(Clone, Debug)]
pub struct HighlightSpan {
    pub byte_start: usize,
    pub byte_end: usize,
    pub color: Hsla,
}

/// tree-sitter TextProvider backed by ropey Rope chunks.
/// Avoids full-text allocation for query predicate evaluation.
struct RopeProvider<'a> {
    rope: &'a Rope,
}

impl<'a> TextProvider<&'a str> for RopeProvider<'a> {
    type I = ropey::iter::Chunks<'a>;

    fn text(&mut self, node: Node) -> Self::I {
        let start = node.start_byte();
        let end = node.end_byte().min(self.rope.len_bytes());
        self.rope.byte_slice(start..end).chunks()
    }
}

struct CachedHighlights {
    byte_range: Range<usize>,
    raw_spans: Vec<(usize, usize, Hsla)>,
}

pub struct SyntaxHighlighter {
    parser: Parser,
    tree: Option<Tree>,
    query: Option<Query>,
    highlight_map: Vec<Hsla>, // capture_index → color
    language_name: Option<String>,
    highlight_cache: Option<CachedHighlights>,
}

impl SyntaxHighlighter {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            tree: None,
            query: None,
            highlight_map: Vec::new(),
            language_name: None,
            highlight_cache: None,
        }
    }

    /// Detect language from file extension and configure parser + query.
    pub fn configure_for_file(&mut self, path: &Path) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some((language, query_source)) = load_language(ext) else {
            self.language_name = None;
            self.query = None;
            return;
        };

        if let Err(e) = self.parser.set_language(&language) {
            tracing::warn!("failed to set language for {ext}: {e}");
            self.language_name = None;
            self.query = None;
            return;
        }
        self.language_name = Some(ext.to_string());

        match Query::new(&language, query_source) {
            Ok(query) => {
                self.highlight_map = build_highlight_map(&query, &ThemeColors::catppuccin_mocha());
                self.query = Some(query);
            }
            Err(e) => {
                tracing::warn!("failed to compile highlight query for {ext}: {e}");
                self.query = None;
            }
        }
    }

    /// Apply an incremental edit to the existing tree (O(log n)).
    pub fn apply_edit(&mut self, edit: &tree_sitter::InputEdit) {
        if let Some(tree) = &mut self.tree {
            tree.edit(edit);
        }
        self.highlight_cache = None;
    }

    /// Parse text (incrementally if old tree exists). Used for initial load.
    pub fn parse(&mut self, text: &str) {
        let old_tree = self.tree.as_ref();
        self.tree = self.parser.parse(text, old_tree);
        self.highlight_cache = None;
    }

    /// Incremental reparse reading directly from rope chunks — no String allocation.
    pub fn reparse_with_rope(&mut self, rope: &Rope) {
        let old_tree = self.tree.as_ref();
        self.tree = self.parser.parse_with_options(
            &mut |byte_offset, _point| -> &[u8] {
                if byte_offset >= rope.len_bytes() {
                    return &[];
                }
                let (chunk, start, _, _) = rope.chunk_at_byte(byte_offset);
                &chunk.as_bytes()[byte_offset - start..]
            },
            old_tree,
            None,
        );
        self.highlight_cache = None;
    }

    /// Highlight a byte range, returning spans split by line.
    /// Reads text from the Rope via RopeProvider — no full-text allocation.
    /// Caches raw spans; cache is invalidated on edit.
    pub fn highlight_lines(
        &mut self,
        rope: &Rope,
        visible_byte_range: Range<usize>,
        line_byte_offsets: &[(usize, usize)],
    ) -> Vec<Vec<HighlightSpan>> {
        let mut result: Vec<Vec<HighlightSpan>> =
            line_byte_offsets.iter().map(|_| Vec::new()).collect();

        if self.tree.is_none() || self.query.is_none() {
            return result;
        }

        // Populate cache if needed
        let cache_valid = self
            .highlight_cache
            .as_ref()
            .is_some_and(|c| c.byte_range == visible_byte_range);

        if !cache_valid {
            let raw_spans = {
                let mut cursor = QueryCursor::new();
                cursor.set_byte_range(visible_byte_range.clone());

                let tree = self.tree.as_ref().unwrap();
                let query = self.query.as_ref().unwrap();
                let provider = RopeProvider { rope };
                let mut matches = cursor.matches(query, tree.root_node(), provider);

                let mut spans: Vec<(usize, usize, Hsla)> = Vec::new();
                while let Some(m) = matches.next() {
                    for capture in m.captures {
                        let idx = capture.index as usize;
                        if idx < self.highlight_map.len() {
                            let color = self.highlight_map[idx];
                            spans.push((capture.node.start_byte(), capture.node.end_byte(), color));
                        }
                    }
                }
                spans.sort_by_key(|s| (s.0, std::cmp::Reverse(s.1)));
                spans
            };
            self.highlight_cache = Some(CachedHighlights {
                byte_range: visible_byte_range,
                raw_spans,
            });
        }

        let raw_spans = &self.highlight_cache.as_ref().unwrap().raw_spans;

        // Distribute spans into per-line buckets
        for (line_idx, &(line_start, line_end)) in line_byte_offsets.iter().enumerate() {
            for &(span_start, span_end, color) in raw_spans {
                if span_end <= line_start || span_start >= line_end {
                    continue;
                }
                let clipped_start = span_start.max(line_start);
                let clipped_end = span_end.min(line_end);
                if clipped_start < clipped_end {
                    result[line_idx].push(HighlightSpan {
                        byte_start: clipped_start - line_start,
                        byte_end: clipped_end - line_start,
                        color,
                    });
                }
            }
        }

        result
    }

    #[allow(dead_code)]
    pub fn has_language(&self) -> bool {
        self.language_name.is_some() && self.query.is_some()
    }
}

// -- Language registry --
//
// To add a new language:
// 1. Add the tree-sitter-* crate to workspace Cargo.toml and crates/seoul-terminal/Cargo.toml
// 2. Add a `register(...)` call in `build_registry()` below

fn load_language(ext: &str) -> Option<(Language, &'static str)> {
    static REGISTRY: LazyLock<HashMap<&'static str, (Language, &'static str)>> =
        LazyLock::new(build_registry);
    REGISTRY.get(ext).cloned()
}

fn build_registry() -> HashMap<&'static str, (Language, &'static str)> {
    let mut m = HashMap::new();
    let mut r = |exts: &[&'static str], lang: Language, q: &'static str| {
        for e in exts {
            m.insert(*e, (lang.clone(), q));
        }
    };

    // ─── Tier 0: Already popular ────────────────────────
    r(
        &["rs"],
        tree_sitter_rust::LANGUAGE.into(),
        tree_sitter_rust::HIGHLIGHTS_QUERY,
    );
    r(
        &["js", "jsx", "mjs", "cjs"],
        tree_sitter_javascript::LANGUAGE.into(),
        tree_sitter_javascript::HIGHLIGHT_QUERY,
    );
    r(
        &["ts"],
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
    );
    r(
        &["tsx"],
        tree_sitter_typescript::LANGUAGE_TSX.into(),
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
    );
    r(
        &["py", "pyi"],
        tree_sitter_python::LANGUAGE.into(),
        tree_sitter_python::HIGHLIGHTS_QUERY,
    );
    r(
        &["json"],
        tree_sitter_json::LANGUAGE.into(),
        tree_sitter_json::HIGHLIGHTS_QUERY,
    );
    r(
        &["toml"],
        tree_sitter_toml_ng::LANGUAGE.into(),
        tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
    );

    // ─── Tier 1: Systems / compiled ─────────────────────
    r(
        &["c", "h"],
        tree_sitter_c::LANGUAGE.into(),
        tree_sitter_c::HIGHLIGHT_QUERY,
    );
    r(
        &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
        tree_sitter_cpp::LANGUAGE.into(),
        tree_sitter_cpp::HIGHLIGHT_QUERY,
    );
    r(
        &["go"],
        tree_sitter_go::LANGUAGE.into(),
        tree_sitter_go::HIGHLIGHTS_QUERY,
    );
    r(
        &["java"],
        tree_sitter_java::LANGUAGE.into(),
        tree_sitter_java::HIGHLIGHTS_QUERY,
    );
    r(
        &["cs"],
        tree_sitter_c_sharp::LANGUAGE.into(),
        include_str!("../queries/c-sharp-highlights.scm"),
    );
    r(
        &["swift"],
        tree_sitter_swift::LANGUAGE.into(),
        tree_sitter_swift::HIGHLIGHTS_QUERY,
    );
    r(
        &["kt", "kts"],
        kotlin_language(),
        include_str!("../queries/kotlin-highlights.scm"),
    );
    r(
        &["scala", "sc"],
        tree_sitter_scala::LANGUAGE.into(),
        tree_sitter_scala::HIGHLIGHTS_QUERY,
    );
    r(
        &["zig"],
        tree_sitter_zig::LANGUAGE.into(),
        tree_sitter_zig::HIGHLIGHTS_QUERY,
    );
    r(
        &["dart"],
        tree_sitter_dart::LANGUAGE.into(),
        tree_sitter_dart::HIGHLIGHTS_QUERY,
    );

    // ─── Tier 2: Dynamic / scripting ────────────────────
    r(
        &["rb", "erb"],
        tree_sitter_ruby::LANGUAGE.into(),
        tree_sitter_ruby::HIGHLIGHTS_QUERY,
    );
    r(
        &["php", "phtml"],
        tree_sitter_php::LANGUAGE_PHP.into(),
        tree_sitter_php::HIGHLIGHTS_QUERY,
    );
    r(
        &["lua"],
        tree_sitter_lua::LANGUAGE.into(),
        tree_sitter_lua::HIGHLIGHTS_QUERY,
    );
    r(
        &["ex", "exs"],
        tree_sitter_elixir::LANGUAGE.into(),
        tree_sitter_elixir::HIGHLIGHTS_QUERY,
    );
    r(
        &["hs"],
        tree_sitter_haskell::LANGUAGE.into(),
        tree_sitter_haskell::HIGHLIGHTS_QUERY,
    );
    r(
        &["r", "R"],
        tree_sitter_r::LANGUAGE.into(),
        tree_sitter_r::HIGHLIGHTS_QUERY,
    );
    r(
        &["ml"],
        tree_sitter_ocaml::LANGUAGE_OCAML.into(),
        tree_sitter_ocaml::HIGHLIGHTS_QUERY,
    );
    r(
        &["mli"],
        tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(),
        tree_sitter_ocaml::HIGHLIGHTS_QUERY,
    );

    // ─── Tier 3: Web / config / markup ──────────────────
    r(
        &["html", "htm"],
        tree_sitter_html::LANGUAGE.into(),
        tree_sitter_html::HIGHLIGHTS_QUERY,
    );
    r(
        &["css"],
        tree_sitter_css::LANGUAGE.into(),
        tree_sitter_css::HIGHLIGHTS_QUERY,
    );
    r(
        &["sh", "bash", "zsh"],
        tree_sitter_bash::LANGUAGE.into(),
        tree_sitter_bash::HIGHLIGHT_QUERY,
    );
    r(
        &["yaml", "yml"],
        tree_sitter_yaml::LANGUAGE.into(),
        tree_sitter_yaml::HIGHLIGHTS_QUERY,
    );
    r(
        &["md", "markdown"],
        tree_sitter_md::LANGUAGE.into(),
        tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
    );

    m
}

// -- Catppuccin Mocha color mapping --

fn highlight_color(name: &str, t: &ThemeColors) -> Hsla {
    use crate::theme::opaque;

    // Prefix matching: "keyword.function" should match "keyword"
    let base_name = name.split('.').next().unwrap_or(name);
    let hex = match base_name {
        "keyword" | "conditional" | "repeat" | "include" | "exception" => opaque(t.mauve),
        "string" => opaque(t.green),
        "comment" => opaque(t.overlay0),
        "function" | "method" => opaque(t.blue),
        "type" => opaque(t.yellow),
        "variable" => opaque(t.text),
        "number" | "float" => opaque(t.peach),
        "operator" => opaque(t.sky),
        "punctuation" => opaque(t.overlay2),
        "property" | "field" => opaque(t.lavender),
        "constant" => opaque(t.peach),
        "attribute" | "label" => opaque(t.yellow),
        "constructor" | "tag" => opaque(t.sapphire),
        "namespace" | "module" => opaque(t.red),
        "boolean" => opaque(t.peach),
        "character" | "escape" => opaque(t.teal),
        "parameter" => opaque(t.flamingo),
        "preproc" => opaque(t.red),
        "selector" => opaque(t.teal),
        "storageclass" | "define" | "macro" => opaque(t.mauve),
        "title" => opaque(t.blue),
        "uri" | "link" => opaque(t.teal),
        "text" | "none" => opaque(t.text),
        _ => opaque(t.text),
    };
    Hsla::from(gpui::rgba(hex))
}

/// Build a map from capture index → color.
fn build_highlight_map(query: &Query, theme: &ThemeColors) -> Vec<Hsla> {
    query
        .capture_names()
        .iter()
        .map(|name| highlight_color(name, theme))
        .collect()
}
