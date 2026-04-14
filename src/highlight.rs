/// Lightweight syntax highlighting for code blocks
///
/// Zero-dependency keyword-based highlighter that produces styled ratatui Lines.
/// Covers common languages (Rust, Python, JS/TS, Go, Bash, etc.) with heuristic
/// fallback for unknown languages.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Token types recognized by the highlighter
#[derive(Debug, Clone, Copy, PartialEq)]
enum TokenKind {
    Keyword,
    String,
    Comment,
    Number,
    Type,
    Macro,
    Operator,
    Plain,
}

/// Language-specific keyword sets
struct LangProfile {
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    comment_prefix: &'static str,
    block_comment: Option<(&'static str, &'static str)>,
    has_macros: bool,
}

const RUST: LangProfile = LangProfile {
    keywords: &[
        "fn","let","mut","const","static","pub","use","mod","crate","self","super",
        "struct","enum","impl","trait","type","where","for","in","loop","while",
        "if","else","match","return","break","continue","async","await","move",
        "unsafe","extern","dyn","ref","as","true","false",
    ],
    types: &[
        "Self","String","Vec","Option","Result","Box","Rc","Arc","HashMap","HashSet",
        "u8","u16","u32","u64","u128","usize","i8","i16","i32","i64","i128","isize",
        "f32","f64","bool","char","str",
    ],
    comment_prefix: "//",
    block_comment: Some(("/*", "*/")),
    has_macros: true,
};

const PYTHON: LangProfile = LangProfile {
    keywords: &[
        "def","class","if","elif","else","for","while","return","import","from",
        "as","with","try","except","finally","raise","yield","lambda","pass",
        "break","continue","and","or","not","in","is","None","True","False",
        "async","await","global","nonlocal","assert","del",
    ],
    types: &[
        "int","float","str","bool","list","dict","tuple","set","bytes","object",
        "type","range","frozenset","complex","memoryview","bytearray",
    ],
    comment_prefix: "#",
    block_comment: None,
    has_macros: false,
};

const JAVASCRIPT: LangProfile = LangProfile {
    keywords: &[
        "function","const","let","var","if","else","for","while","do","return",
        "class","extends","new","this","super","import","export","from","default",
        "try","catch","finally","throw","async","await","yield","switch","case",
        "break","continue","typeof","instanceof","in","of","true","false","null",
        "undefined","void","delete",
    ],
    types: &[
        "Array","Object","String","Number","Boolean","Map","Set","Promise",
        "Symbol","BigInt","Date","RegExp","Error","JSON","Math","console",
    ],
    comment_prefix: "//",
    block_comment: Some(("/*", "*/")),
    has_macros: false,
};

const GO: LangProfile = LangProfile {
    keywords: &[
        "func","var","const","type","struct","interface","map","chan","range",
        "if","else","for","switch","case","default","select","return","break",
        "continue","go","defer","fallthrough","goto","package","import",
        "true","false","nil",
    ],
    types: &[
        "int","int8","int16","int32","int64","uint","uint8","uint16","uint32",
        "uint64","float32","float64","complex64","complex128","string","bool",
        "byte","rune","error","any",
    ],
    comment_prefix: "//",
    block_comment: Some(("/*", "*/")),
    has_macros: false,
};

const BASH: LangProfile = LangProfile {
    keywords: &[
        "if","then","else","elif","fi","for","while","do","done","case","esac",
        "in","function","return","local","export","source","echo","exit",
        "read","shift","eval","exec","set","unset","true","false",
    ],
    types: &[],
    comment_prefix: "#",
    block_comment: None,
    has_macros: false,
};

const TOML_YAML: LangProfile = LangProfile {
    keywords: &["true","false","null","yes","no","on","off"],
    types: &[],
    comment_prefix: "#",
    block_comment: None,
    has_macros: false,
};

/// Color palette for syntax highlighting
struct Palette {
    keyword: Color,
    string: Color,
    comment: Color,
    number: Color,
    r#type: Color,
    r#macro: Color,
    operator: Color,
    plain: Color,
}

impl Palette {
    fn dark() -> Self {
        Self {
            keyword:  Color::Rgb(198, 120, 221), // purple
            string:   Color::Rgb(152, 195, 121), // green
            comment:  Color::Rgb(92, 99, 112),   // gray
            number:   Color::Rgb(209, 154, 102), // orange
            r#type:   Color::Rgb(86, 182, 194),  // cyan
            r#macro:  Color::Rgb(224, 108, 117), // red
            operator: Color::Rgb(171, 178, 191), // light gray
            plain:    Color::Rgb(220, 230, 240), // off-white
        }
    }

    fn light() -> Self {
        Self {
            keyword:  Color::Rgb(152, 28, 172),  // purple
            string:   Color::Rgb(47, 126, 44),   // green
            comment:  Color::Rgb(140, 148, 158),  // gray
            number:   Color::Rgb(193, 92, 0),    // orange
            r#type:   Color::Rgb(1, 120, 139),   // teal
            r#macro:  Color::Rgb(200, 40, 41),   // red
            operator: Color::Rgb(57, 58, 52),    // dark gray
            plain:    Color::Rgb(40, 42, 46),    // near-black
        }
    }

    fn color_for(&self, kind: TokenKind) -> Color {
        match kind {
            TokenKind::Keyword  => self.keyword,
            TokenKind::String   => self.string,
            TokenKind::Comment  => self.comment,
            TokenKind::Number   => self.number,
            TokenKind::Type     => self.r#type,
            TokenKind::Macro    => self.r#macro,
            TokenKind::Operator => self.operator,
            TokenKind::Plain    => self.plain,
        }
    }
}

pub struct Highlighter;

impl Highlighter {
    pub fn new() -> Self { Self }

    fn profile_for(language: &str) -> &'static LangProfile {
        match language.to_lowercase().as_str() {
            "rust" | "rs" => &RUST,
            "python" | "py" => &PYTHON,
            "javascript" | "js" | "typescript" | "ts" | "jsx" | "tsx" => &JAVASCRIPT,
            "go" | "golang" => &GO,
            "bash" | "sh" | "zsh" | "fish" | "shell" => &BASH,
            "toml" | "yaml" | "yml" | "json" | "ini" | "cfg" => &TOML_YAML,
            // C-family languages share JS-like structure
            "c" | "cpp" | "c++" | "cs" | "csharp" | "java" | "kotlin" | "swift" |
            "zig" | "scala" | "dart" => &JAVASCRIPT,
            _ => &RUST, // reasonable default for unknown langs
        }
    }

    /// Highlight a code block, returning styled Lines with `│   ` prefix
    pub fn highlight_code(&self, code: &str, language: &str, dark: bool) -> Vec<Line<'static>> {
        let profile = Self::profile_for(language);
        let palette = if dark { Palette::dark() } else { Palette::light() };
        let mut lines = Vec::new();

        for source_line in code.lines() {
            let tokens = tokenize(source_line, profile);
            let mut spans: Vec<Span<'static>> = vec![Span::styled(
                "│   ".to_string(),
                Style::default().fg(palette.comment),
            )];
            for (kind, text) in tokens {
                let fg = palette.color_for(kind);
                spans.push(Span::styled(text, Style::default().fg(fg)));
            }
            lines.push(Line::from(spans));
        }

        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "│".to_string(),
                Style::default().fg(palette.comment),
            )));
        }

        lines
    }
}

/// Tokenize a single line of source code into (TokenKind, text) pairs
fn tokenize(line: &str, profile: &LangProfile) -> Vec<(TokenKind, String)> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Line comments
        if !profile.comment_prefix.is_empty()
            && line[i..].starts_with(profile.comment_prefix)
        {
            tokens.push((TokenKind::Comment, line[i..].to_string()));
            return tokens;
        }

        // Block comment start
        if let Some((start, _end)) = profile.block_comment {
            if line[i..].starts_with(start) {
                tokens.push((TokenKind::Comment, line[i..].to_string()));
                return tokens;
            }
        }

        // Strings (double or single quote)
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            let start = i;
            i += 1;
            while i < len && chars[i] != quote {
                if chars[i] == '\\' { i += 1; } // skip escaped char
                i += 1;
            }
            if i < len { i += 1; } // closing quote
            tokens.push((TokenKind::String, chars[start..i].iter().collect()));
            continue;
        }

        // Numbers
        if chars[i].is_ascii_digit()
            || (chars[i] == '.' && i + 1 < len && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            // Hex prefix
            if chars[i] == '0' && i + 1 < len && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
                i += 2;
                while i < len && (chars[i].is_ascii_hexdigit() || chars[i] == '_') { i += 1; }
            } else {
                while i < len && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_' || chars[i] == 'e' || chars[i] == 'E') { i += 1; }
            }
            // Numeric suffix (u32, f64, etc.)
            while i < len && chars[i].is_ascii_alphanumeric() { i += 1; }
            tokens.push((TokenKind::Number, chars[start..i].iter().collect()));
            continue;
        }

        // Words (identifiers, keywords, types)
        if chars[i].is_ascii_alphanumeric() || chars[i] == '_' {
            let start = i;
            while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') { i += 1; }
            let word: String = chars[start..i].iter().collect();

            // Rust macros: word followed by !
            if profile.has_macros && i < len && chars[i] == '!' {
                let mut macro_word = word;
                macro_word.push('!');
                i += 1;
                tokens.push((TokenKind::Macro, macro_word));
                continue;
            }

            let kind = if profile.keywords.contains(&word.as_str()) {
                TokenKind::Keyword
            } else if profile.types.contains(&word.as_str()) {
                TokenKind::Type
            } else if word.chars().next().map_or(false, |c| c.is_uppercase()) {
                // PascalCase heuristic → treat as type
                TokenKind::Type
            } else {
                TokenKind::Plain
            };
            tokens.push((kind, word));
            continue;
        }

        // Operators and punctuation
        if "=<>!&|+-*/%^~?:;,.{}()[]@#".contains(chars[i]) {
            let start = i;
            // Grab runs of operator chars (e.g. ==, ->, =>)
            while i < len && "=<>!&|+-*/%^~?:".contains(chars[i]) { i += 1; }
            if i == start { i += 1; } // single punctuation char
            tokens.push((TokenKind::Operator, chars[start..i].iter().collect()));
            continue;
        }

        // Whitespace and other — pass through as plain
        let start = i;
        while i < len && chars[i].is_whitespace() { i += 1; }
        if i == start { i += 1; } // unknown char
        tokens.push((TokenKind::Plain, chars[start..i].iter().collect()));
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlighter_creation() {
        let _h = Highlighter::new();
    }

    #[test]
    fn test_highlight_rust_code() {
        let h = Highlighter::new();
        let code = "fn main() {\n    println!(\"hello\");\n}";
        let lines = h.highlight_code(code, "rust", true);
        assert_eq!(lines.len(), 3);
        // Each line should start with the border prefix
        for line in &lines {
            let first_span = &line.spans[0];
            assert!(first_span.content.starts_with('│'));
        }
    }

    #[test]
    fn test_highlight_unknown_language() {
        let h = Highlighter::new();
        let code = "some text here";
        let lines = h.highlight_code(code, "nonexistent_language_xyz", true);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_highlight_empty_code() {
        let h = Highlighter::new();
        let lines = h.highlight_code("", "rust", false);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_highlight_light_vs_dark() {
        let h = Highlighter::new();
        let code = "let x = 42;";
        let dark_lines = h.highlight_code(code, "rust", true);
        let light_lines = h.highlight_code(code, "rust", false);
        assert!(!dark_lines.is_empty());
        assert!(!light_lines.is_empty());
    }

    #[test]
    fn test_tokenize_rust_keywords() {
        let tokens = tokenize("fn main() {", &RUST);
        assert_eq!(tokens[0], (TokenKind::Keyword, "fn".to_string()));
    }

    #[test]
    fn test_tokenize_string() {
        let tokens = tokenize("let s = \"hello world\";", &RUST);
        let has_string = tokens.iter().any(|(k, _)| *k == TokenKind::String);
        assert!(has_string);
    }

    #[test]
    fn test_tokenize_comment() {
        let tokens = tokenize("// this is a comment", &RUST);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, TokenKind::Comment);
    }

    #[test]
    fn test_tokenize_number() {
        let tokens = tokenize("let x = 42;", &RUST);
        let has_number = tokens.iter().any(|(k, _)| *k == TokenKind::Number);
        assert!(has_number);
    }

    #[test]
    fn test_tokenize_macro() {
        let tokens = tokenize("println!(\"test\");", &RUST);
        let has_macro = tokens.iter().any(|(k, _)| *k == TokenKind::Macro);
        assert!(has_macro);
    }

    #[test]
    fn test_tokenize_python() {
        let tokens = tokenize("def hello():", &PYTHON);
        assert_eq!(tokens[0], (TokenKind::Keyword, "def".to_string()));
    }

    #[test]
    fn test_tokenize_type_heuristic() {
        let tokens = tokenize("let v: Vec<String> = Vec::new();", &RUST);
        let types: Vec<_> = tokens.iter().filter(|(k, _)| *k == TokenKind::Type).collect();
        assert!(types.len() >= 2); // Vec and String
    }

    #[test]
    fn test_tokenize_hex_number() {
        let tokens = tokenize("let addr = 0xFF00;", &RUST);
        let has_number = tokens.iter().any(|(k, t)| *k == TokenKind::Number && t.starts_with("0x"));
        assert!(has_number);
    }

    #[test]
    fn test_tokenize_escaped_string() {
        let tokens = tokenize(r#"let s = "hello \"world\"";"#, &RUST);
        let strings: Vec<_> = tokens.iter().filter(|(k, _)| *k == TokenKind::String).collect();
        assert_eq!(strings.len(), 1);
    }
}
