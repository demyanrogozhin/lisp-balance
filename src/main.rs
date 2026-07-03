use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process;

use clap::Parser;
use parinfer_rust::parinfer;
use parinfer_rust::types::*;
use similar::{ChangeTag, TextDiff};

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "lisp-balance",
    about = "Fix unbalanced parentheses in Lisp code using parinfer",
    version,
    after_help = "If FILE is omitted, reads from stdin and writes to stdout (pipe mode).
                   In check mode, exit code is 0 (balanced) or 1 (unbalanced)."
)]
struct Args {
    /// File to fix. Omit for stdin/stdout mode.
    #[arg(value_hint = clap::ValueHint::FilePath)]
    file: Option<PathBuf>,

    /// Fix the file in-place (modifies original).
    #[arg(short = 'i', long)]
    in_place: bool,

    /// Show unified diff of changes.
    #[arg(short = 'd', long)]
    diff: bool,

    /// Parinfer mode: indent, paren, or smart.
    #[arg(short = 'm', long, default_value = "smart")]
    mode: String,

    /// Only validate; exit 0 if balanced, 1 if not.
    #[arg(short = 'c', long)]
    check: bool,

    /// Suppress normal output; only print errors.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// JSON output for machine consumption (OpenCode protocol).
    #[arg(short = 'j', long)]
    json: bool,

    /// Language dialect: lisp, scheme, racket, clojure, etc.
    #[arg(long, default_value = "lisp")]
    lang: String,
}

// ---------------------------------------------------------------------------
// Language features
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct LanguageFeatures {
    comment_char: char,
    lisp_vline_symbols: bool,
    lisp_block_comments: bool,
    guile_block_comments: bool,
    scheme_sexp_comments: bool,
    janet_long_strings: bool,
    hy_bracket_strings: bool,
}

impl LanguageFeatures {
    fn for_language(lang: &str) -> Self {
        match lang.to_lowercase().as_str() {
            "clojure" => Self::clojure(),
            "common-lisp" | "lisp" => Self::common_lisp(),
            "scheme" => Self::scheme(),
            "racket" => Self::racket(),
            "hy" => Self::hy(),
            "janet" => Self::janet(),
            "picolisp" => Self::picolisp(),
            _ => Self::common_lisp(), // default: CL features
        }
    }

    fn clojure() -> Self {
        Self::default()
    }

    fn common_lisp() -> Self {
        Self {
            lisp_vline_symbols: true,
            lisp_block_comments: true,
            ..Self::default()
        }
    }

    fn scheme() -> Self {
        Self {
            lisp_vline_symbols: true,
            lisp_block_comments: true,
            scheme_sexp_comments: true,
            ..Self::default()
        }
    }

    fn racket() -> Self {
        Self {
            lisp_vline_symbols: true,
            lisp_block_comments: true,
            scheme_sexp_comments: true,
            ..Self::default()
        }
    }

    fn hy() -> Self {
        Self {
            hy_bracket_strings: true,
            ..Self::default()
        }
    }

    fn janet() -> Self {
        Self {
            comment_char: '#',
            janet_long_strings: true,
            ..Self::default()
        }
    }

    fn picolisp() -> Self {
        Self {
            comment_char: '#',
            lisp_vline_symbols: true,
            lisp_block_comments: true,
            ..Self::default()
        }
    }
}

impl Default for LanguageFeatures {
    fn default() -> Self {
        Self {
            comment_char: ';',
            lisp_vline_symbols: false,
            lisp_block_comments: false,
            guile_block_comments: false,
            scheme_sexp_comments: false,
            janet_long_strings: false,
            hy_bracket_strings: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct JsonOutput {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    changed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fixed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    mode: String,
    language: String,
}

#[derive(serde::Serialize)]
struct JsonError {
    name: String,
    message: String,
    line: usize,
    column: usize,
}

// ---------------------------------------------------------------------------
// Comment stripping
// ---------------------------------------------------------------------------
//
// parinfer only needs the code to balance parens — comment text is irrelevant
// to structure, and characters inside `;;` comments (a stray `|`, unbalanced
// `"`, etc.) can trip parinfer's quote-danger check. So we simply *ignore*
// comment content: strip it before parinfer, then re-attach the original
// comments line-by-line afterwards. parinfer preserves line count, so each
// output line maps back to its input line.

/// Strip the comment portion of every line.
///
/// Returns `(code_only, comments)` where `comments[i]` is the comment text
/// (including the leading `;`) that was on line `i`, or `None` if line `i` had
/// no comment. A `;` inside a string literal is not treated as a comment start.
fn strip_comments(text: &str, comment_char: char) -> (String, Vec<Option<String>>) {
    let cc = comment_char as u8;

    let mut code_lines: Vec<String> = Vec::new();
    let mut comments: Vec<Option<String>> = Vec::new();

    for line in text.split('\n') {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut in_string = false;
        let mut cstart = len;
        let mut i = 0;

        while i < len {
            let b = bytes[i];
            if in_string {
                if b == b'\\' {
                    i += 2; // escaped char in string
                    continue;
                }
                if b == b'"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            if b == b'"' {
                in_string = true;
                i += 1;
                continue;
            }
            // CL character literal `#\x`: consume `#\` plus the following char
            // so a `;`, `"`, or paren inside the literal (e.g. `#\;`, `#\(`)
            // is not mistaken for a comment / string / structure.
            if b == b'#' && i + 1 < len && bytes[i + 1] == b'\\' {
                i += 3; // `#`, `\`, and the literal's char
                // Consume a trailing alphabetic char-name (e.g. `#\Space`).
                while i < len && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                continue;
            }
            if b == cc {
                cstart = i;
                break;
            }
            i += 1;
        }

        let code_part = &line[..cstart];
        let comment_part = &line[cstart..];

        if cstart < line.len() && code_part.trim().is_empty() {
            // A whole-line comment (only whitespace before `;`): hide the line
            // entirely so parinfer never sees it, and restore the original line.
            code_lines.push(String::new());
            comments.push(Some(line.to_string()));
        } else {
            code_lines.push(code_part.to_string());
            comments.push(if cstart < line.len() {
                Some(comment_part.to_string())
            } else {
                None
            });
        }
    }

    (code_lines.join("\n"), comments)
}

/// Re-attach the comments removed by `strip_comments` to each output line.
fn reattach_comments(code: &str, comments: &[Option<String>]) -> String {
    let lines: Vec<&str> = code.split('\n').collect();
    let mut out = String::with_capacity(code.len());

    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        if let Some(comment) = comments.get(i).and_then(|c| c.as_ref()) {
            // If this slot holds a whole-line comment (stored with its own
            // indentation), `line` will be empty — just append it as-is.
            // Otherwise it's an inline comment: ensure a separating space.
            if !line.is_empty() && !line.ends_with(char::is_whitespace) {
                out.push(' ');
            }
            out.push_str(comment);
        }
        if i + 1 < lines.len() {
            out.push('\n');
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Preprocess / postprocess pipeline
// ---------------------------------------------------------------------------

/// Strip comments before parinfer. Returns the code-only text plus the
/// per-line comments captured for restoration.
fn preprocess(text: &str, features: &LanguageFeatures) -> (String, Vec<Option<String>>) {
    strip_comments(text, features.comment_char)
}

/// Re-attach comments to parinfer's output.
fn postprocess(parinfer_output: &str, comments: &[Option<String>]) -> String {
    reattach_comments(parinfer_output, comments)
}
// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

fn run_parinfer(text: &str, mode: &str, lang: &str) -> (bool, Option<Error>, String) {
    let features = LanguageFeatures::for_language(lang);

    // Preprocess: strip `;;` comments so parinfer only sees code.
    let (processed_text, comments) = preprocess(text, &features);

    let options = Options {
        cursor_x: None,
        cursor_line: None,
        prev_cursor_x: None,
        prev_cursor_line: None,
        prev_text: None,
        selection_start_line: None,
        changes: vec![],
        comment_char: features.comment_char,
        string_delimiters: vec!["\"".to_string()],
        lisp_vline_symbols: features.lisp_vline_symbols,
        lisp_block_comments: features.lisp_block_comments,
        guile_block_comments: features.guile_block_comments,
        scheme_sexp_comments: features.scheme_sexp_comments,
        janet_long_strings: features.janet_long_strings,
        hy_bracket_strings: features.hy_bracket_strings,
    };

    let request = Request {
        mode: mode.to_string(),
        text: processed_text,
        options,
    };

    let answer = parinfer::process(&request);

    // Extract the fields we need as owned values: `answer` borrows from the
    // local `request` (via reference-bearing fields), so it cannot be returned.
    let success = answer.success;
    let error = answer.error;
    let raw_output = answer.text.to_string();

    // Postprocess: re-attach the stripped comments to the fixed code.
    let restored = postprocess(&raw_output, &comments);

    (success, error, restored)
}

fn make_diff(original: &str, fixed: &str, filename: &str) -> String {
    let diff = TextDiff::from_lines(original, fixed);
    let mut out = String::new();

    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        out.push_str(sign);
        out.push_str(change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }

    // Wrap in unified diff header
    let header = format!(
        "--- a/{filename}\n+++ b/{filename}\n@@ -1,{} +1,{} @@\n",
        original.lines().count(),
        fixed.lines().count()
    );
    header + &out
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args = Args::parse();

    // --resolve input -------------------------------------------------------
    let (source_text, filename) = if let Some(path) = &args.file {
        match fs::read_to_string(path) {
            Ok(text) => (text, path.to_string_lossy().to_string()),
            Err(e) => {
                eprintln!("lisp-balance: error reading '{}': {}", path.display(), e);
                process::exit(1);
            }
        }
    } else {
        let mut buf = String::new();
        if io::stdin().read_to_string(&mut buf).is_err() {
            // Not a terminal? Just exit gracefully
            if !args.quiet {
                eprintln!("lisp-balance: no input (use --help for usage)");
            }
            process::exit(1);
        }
        (buf, "<stdin>".to_string())
    };

    let trimmed = source_text.trim();
    if trimmed.is_empty() {
        if !args.quiet {
            eprintln!("lisp-balance: empty input");
        }
        process::exit(0);
    }

    // --run parinfer --------------------------------------------------------
    let (success, error, fixed_text) =
        run_parinfer(&source_text, &args.mode, &args.lang);

    if !success {
        // Parinfer itself failed
        if args.json {
            let err = error.map(|e| JsonError {
                name: e.name.to_string(),
                message: e.message,
                line: e.line_no,
                column: e.x,
            });
            let out = JsonOutput {
                success: false,
                changed: None,
                error: err,
                original: Some(source_text),
                fixed: None,
                diff: None,
                mode: args.mode.clone(),
                language: args.lang.clone(),
            };
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
        } else {
            if !args.quiet {
                if let Some(e) = error {
                    eprintln!(
                        "lisp-balance: {} at line {}, column {}",
                        e.message, e.line_no, e.x
                    );
                } else {
                    eprintln!("lisp-balance: unknown error");
                }
            }
        }
        process::exit(1);
    }

    // --check mode ----------------------------------------------------------
    if args.check {
        let changed = fixed_text != source_text;
        if args.json {
            let out = JsonOutput {
                success: true,
                changed: Some(changed),
                error: None,
                original: Some(source_text),
                fixed: Some(fixed_text),
                diff: None,
                mode: args.mode.clone(),
                language: args.lang.clone(),
            };
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
        } else {
            if changed {
                if !args.quiet {
                    eprintln!("lisp-balance: unbalanced parens detected");
                }
                process::exit(1);
            }
            // balanced
            process::exit(0);
        }
        return;
    }

    let changed = fixed_text != source_text;

    // --in-place mode -------------------------------------------------------
    if args.in_place {
        if let Some(path) = &args.file {
            if changed {
                if args.diff {
                    // Show diff but also write
                    let diff = make_diff(&source_text, &fixed_text, &filename);
                    if args.json {
                        let out = JsonOutput {
                            success: true,
                            changed: Some(true),
                            error: None,
                            original: Some(source_text),
                            fixed: Some(fixed_text.clone()),
                            diff: Some(diff),
                            mode: args.mode.clone(),
                            language: args.lang.clone(),
                        };
                        println!("{}", serde_json::to_string_pretty(&out).unwrap());
                    } else if !args.quiet {
                        print!("{diff}");
                    }
                } else if args.json {
                    let out = JsonOutput {
                        success: true,
                        changed: Some(true),
                        error: None,
                        original: Some(source_text),
                        fixed: Some(fixed_text.clone()),
                        diff: None,
                        mode: args.mode.clone(),
                        language: args.lang.clone(),
                    };
                    println!("{}", serde_json::to_string_pretty(&out).unwrap());
                } else if !args.quiet {
                    eprintln!("lisp-balance: fixed '{}'", path.display());
                }

                match fs::write(path, &fixed_text) {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!(
                            "lisp-balance: error writing '{}': {}",
                            path.display(),
                            e
                        );
                        process::exit(1);
                    }
                }
            } else {
                // No change needed
                if args.json {
                    let out = JsonOutput {
                        success: true,
                        changed: Some(false),
                        error: None,
                        original: None,
                        fixed: None,
                        diff: None,
                        mode: args.mode.clone(),
                        language: args.lang.clone(),
                    };
                    println!("{}", serde_json::to_string_pretty(&out).unwrap());
                } else if !args.quiet {
                    eprintln!("lisp-balance: '{}' is already balanced", path.display());
                }
            }
        } else {
            eprintln!("lisp-balance: --in-place requires a file argument");
            process::exit(1);
        }
        return;
    }

    // --pipe mode (stdin -> stdout) -----------------------------------------
    if changed {
        if args.diff {
            let diff = make_diff(&source_text, &fixed_text, &filename);
            // Diff goes to stderr, fixed text to stdout
            if !args.quiet {
                eprintln!("{diff}");
            }
        }
        // Always output fixed text to stdout in pipe mode
        print!("{fixed_text}");
    } else {
        // No change, pass through
        print!("{source_text}");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- core: run_parinfer balances parens --------------------------------

    #[test]
    fn balances_missing_closing_parens() {
        let (success, error, fixed) = run_parinfer("(let ((x 1)\n", "smart", "lisp");
        assert!(success);
        assert!(error.is_none());
        assert_eq!(fixed, "(let ((x 1)))\n");
    }

    #[test]
    fn balanced_input_passes_through() {
        let original = "(defn foo [x]\n  (+ x 1))\n";
        let (success, _, fixed) = run_parinfer(original, "smart", "clojure");
        assert!(success);
        assert_eq!(fixed, original);
    }

    #[test]
    fn already_balanced_no_change() {
        let original = "(+ 1 (* 2 3))";
        let (success, _, fixed) = run_parinfer(original, "smart", "lisp");
        assert!(success);
        assert_eq!(fixed, original);
    }

    // --- comment stripping --------------------------------------------------

    #[test]
    fn strip_comments_removes_line_comments() {
        let src = "(foo) ;; a | b\n(bar)\n";
        let (code, comments) = strip_comments(src, ';');
        assert_eq!(code, "(foo) \n(bar)\n");
        assert_eq!(comments[0].as_deref(), Some(";; a | b"));
        assert_eq!(comments[1], None);
    }

    #[test]
    fn strip_comments_ignores_semicolon_in_string() {
        let src = "(princ \"a;b|c\") ;; real ; comment\n";
        let (code, comments) = strip_comments(src, ';');
        assert_eq!(code, "(princ \"a;b|c\") \n");
        assert_eq!(comments[0].as_deref(), Some(";; real ; comment"));
    }

    #[test]
    fn strip_comments_respects_cl_char_literal() {
        // `#\;` is the CL character literal for `;` — not a comment start.
        let src = "(char= (char rest 0) #\\;) ;; trailing comment\n";
        let (code, comments) = strip_comments(src, ';');
        assert_eq!(code, "(char= (char rest 0) #\\;) \n");
        assert_eq!(comments[0].as_deref(), Some(";; trailing comment"));
    }

    #[test]
    fn char_literal_with_semicolon_fixes_cleanly() {
        // The exact case that surfaced the bug: an unbalanced fragment whose
        // `#\;` made the old stripper leave a hanging backslash.
        let src = " ((and (>= (length rest) 1) (char= (char rest 0) #\\;))\n";
        let (success, error, fixed) = run_parinfer(src, "smart", "lisp");
        assert!(success, "error: {:?}", error);
        // Output must have balanced parens (parinfer adds the missing `)`).
        assert_eq!(
            fixed.matches('(').count(),
            fixed.matches(')').count(),
            "unbalanced: {fixed}"
        );
        // The character literal is preserved.
        assert!(fixed.contains("#\\;"));
    }

    #[test]
    fn strip_comments_whole_line_comment_preserves_indent() {
        let src = "  ;; module show|find\n(foo)\n";
        let (code, comments) = strip_comments(src, ';');
        // Whole-line comment → hidden as empty line, original kept verbatim.
        assert_eq!(code, "\n(foo)\n");
        assert_eq!(comments[0].as_deref(), Some("  ;; module show|find"));
    }

    #[test]
    fn comments_round_trip_through_strip_and_reattach() {
        let cases = [
            "(foo) ;; a | b\n",
            ";; header line\n(foo)\n",
            "  ;; indented whole-line\n",
            "(princ \"a;b\") ;; trailing\n",
            "(+ 1 2)\n",
        ];
        for src in cases {
            let (code, comments) = strip_comments(src, ';');
            let back = reattach_comments(&code, &comments);
            assert_eq!(back, src, "round-trip failed for {src:?}");
        }
    }

    // --- end-to-end via run_parinfer ----------------------------------------

    #[test]
    fn comment_pipe_no_longer_breaks_lisp() {
        // Regression: a stray `|` inside a `;;` comment used to trigger
        // parinfer's quote-danger when lisp_vline_symbols is on.
        let src = "(load \"lint.lisp\")\n;; module show|find query\n";
        let (success, error, fixed) = run_parinfer(src, "smart", "lisp");
        assert!(success);
        assert!(error.is_none());
        // Comment text is preserved verbatim.
        assert!(fixed.contains("show|find"));
    }

    #[test]
    fn clojure_unbalanced_quote_in_comment_ok() {
        let src = ";; he said \"hi\n(+ 1 2)\n";
        let (success, _, _) = run_parinfer(src, "smart", "clojure");
        assert!(success);
    }

    #[test]
    fn single_quote_in_code_is_preserved() {
        let src = "(list '(a b c))\n";
        let (success, _, fixed) = run_parinfer(src, "smart", "lisp");
        assert!(success);
        assert_eq!(fixed, src);
    }

    #[test]
    fn agent_scenario_fixes_unbalanced_with_comment_noise() {
        // What the agent harness actually does: the LLM wrote Lisp that is
        // missing closing parens AND dropped stray `|` / `"` into comments.
        // lisp-balance must still emit balanced parens, ignoring the comments.
        let src = "(defun foo ()\n  ;; note: a|b and \"oops\n  (let ((x 1)\n    (bar x)\n";
        let (success, error, fixed) = run_parinfer(src, "smart", "lisp");
        assert!(success, "error: {:?}", error);
        // Parens balanced in the output.
        assert_eq!(
            fixed.matches('(').count(),
            fixed.matches(')').count(),
            "unbalanced: {fixed}"
        );
        // Comment text preserved verbatim.
        assert!(fixed.contains("a|b and \"oops"));
    }

    // --- diff ---------------------------------------------------------------

    #[test]
    fn diff_marks_changes() {
        let d = make_diff("(let ((x 1)\n", "(let ((x 1)))\n", "test.lisp");
        assert!(d.contains("-(let ((x 1)"));
        assert!(d.contains("+(let ((x 1)))"));
        assert!(d.contains("--- a/test.lisp"));
    }
}
