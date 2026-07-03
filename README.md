# Lisp Balance

Rust CLI that fixes unbalanced parentheses in Lisp code using [parinfer-rust](https://github.com/eraserhd/parinfer-rust). Supports file I/O, diffs, validation, and JSON output for editor/agent integration. Created for LLM use in mind.

## Install

```sh
cargo install --path .
```

Requires Rust 1.85+ (edition 2024).

## Usage

```
lisp-balance [OPTIONS] [FILE]
```

| Flag | Description |
|------|-------------|
| `-i, --in-place` | Fix file in-place |
| `-d, --diff` | Show unified diff of changes |
| `-m, --mode <MODE>` | `indent` \| `paren` \| `smart` (default: `smart`) |
| `-c, --check` | Validate only; exit 0 balanced / 1 unbalanced |
| `-q, --quiet` | Only print errors |
| `-j, --json` | JSON output (OpenCode protocol) |
| `--lang <LANG>` | `lisp` \| `scheme` \| `racket` \| `clojure` \| `hy` \| `janet` \| `picolisp` (default: `lisp`) |

`FILE` omitted → stdin/stdout pipe mode.

### Examples

```sh
lisp-balance -d broken.lisp            # show diff
lisp-balance -i broken.lisp            # fix in place
echo '(let ((x 1)' | lisp-balance      # pipe mode → (let ((x 1))
lisp-balance -c broken.lisp && echo ok || echo unbalanced
```

## Language dialects

`--lang` activates parinfer features per dialect: Lisp `#|` comments, `#|...|#` vline symbols, `#{...}#` pairs, `#_` ignore; Scheme/Racket add `#;` sexp comments; Clojure uses parinfer defaults; Hy `#[...]` bracket strings; Janet long strings; Picolisp `#` comments.

## JSON output

```sh
lisp-balance --json broken.lisp
```

```json
{
  "success": true,
  "changed": true,
  "original": "(let ((x 1)",
  "fixed": "(let ((x 1))",
  "diff": "--- a/broken.lisp\n+++ b/broken.lisp\n@@ -1 +1 @@\n-(let ((x 1)\n+(let ((x 1))",
  "mode": "smart",
  "language": "lisp"
}
```

On failure, `success` is `false` with an `error` object (`name`, `message`, `line`, `column`).

