package tools

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"
	"unicode/utf8"

	"keencode/internal/model"
)

// Report budgets mirroring the agent layer (core/agent/src/tool.rs:18-33):
// successful tool text is capped at 512 KiB, a failed tool's message at
// 4 KiB.
const (
	maxReportTextBytes  = 512 << 10
	maxReportErrorBytes = 4 << 10
	// bashSummaryMaxRunes bounds the single-line tool-card summary.
	bashSummaryMaxRunes = 160
)

// bashTool runs one non-interactive command string through the system Bash
// with -lc, under a hard timeout that kills the whole process group, with
// per-stream head+tail previews and full-output spill to disk
// (command.rs:38-177). Recorded v1 divergence: run_in_background and the
// PowerShell sibling are deferred — the background manager is a session
// capability the v1 plan does not include.
type bashTool struct {
	env    *Environment
	envErr error
}

// NewBash builds the Bash tool bound to one work directory. timeout is the
// default command timeout; values at or below zero keep the 120 s default
// and values above the limit clamp to it.
func NewBash(workDir string, timeout time.Duration) Tool {
	env, err := NewEnvironment(workDir)
	if err == nil && timeout > 0 {
		if timeout > env.limits.MaxCommandTimeout {
			timeout = env.limits.MaxCommandTimeout
		}
		env.limits.DefaultCommandTimeout = timeout
	}
	return &bashTool{env: env, envErr: err}
}

// Definition returns the strict Bash schema.
func (t *bashTool) Definition() model.ToolDefinition {
	limits := DefaultLimits()
	if t.env != nil {
		limits = t.env.Limits()
	}
	timeoutDescription := fmt.Sprintf(
		"Optional timeout in milliseconds; omit to use the %d ms default, values above %d fail validation.",
		limits.DefaultCommandTimeout.Milliseconds(), limits.MaxCommandTimeout.Milliseconds())
	return model.ToolDefinition{
		Name: "Bash",
		Description: "Run a command non-interactively using system Bash with -lc. Commands may change state inside or outside the project and are always treated as side-effecting tools. Cancellation or timeout terminates the entire process group.\n\n" +
			"Working rules:\n" +
			"- Quote every path and argument; never interpolate untrusted text into an executable position.\n" +
			"- Do not start interactive commands (editors, pagers, prompts). Pass non-interactive flags such as -y or --yes, and feed input through files instead of a terminal.\n" +
			"- Prefer file tools for reading and editing files, and this tool for builds, tests, version control and system commands.\n" +
			"- Filter or redirect noisy output so the returned preview carries evidence rather than volume. A truncated preview keeps the head and tail and saves the complete output to a file you can read.\n" +
			"- Set timeout_ms for commands that can legitimately run long. A command that exceeds its timeout is killed with its process tree, so raise the limit instead of retrying blindly.\n\n" +
			"Version control safety:\n" +
			"- Never rewrite published history: no force push, and no rebase or commit --amend on commits that are not yours alone.\n" +
			"- Do not commit, push, tag or open pull requests unless the user asked for it.\n" +
			"- Never bypass hooks or checks with --no-verify, and never discard work with destructive commands (checkout --, reset --hard, clean -f, branch -D) unless the user explicitly asked.\n" +
			"- Inspect downloaded scripts before executing them.\n" +
			"- When a command fails, read the error and change the approach; repeat an identical invocation only when conditions have changed or the failure is demonstrably transient.",
		InputSchema: objectSchema(map[string]any{
			"command":    map[string]any{"type": "string", "minLength": 1},
			"cwd":        map[string]any{"type": "string", "minLength": 1},
			"timeout_ms": map[string]any{"type": "integer", "minimum": 1, "description": timeoutDescription},
		}, "command"),
	}
}

// Effect is always a side effect: any command can mutate anything.
func (t *bashTool) Effect(json.RawMessage) Effect { return EffectSideEffect }

// shellInput is the strict Shell input (command.rs:297-311).
type shellInput struct {
	Command   string  `json:"command"`
	Cwd       *string `json:"cwd,omitempty"`
	TimeoutMs *int64  `json:"timeout_ms,omitempty"`
}

// termination is why one supervised process ended (command.rs:528-535).
type termination int

const (
	terminationExited termination = iota
	terminationTimedOut
	terminationCancelled
)

// streamCapture holds one drained output pipe: a bounded head+tail preview,
// the total byte count, and the artifact file with the complete stream
// (command.rs:538-549, 1184-1254).
type streamCapture struct {
	path        string
	artifactErr string
	head        []byte
	tail        []byte
	headCap     int
	tailCap     int
	total       int64
	preview     string
	truncated   bool
	file        *os.File
}

// newStreamCapture creates the artifact file eagerly so the full stream is
// never lost to a later truncation decision.
func newStreamCapture(dir, label string, previewLimit int) *streamCapture {
	c := &streamCapture{
		headCap: previewLimit / 2,
		tailCap: previewLimit - previewLimit/2,
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		c.artifactErr = "创建输出目录失败：" + err.Error()
		return c
	}
	file, err := os.CreateTemp(dir, "keencode-"+label+"-*.log")
	if err != nil {
		c.artifactErr = "创建输出文件失败：" + err.Error()
		return c
	}
	c.file = file
	c.path = file.Name()
	return c
}

// Write implements io.Writer. It never reports failure: a full disk must
// not tear down the drain loop, because a dead pipe kills the child
// (command.rs:947-970 keeps draining for the same reason).
func (c *streamCapture) Write(p []byte) (int, error) {
	c.total += int64(len(p))
	if c.file != nil {
		if _, err := c.file.Write(p); err != nil {
			c.artifactErr = "保存完整输出失败：" + err.Error()
			c.file.Close()
			c.file = nil
		}
	}
	c.retain(p)
	return len(p), nil
}

// retain keeps the head and tail windows and drops the middle without
// stopping the drain (command.rs:1257-1275).
func (c *streamCapture) retain(p []byte) {
	headRoom := c.headCap - len(c.head)
	headTake := headRoom
	if headTake > len(p) {
		headTake = len(p)
	}
	c.head = append(c.head, p[:headTake]...)
	if c.tailCap == 0 || headTake == len(p) {
		return
	}
	c.tail = append(c.tail, p[headTake:]...)
	if excess := len(c.tail) - c.tailCap; excess > 0 {
		c.tail = append(c.tail[:0], c.tail[excess:]...)
	}
}

// finish closes the artifact file and renders the lossy preview.
func (c *streamCapture) finish() {
	if c.file != nil {
		if err := c.file.Sync(); err != nil && c.artifactErr == "" {
			c.artifactErr = "刷新完整输出失败：" + err.Error()
		}
		c.file.Close()
		c.file = nil
	}
	c.truncated = c.total > int64(c.headCap+c.tailCap)
	c.preview = renderStreamPreview(c.head, c.tail, c.truncated, c.total)
}

// Execute validates the input and supervises one process group.
func (t *bashTool) Execute(ctx context.Context, inv Invocation) (ToolOutput, error) {
	if t.envErr != nil {
		return ToolOutput{}, t.envErr
	}
	if err := t.env.BindInvocation(inv); err != nil {
		return ToolOutput{}, err
	}
	if result, done := ctxResult(ctx, ""); done {
		return result, nil
	}
	var in shellInput
	if err := decodeStrict(inv.Input, &in); err != nil {
		return errorResult(err.Error(), ""), nil
	}
	if strings.TrimSpace(in.Command) == "" {
		return errorResult("Shell command 不能为空", ""), nil
	}
	if in.Cwd != nil && strings.TrimSpace(*in.Cwd) == "" {
		return errorResult("命令工作目录不能为空", ""), nil
	}
	limits := t.env.Limits()
	timeout := limits.DefaultCommandTimeout
	if in.TimeoutMs != nil {
		if *in.TimeoutMs <= 0 || *in.TimeoutMs > limits.MaxCommandTimeout.Milliseconds() {
			return errorResult(fmt.Sprintf("timeout_ms 必须在 1 到 %d 之间",
				limits.MaxCommandTimeout.Milliseconds()), ""), nil
		}
		timeout = time.Duration(*in.TimeoutMs) * time.Millisecond
	}
	summary := bashSummary(in.Command)

	dir := t.env.WorkingDir()
	if in.Cwd != nil {
		resolved, err := t.env.ResolvePath(*in.Cwd)
		if err != nil {
			return errorResult(err.Error(), summary), nil
		}
		dir = resolved
	}
	if info, err := os.Stat(dir); err != nil || !info.IsDir() {
		return errorResult(fmt.Sprintf("命令工作目录不存在或不是目录：%s", displayPath(dir)), summary), nil
	}
	if err := t.env.CheckWorkspacePath(dir); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	if err := t.env.CheckCommandBoundary(in.Command); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	program, err := resolveProgram(bashCandidates())
	if err != nil {
		return errorResult(fmt.Sprintf("Bash 可执行文件不可用：%v", err), summary), nil
	}

	previewLimit := limits.MaxCommandPreviewBytes
	cmd := &exec.Cmd{
		Path: program,
		Args: []string{program, "-lc", in.Command},
		Dir:  dir,
	}
	configureProcessGroup(cmd)
	stdout := newStreamCapture(t.env.ArtifactDir(), "stdout", previewLimit)
	stderr := newStreamCapture(t.env.ArtifactDir(), "stderr", previewLimit)
	cmd.Stdout = stdout
	cmd.Stderr = stderr

	if err := cmd.Start(); err != nil {
		os.Remove(stdout.path)
		os.Remove(stderr.path)
		return errorResult(fmt.Sprintf("启动 Bash 可执行文件失败：%v", err), summary), nil
	}
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()

	kind := terminationExited
	var waitErr error
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case waitErr = <-done:
		// The leader exited; kill surviving group members so background
		// children do not outlive the call (command.rs:1129-1133).
		killProcessTree(cmd)
	case <-timer.C:
		killProcessTree(cmd)
		waitErr = <-done
		kind = terminationTimedOut
	case <-ctx.Done():
		killProcessTree(cmd)
		waitErr = <-done
		kind = terminationCancelled
	}
	stdout.finish()
	stderr.finish()

	exitCode := -1
	if waitErr == nil {
		exitCode = 0
	} else {
		var exitErr *exec.ExitError
		if errors.As(waitErr, &exitErr) {
			exitCode = exitErr.ExitCode()
		} else {
			return errorResult(fmt.Sprintf("等待命令退出失败：%v", waitErr), summary), nil
		}
	}
	report := renderBashReport("Bash", bashStatusText(kind, exitCode, timeout, limits.MaxCommandTimeout),
		dir, stdout, stderr, kind != terminationExited || exitCode != 0)
	if kind == terminationExited && exitCode == 0 {
		return textOutput(report, summary), nil
	}
	return errorResult(report, summary), nil
}

// bashStatusText renders the first report line for each termination
// (command.rs:1399-1412).
func bashStatusText(kind termination, exitCode int, timeout, maxTimeout time.Duration) string {
	switch kind {
	case terminationTimedOut:
		return fmt.Sprintf("执行超时（%d 毫秒）并已终止整个进程树；若该命令确实需要更长时间，请提高 timeout_ms（上限 %d 毫秒），不要按原参数重试",
			timeout.Milliseconds(), maxTimeout.Milliseconds())
	case terminationCancelled:
		return "已取消并清理进程树"
	default:
		if exitCode < 0 {
			return "被操作系统信号终止"
		}
		return fmt.Sprintf("退出码 %d", exitCode)
	}
}

// renderBashReport assembles the final model-facing report inside the
// success (512 KiB) or failure (4 KiB) budget (command.rs:1314-1453,
// simplified to a deterministic two-slice budget: metadata first, previews
// share the rest).
func renderBashReport(label, statusText, cwd string, stdout, stderr *streamCapture, failed bool) string {
	const reserve = 256

	stdoutHeading := fmt.Sprintf("\nstdout（%d 字节）：\n", stdout.total)
	stderrHeading := fmt.Sprintf("\nstderr（%d 字节）：\n", stderr.total)
	headings := len(stdoutHeading) + len(stderrHeading)

	previewA := stdout.preview
	previewB := stderr.preview
	if previewA == "" {
		previewA = "<空>"
	}
	if previewB == "" {
		previewB = "<空>"
	}

	budget := maxReportTextBytes
	if failed {
		budget = maxReportErrorBytes
	}
	metadataBudget := budget / 2
	if metadataBudget < 512 {
		metadataBudget = 512
	}
	// Budget the previews against the pre-cleanup metadata (its largest
	// form, artifact paths included) so the report can only shrink.
	metadataUpperBound := renderBashMetadata(label, statusText, cwd, stdout, stderr, metadataBudget)
	previewBudget := budget - len(metadataUpperBound) - headings - reserve
	if previewBudget < 0 {
		previewBudget = 0
	}
	budgetA := previewBudget / 2
	if rest := previewBudget - len(previewB); rest > budgetA {
		budgetA = rest
	}
	if budgetA > len(previewA) {
		budgetA = len(previewA)
	}
	previewA, truncatedA := fitPreview(previewA, budgetA)
	previewB, truncatedB := fitPreview(previewB, previewBudget-budgetA)

	// Keep an artifact copy exactly when the model could not see everything.
	for _, stream := range []struct {
		capture   *streamCapture
		truncated bool
	}{
		{stdout, stdout.truncated || truncatedA},
		{stderr, stderr.truncated || truncatedB},
	} {
		if stream.truncated || stream.capture.artifactErr != "" || stream.capture.path == "" {
			continue
		}
		if err := os.Remove(stream.capture.path); err != nil && !os.IsNotExist(err) {
			stream.capture.artifactErr = "清理临时输出失败：" + err.Error()
			continue
		}
		stream.capture.path = ""
	}
	metadata := renderBashMetadata(label, statusText, cwd, stdout, stderr, metadataBudget)
	return metadata + stdoutHeading + previewA + stderrHeading + previewB
}

// fitPreview shortens a preview to the budget with an explicit marker,
// preserving rune boundaries on both sides (command.rs:1354-1370).
func fitPreview(preview string, budget int) (string, bool) {
	const truncation = "\n...[预览已截断]...\n"
	if len(preview) <= budget {
		return preview, false
	}
	remaining := budget - len(truncation)
	if remaining < 0 {
		remaining = 0
	}
	headEnd := remaining / 2
	for headEnd > 0 && !utf8.RuneStart(preview[headEnd]) {
		headEnd--
	}
	tailStart := len(preview) - (remaining - remaining/2)
	for tailStart < len(preview) && !utf8.RuneStart(preview[tailStart]) {
		tailStart++
	}
	return preview[:headEnd] + truncation + preview[tailStart:], true
}

// renderBashMetadata renders status, artifact paths, spill warnings and the
// work directory, degrading fields to fixed fallbacks under budget
// (command.rs:1392-1453).
func renderBashMetadata(label, statusText, cwd string, stdout, stderr *streamCapture, maxBytes int) string {
	report := label + "：" + statusText
	type reportField struct{ full, fallback string }
	var fields []reportField
	for _, stream := range []struct {
		name    string
		capture *streamCapture
	}{{"stdout", stdout}, {"stderr", stderr}} {
		if stream.capture.path != "" {
			descriptor := "完整输出"
			if stream.capture.artifactErr != "" {
				descriptor = "输出文件（可能不完整）"
			}
			fields = append(fields, reportField{
				full:     fmt.Sprintf("\n%s %s：%s", stream.name, descriptor, displayPath(stream.capture.path)),
				fallback: fmt.Sprintf("\n%s 输出文件路径超出报告预算，已省略", stream.name),
			})
		}
	}
	for _, stream := range []struct {
		name    string
		capture *streamCapture
	}{{"stdout", stdout}, {"stderr", stderr}} {
		if stream.capture.artifactErr != "" {
			fields = append(fields, reportField{
				full:     fmt.Sprintf("\n%s 输出落盘警告：%s", stream.name, stream.capture.artifactErr),
				fallback: fmt.Sprintf("\n%s 输出落盘警告：读取、保存或清理失败（详情已省略）", stream.name),
			})
		}
	}
	fields = append(fields, reportField{
		full:     "\n工作目录：" + displayPath(cwd),
		fallback: "\n工作目录超出报告预算，已省略",
	})
	reserved := 0
	for _, f := range fields {
		reserved += min(len(f.full), len(f.fallback))
	}
	for _, f := range fields {
		smaller := min(len(f.full), len(f.fallback))
		reserved -= smaller
		if len(report)+len(f.full)+reserved <= maxBytes {
			report += f.full
		} else {
			report += f.fallback
		}
	}
	return report
}

// renderStreamPreview lossily decodes the head and tail and inserts an
// explicit omission marker when truncated (command.rs:1278-1298).
func renderStreamPreview(head, tail []byte, truncated bool, total int64) string {
	if !truncated {
		joined := make([]byte, 0, len(head)+len(tail))
		joined = append(joined, head...)
		joined = append(joined, tail...)
		return string(joined) // invalid bytes convert to U+FFFD
	}
	head = trimIncompleteTailRune(head)
	if skip := leadingContinuationBytes(tail); skip > 0 {
		tail = tail[skip:]
	}
	return string(head) +
		fmt.Sprintf("\n...[中间输出已从预览省略；完整流共 %d 字节]...\n", total) +
		string(tail)
}

// trimIncompleteTailRune drops a trailing incomplete UTF-8 sequence; a
// genuinely invalid lead byte stays and converts lossily like Rust's
// from_utf8_lossy.
func trimIncompleteTailRune(b []byte) []byte {
	low := len(b) - (utf8.UTFMax - 1)
	if low < 0 {
		low = 0
	}
	for i := len(b) - 1; i >= low; i-- {
		if !utf8.RuneStart(b[i]) {
			continue
		}
		want := utf8SequenceLen(b[i])
		if want == 0 {
			return b // invalid lead byte: keep, converts lossily
		}
		if len(b)-i < want {
			return b[:i] // incomplete trailing sequence
		}
		return b // complete sequence (valid or invalid): keep
	}
	return b
}

// leadingContinuationBytes counts the orphan continuation bytes at the tail
// window's start so the marker never splits a rune.
func leadingContinuationBytes(b []byte) int {
	n := 0
	for n < len(b) && n < utf8.UTFMax-1 && !utf8.RuneStart(b[n]) {
		n++
	}
	return n
}

// bashSummary renders the tool-card summary: the first command line,
// trimmed and rune-capped.
func bashSummary(command string) string {
	line := command
	if idx := strings.IndexAny(line, "\r\n"); idx >= 0 {
		line = line[:idx]
	}
	line = strings.TrimSpace(line)
	runes := []rune(line)
	if len(runes) > bashSummaryMaxRunes {
		line = string(runes[:bashSummaryMaxRunes]) + "…"
	}
	return line
}

// bashCandidates returns the Bash executables to try per platform
// (command.rs:686-699).
func bashCandidates() []string {
	if runtime.GOOS == "windows" {
		return []string{"bash.exe", `C:\Program Files\Git\bin\bash.exe`, `C:\Program Files\Git\usr\bin\bash.exe`}
	}
	return []string{"bash"}
}

// resolveProgram picks the first usable candidate: absolute paths must
// exist, bare names resolve through PATH (command.rs:1009-1030).
func resolveProgram(candidates []string) (string, error) {
	var lastErr error
	for _, candidate := range candidates {
		if filepath.IsAbs(candidate) {
			info, err := os.Stat(candidate)
			switch {
			case err == nil && !info.IsDir():
				return candidate, nil
			case err != nil:
				lastErr = err
			default:
				lastErr = fmt.Errorf("%s 不是可执行文件", candidate)
			}
			continue
		}
		path, err := exec.LookPath(candidate)
		if err == nil {
			return path, nil
		}
		lastErr = err
	}
	if lastErr == nil {
		lastErr = errors.New("没有候选可执行文件")
	}
	return "", lastErr
}
