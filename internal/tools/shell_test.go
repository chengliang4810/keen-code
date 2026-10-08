package tools

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

// bashToolByEnv builds a Bash tool over a fresh work directory.
func bashToolFor(t *testing.T, dir string) Tool {
	t.Helper()
	return NewBash(dir, 120*time.Second)
}

// pidExists reports whether a process id is still alive.
func pidExists(pid int) bool {
	return syscall.Kill(pid, 0) == nil
}

func TestBashEchoReport(t *testing.T) {
	dir := t.TempDir()
	out := callTool(t, bashToolFor(t, dir), `{"command":"echo hello"}`)
	if out.IsError {
		t.Fatalf("echo 不应失败：%s", out.Content)
	}
	if !strings.Contains(out.Content, "hello") {
		t.Fatalf("stdout 预览缺少 hello：%q", out.Content)
	}
	if !strings.Contains(out.Content, "退出码 0") {
		t.Fatalf("报告缺少退出码：%q", out.Content)
	}
	if !strings.Contains(out.Content, "stdout（6 字节）：") {
		t.Fatalf("报告缺少 stdout 字节数：%q", out.Content)
	}
	if !strings.Contains(out.Content, "stderr（0 字节）：") || !strings.Contains(out.Content, "<空>") {
		t.Fatalf("空 stderr 应显示占位：%q", out.Content)
	}
}

func TestBashNonZeroExitIsErrorResult(t *testing.T) {
	dir := t.TempDir()
	out := callTool(t, bashToolFor(t, dir), `{"command":"echo oops >&2; exit 3"}`)
	if !out.IsError {
		t.Fatalf("非零退出应产生错误结果：%+v", out)
	}
	if !strings.Contains(out.Content, "退出码 3") || !strings.Contains(out.Content, "oops") {
		t.Fatalf("报告缺少退出码与 stderr：%q", out.Content)
	}
}

func TestBashTimeoutKillsAndReports(t *testing.T) {
	dir := t.TempDir()
	start := time.Now()
	out := callTool(t, bashToolFor(t, dir), `{"command":"sleep 30","timeout_ms":300}`)
	elapsed := time.Since(start)
	if !out.IsError {
		t.Fatalf("超时应产生错误结果：%+v", out)
	}
	if !strings.Contains(out.Content, "执行超时（300 毫秒）并已终止整个进程树") {
		t.Fatalf("报告缺少超时说明：%q", out.Content)
	}
	if elapsed > 5*time.Second {
		t.Fatalf("超时未及时返回，耗时 %v", elapsed)
	}
}

func TestBashTimeoutKillsWholeProcessGroup(t *testing.T) {
	dir := t.TempDir()
	// The grandchild sleep inherits the group; the group kill must reap it.
	out := callTool(t, bashToolFor(t, dir), `{"command":"sleep 30 & echo PID=$!","timeout_ms":300}`)
	if !out.IsError || !strings.Contains(out.Content, "执行超时") {
		t.Fatalf("超时报告异常：%q", out.Content)
	}
	re := regexp.MustCompile(`PID=(\d+)`)
	match := re.FindStringSubmatch(out.Content)
	if match == nil {
		t.Fatalf("报告中找不到子进程 PID：%q", out.Content)
	}
	pid, err := strconv.Atoi(match[1])
	if err != nil {
		t.Fatalf("PID 解析失败：%v", err)
	}
	deadline := time.Now().Add(3 * time.Second)
	for pidExists(pid) && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	if pidExists(pid) {
		t.Fatalf("孙进程 %d 未被进程组清理", pid)
	}
}

func TestBashSpillsTruncatedOutputToArtifact(t *testing.T) {
	dir := t.TempDir()
	out := callTool(t, bashToolFor(t, dir), `{"command":"head -c 200000 /dev/zero | tr '\\0' 'a'"}`)
	if out.IsError {
		t.Fatalf("大输出命令不应失败：%s", out.Content)
	}
	if !strings.Contains(out.Content, "中间输出已从预览省略；完整流共 200000 字节") {
		t.Fatalf("预览缺少截断标记：%q", out.Content)
	}
	pathRe := regexp.MustCompile(`stdout 完整输出：(\S+)`)
	match := pathRe.FindStringSubmatch(out.Content)
	if match == nil {
		t.Fatalf("报告缺少完整输出文件路径：%q", out.Content)
	}
	data, err := os.ReadFile(match[1])
	if err != nil {
		t.Fatalf("读取落盘输出失败：%v", err)
	}
	if len(data) != 200000 || strings.Count(string(data), "a") != 200000 {
		t.Fatalf("落盘输出应完整（200000 字节 a），得到 %d 字节", len(data))
	}
	// The head and tail preview keeps both ends.
	if !strings.Contains(out.Content, "stdout（200000 字节）：\naaaa") {
		t.Fatalf("预览缺少开头：%q", out.Content)
	}
}

func TestBashSmallOutputLeavesNoArtifact(t *testing.T) {
	dir := canonDir(t)
	tool := NewBash(dir, 120*time.Second)
	countArtifacts := func() int {
		entries, err := os.ReadDir(tool.(*bashTool).env.ArtifactDir())
		if err != nil {
			return 0
		}
		n := 0
		for _, e := range entries {
			if strings.Contains(e.Name(), "keencode-stdout") || strings.Contains(e.Name(), "keencode-stderr") {
				n++
			}
		}
		return n
	}
	before := countArtifacts()
	out := callTool(t, tool, `{"command":"echo tiny"}`)
	if out.IsError {
		t.Fatalf("echo 失败：%s", out.Content)
	}
	if strings.Contains(out.Content, "完整输出：") {
		t.Fatalf("小输出不应落盘：%q", out.Content)
	}
	if after := countArtifacts(); after != before {
		t.Fatalf("成功小命令不应残留输出文件：before=%d after=%d", before, after)
	}
}

func TestBashCwdAndBoundary(t *testing.T) {
	env, workDir := airtightEnv(t, nil)
	bash := &bashTool{env: env}
	if err := os.MkdirAll(filepath.Join(workDir, "sub"), 0o755); err != nil {
		t.Fatalf("mkdir 失败：%v", err)
	}
	out := callTool(t, bash, `{"command":"pwd","cwd":"sub"}`)
	if out.IsError || !strings.Contains(out.Content, filepath.ToSlash(filepath.Join(workDir, "sub"))) {
		t.Fatalf("cwd 未生效：%q", out.Content)
	}

	out = callTool(t, bash, `{"command":"pwd","cwd":".."}`)
	if !out.IsError || !strings.Contains(out.Content, "outside the workspace") {
		t.Fatalf("cwd 越界应拒绝：%q", out.Content)
	}

	outsideDir, err := os.MkdirTemp("", "keencode-outside-*")
	if err != nil {
		t.Fatalf("MkdirTemp 失败：%v", err)
	}
	defer os.RemoveAll(outsideDir)
	outside := filepath.Join(outsideDir, "elsewhere.txt")
	out = callTool(t, bash, `{"command":"cat `+outside+`"}`)
	if !out.IsError || !strings.Contains(out.Content, "command touches") {
		t.Fatalf("命令内越界路径应拒绝：%q", out.Content)
	}

	// The rejected command never executed.
	if _, err := os.Stat(outside); err == nil {
		t.Fatalf("被拒绝的命令不应创建目标文件")
	}
}

func TestBashSystemPathReadsAreAllowed(t *testing.T) {
	if _, err := os.Stat("/etc/hosts"); err != nil {
		t.Skip("/etc/hosts 不存在")
	}
	dir := t.TempDir()
	out := callTool(t, bashToolFor(t, dir), `{"command":"wc -c /etc/hosts"}`)
	if out.IsError {
		t.Fatalf("系统目录读取应放行：%s", out.Content)
	}
}

func TestBashCancelBeforeStart(t *testing.T) {
	dir := canonDir(t)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	out, err := bashToolFor(t, dir).Execute(ctx, Invocation{
		CallID: "c", Name: "Bash", Input: json.RawMessage(`{"command":"echo hi"}`),
		WorkDir: dir,
	})
	if err != nil {
		t.Fatalf("取消路径不应返回基础设施错误：%v", err)
	}
	if !out.IsError || !strings.Contains(out.Content, "工具调用已取消") {
		t.Fatalf("取消应返回稳定结果：%+v", out)
	}
}

func TestBashValidation(t *testing.T) {
	dir := t.TempDir()
	bash := bashToolFor(t, dir)
	cases := []struct {
		name    string
		input   string
		wantSub string
	}{
		{"empty command", `{"command":"  "}`, "Shell command 不能为空"},
		{"empty cwd", `{"command":"ls","cwd":" "}`, "命令工作目录不能为空"},
		{"zero timeout", `{"command":"ls","timeout_ms":0}`, "timeout_ms 必须在 1 到"},
		{"huge timeout", `{"command":"ls","timeout_ms":7200000}`, "timeout_ms 必须在 1 到"},
		{"unknown field", `{"command":"ls","extra":true}`, "工具输入无效"},
		{"missing cwd dir", `{"command":"ls","cwd":"no/such/dir"}`, "命令工作目录不存在或不是目录"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			out := callTool(t, bash, tc.input)
			if !out.IsError || !strings.Contains(out.Content, tc.wantSub) {
				t.Fatalf("输出 = %q，want 包含 %q", out.Content, tc.wantSub)
			}
		})
	}
}

func TestBashEnvErrReturnsInfraError(t *testing.T) {
	tool := NewBash(filepath.Join(t.TempDir(), "missing"), time.Second)
	impl := tool.(*bashTool)
	if impl.envErr == nil {
		t.Fatalf("非法工作目录应记录环境错误")
	}
	if _, err := tool.Execute(context.Background(), Invocation{
		CallID: "c", Name: "Bash", Input: json.RawMessage(`{"command":"echo hi"}`),
	}); err == nil {
		t.Fatalf("环境损坏应返回基础设施错误")
	}
}

func TestResolveProgramPrefersPATH(t *testing.T) {
	if _, err := exec.LookPath("sh"); err != nil {
		t.Skip("sh 不存在")
	}
	got, err := resolveProgram([]string{"sh"})
	if err != nil {
		t.Fatalf("resolveProgram 失败：%v", err)
	}
	if got == "" {
		t.Fatalf("应解析到 sh 路径")
	}
	if _, err := resolveProgram([]string{"definitely-not-a-real-binary-xyz"}); err == nil {
		t.Fatalf("未知程序应失败")
	}
}

func minInt(a, b int) int {
	if a < b {
		return a
	}
	return b
}
