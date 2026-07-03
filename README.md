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

## OpenCode support

Add custom hook in your project `.opencode/plugins/write-hook.ts`:

```typescript
import type { Plugin } from "@opencode-ai/plugin";

export const WriteHookPlugin: Plugin = async ({ client }) => {
    const SERVICE = "lisp-balance-plugin";
    const log = (level: "debug" | "info" | "warn" | "error", message: string) =>
        client.app.log({ body: { service: SERVICE, level, message } });

    await log("info", "Plugin initialized");
    console.log("Write Hook START");

    return {
        // Runs BEFORE the built-in write tool executes
        "tool.execute.before": async (input, output) => {
            if (input.tool !== "write") return;
            const filePath = output.args.filePath as string;

            // Detect the Lisp dialect from the extension so parinfer uses the
            // right comment/string/symbol rules for each language.
            const ext = filePath.slice(filePath.lastIndexOf(".") + 1).toLowerCase();
            const langByExt: Record<string, string> = {
                lisp: "lisp", cl: "lisp", lsp: "lisp", l: "lisp",
                scm: "scheme", ss: "scheme",
                rkt: "racket", rktl: "racket",
                clj: "clojure", cljs: "clojure", cljc: "clojure",
                hy: "hy",
                janet: "janet",
            };

            const lang = langByExt[ext];
            if (!lang) return;

            const original = output.args.content as string;
            await log("debug", `Lisp file detected (${lang}): ${filePath}`);

            try {
                const proc = Bun.spawn(["lisp-balance", "--mode", "smart", "--lang", lang, "--json"], {
                    stdin: Buffer.from(original),
                    stdout: "pipe",
                    stderr: "pipe",
                });

                // Drain stdout + stderr concurrently with the exit wait, or a
                // large payload can fill the OS pipe buffer and deadlock.
                const [exitCode, stdoutText, stderrText] = await Promise.all([
                    proc.exited,
                    new Response(proc.stdout).text(),
                    new Response(proc.stderr).text(),
                ]);

                let json: any;
                try {
                    json = JSON.parse(stdoutText);
                } catch {
                    // Non-JSON output: a stale binary that ignores --json in
                    // pipe mode prints the fixed text on success.
                    if (exitCode === 0 && stdoutText.length > 0) {
                        output.args.content = stdoutText;
                        await log("warn", `Non-JSON stdout from lisp-balance; used raw output for ${filePath}`);
                    } else {
                        await log("error", `lisp-balance unparseable output (exit ${exitCode}): ${stderrText || stdoutText}`);
                    }
                    return;
                }

                if (!json.success) {
                    const e = json.error;
                    await log(
                        "warn",
                        `lisp-balance could not balance ${filePath}: ${e?.message ?? "unknown error"} (line ${e?.line}, col ${e?.column})`,
                    );
                    // Leave the original content untouched rather than blocking the write.
                    return;
                }

                // `fixed` is always present on success per the JSON contract;
                // guard anyway so we never write `undefined`.
                output.args.content =
                    typeof json.fixed === "string" ? json.fixed : original;
                await log("debug", `lisp-balance ${json.changed ? "reformatted" : "no change"}: ${filePath}`);
            } catch (err) {
                await log("error", `lisp-balance invocation failed for ${filePath}: ${String(err)}`);
                // Best-effort: fall through with the original content.
            }
        },
        // Runs AFTER the built-in write tool executes
        "tool.execute.after": async (_input) => {
            // Intentionally empty.
        },
    };
};
```
