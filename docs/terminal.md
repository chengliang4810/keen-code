# 终端实现与诊断

本文件从原 AGENTS.md 分离，记录 Ghostty、PTY、Shell 集成和资源生命周期约束。修改终端时结合当前源码核实并同步更新；以下约束不代表已完成各平台或长期运行验收。开发规则见 [AGENTS.md](../AGENTS.md)。

## Ghostty 模型、渲染与资源约束

libghostty-vt is the only terminal model. WebGPU is the default renderer and
RCode WebGL is the compatibility fallback. Each leaf owns one persistent model;
presentation resources are shared, bounded, and released for hidden leaves.
Native cell, grapheme and hyperlink presentation buffers allocate on first use
and return to the WASM allocator when presentation is reclaimed. Hidden parsing
does not rebuild them. Short visibility pauses retain uploaded cell data.
Unchanged frames do not acquire presentation textures or draw; cursor-only
updates retain cell buffers. WebGL background and decoration geometry uses
row range uploads while rectangle counts are stable; structural changes rebuild
the compact stream. Scrollbar synchronization reads no DOM on unchanged frames.
Blink timers stop in unfocused windows, and native
cursor hiding stops cursor timers. Selection damages only its old and new rows;
streaming search invalidations coalesce instead of rebuilding per output chunk.
WebGL surface and renderer code load only when selected, needed for fallback,
or explicitly requested by diagnostics. Delayed imports cannot install into a
closed, restarted, or replaced session.
xterm, its addons, CSS, snapshots, session pool, and dormant byte ring are removed.
Unsupported graphics produce a visible error with retry instead of changing models.

Command blocks use parser-time Ghostty tracked pins, including endpoint columns,
so command ranges survive reflow and exclude following prompts and commands.
The native marker ring is capped at 2,048 pins; JavaScript history is capped at
1,000 blocks and 512 KiB of estimated UTF-16 command/cwd text. Block implementation
and UI load only for block sessions, and hidden/occluded block presentation stops.
Block search yields after 128 rows or four milliseconds, retains at most 500
matches, and cancels obsolete queries. Block copy, search, sticky headers, navigation, Ask AI, rerun, shared shell input,
and selection all use the same persistent Ghostty model.
Block chrome commits with its matching renderer frame; command-editor focus does
not lower active-pane cadence. Divider padding is presentation-only and does not
change copied command boundaries. Scrollbars preserve native fractional positions
and ignore delayed programmatic scroll events. Hidden output does no surface DOM
or search-mask work until presentation resumes.

Terminal clipboard shortcuts use the native text clipboard plugin on all desktop
platforms. Context clicks expose the selected text through the input element to
restore the webview's native text menu; no persistent DOM scrollback is maintained.
Unclaimed macOS Command shortcuts reach the native menu after explicit clipboard,
block-editor, and readline bindings, including when Kitty keyboard mode is active.
The block prompt retains an enabled terminal input proxy for native menus and
routes editing keys, composed text and paste back to its command editor.
Character drags retain a native selection pin from pointerdown, including before
the first pointermove. Unmoved clicks and lost captures discard provisional pins.
Key encoding supplies the base character required by Kitty keyboard mode; plain
keys and key releases also use Ghostty encoding when the application requests it.
Terminal text uses the configured font, an installed Nerd Font when detected,
or bundled JetBrains Mono. Private-use prompt symbols require an installed font
that supplies them; RCode does not ship a separate symbol font. Native color
emoji remain on the system fallback path. Rerun requires the complete command
submitted through RCode; truncated shell labels are never executed.
Primary-screen full erase, scrollback erase, and terminal reset invalidate block
pins at parse time and clear block chrome, selection, and search. Commands after
an erase in the same output chunk retain their new pins. Alternate-screen erases
preserve primary block history. Block scrollbar status dots are removed along
with their timers and history scans.

The shared command bar activates after shell integration confirms prompt input.
Bare shells keep direct terminal input. Bash before 4.4 reports
`OSC 133;B;rcode_blocks=0` and keeps its native prompt because it lacks PS0.

Settings offer Automatic or WebGL for new terminals, plus opt-in screen reader
output. Accessible text is limited to 256 rows / 64 KiB and refreshes at most four
times per second while visible. Ordinary URL detection runs on pointer demand;
OSC 8 links take precedence. OSC 52 side effects retain one in-flight write and
only the latest pending value across the window.

The adapted Ghostty revision is pinned in `packages/ghostty-core/adapted/wasm/build.zig.zon`.
Both SIMD and scalar artifacts are shipped; the loader fetches only the variant
the webview supports. Scalar validation explicitly disables SIMD instructions
and types. This avoids changing OS minimums solely for the WASM SIMD requirement;
actual older WKWebView and WebKitGTK compatibility still needs platform tests.

PTY output retains a 2 MiB pending plus in-flight byte limit and two-message
window. Acknowledgments are cumulative parsed-byte offsets validated against
native chunk boundaries, so duplicates and retries cannot grant extra credit.
Parser failures stop delivery visibly without acknowledging unconsumed bytes.
Exit waits for the reader drain and final parsing acknowledgments. Unix readers
sleep on PTY readiness plus an explicit shutdown signal without a polling timer.
After shell exit they consume ready output, bounded to 2 MiB / 30 seconds, rather
than wait indefinitely for an inherited slave descriptor to close. Exceeding this
drain bound reports a reader failure and exit status -1. After shell exit,
30 seconds without acknowledgment progress closes a stalled queue with a logged
delivery failure and exit status -1. The deadline is armed before ConPTY close
and thread joins; it does not run during live-shell backpressure. Close wakes blocked
queue workers and the Unix reader; Windows keeps draining the pipe while ConPTY closes.

Enable release diagnostics with `window.__rcodeSetTerminalDiagnostics(true)`
and reload. `window.__rcodeTerm()` reads frontend counters;
`await window.__rcodeTermSnapshot()` adds native queue counters and explicitly
labeled host RSS. Host RSS
excludes WebContent and GPU processes and is not total application memory.

Ghostty presentation uses shared native macOS occlusion/sleep and DOM visibility
tracking. It pauses immediately, retains presentation for two seconds during
short desktop transitions, and then reclaims hidden-window GPU resources.
Sleep requests immediate reclamation; hidden tabs still release their leases
immediately. Per-pane pacing prevents focused output from raising background
pane cadence. WebGPU permits at most two outstanding frame submissions.
User wheel, drag, and keyboard interaction gives only its pane 150 ms of focused
cadence, including in an unfocused visible window. It starts no idle timer or
frame loop. Reclaimed WebGPU canvases shrink to 1x1 while retaining their target
geometry for the next presentation transaction.
`window.__rcodeTermTrace()` explicitly starts a bounded ten-minute resource trace;
it is never started automatically. `pnpm soak:ghostty` exercises real WASM models
without launching the application. `pnpm profile:ghostty` compares allocation
and parsing workloads in a fresh process per artifact and optional baseline.

Automated checks alone do not
establish production readiness, platform parity, or multi-day resource stability.

## PTY 与 Shell 集成

PTY shells are bootstrapped via injected init scripts in `apps/desktop/src/modules/pty/scripts/`:

- **Unix** (`zshenv.zsh`, `zprofile.zsh`, `zlogin.zsh`, `zshrc.zsh`, `bashrc.bash`) for zsh/bash, plus `init.fish` installed to `~/.rcode/cache/shell-integration/fish/init.fish` for fish. Emit OSC 7 (cwd) and OSC 133 A/B/C/D (prompt boundaries + exit code) so the host can track cwd and detect command boundaries without re-parsing the prompt. Fish 4.0+ writes its own OSC 133 prompt markers; RCode sets `fish_features=no-mark-prompt` and re-asserts its own prompt via `-C` to avoid doubling.
- **Windows** (`profile.ps1`) - passed via `pwsh -NoLogo -NoExit -ExecutionPolicy Bypass -File <path>`. Wraps the user's existing `prompt` function (after their `$PROFILE` runs) to emit OSC 7 + OSC 133 A/B/D. Shell priority: `pwsh.exe` (PS 7+) → `powershell.exe` (PS 5.1) → `cmd.exe` (no integration). cwd is normalized to backslashes before being passed to ConPTY (`CreateProcessW` misbehaves with forward-slash cwd).

`pty/shell_init.rs` is split into `#[cfg(unix)]` / `#[cfg(windows)]` modules - keep new platform-specific code in the right cfg arm.

ConPTY on Windows requires `CONPTY_LIFECYCLE_LOCK` (Mutex) around `openpty + spawn_command` in `session.rs`. Concurrent spawns leave one of the resulting PTYs with a stalled output pipe. Don't remove the lock without verifying first-tab stability under fast tab spam.

Each ConPTY child is also assigned to a per-session **Job Object** with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (`pty/job.rs`). When the Job HANDLE drops - clean shutdown, panic, or even SIGKILL'd RCode process - the kernel kills every descendant of the shell (e.g. `npm run dev` spawned from inside pwsh). Without this Windows orphans the entire process subtree because `TerminateProcess` only kills the immediate child. macOS/Linux rely on `Drop for Session → killer.kill()`; on dev-`Ctrl-C` of `cargo run` destructors don't fire and orphans are possible there too - acceptable for now since dev only.

## 平台与生命周期注意事项

- **React 19 StrictMode**, when enabled in development, double-mounts `useEffect` in dev → terminals spawn twice on first render. The first PTY is cleaned up almost immediately. The `CONPTY_LIFECYCLE_LOCK` mutex serializes this; don't be alarmed by `pty opened id=1` followed by `pty closed id=1` in dev logs.
- **Windows PowerShell process lifecycle**: `killer.kill()` from `portable-pty` only kills the immediate child. Descendants (e.g. `npm run dev` started inside pwsh) survive unless something else takes them down. The Job Object in `pty/job.rs` handles this for the RCode-process-death case; an explicit `pty_close` from JS also kills only the immediate child + relies on the Job to take the rest. Don't disable the Job without a replacement.
- **Tab `cwd` storage**: comes from OSC 7 with forward slashes (after `parseOsc7` strips `/C:` → `C:`). Anything that consumes `tab.cwd` and passes it to a Rust fs command on Windows must normalize separators or accept both forms - `apply_common` in `pty::shell_init` handles this for PTY spawn; other call sites must do their own.
