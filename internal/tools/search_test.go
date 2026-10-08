package tools

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestCompileGlobPatterns(t *testing.T) {
	cases := []struct {
		pattern string
		path    string
		want    bool
	}{
		{"*.go", "main.go", true},
		{"*.go", "sub/main.go", false}, // * never crosses /
		{"**/*.go", "main.go", true},
		{"**/*.go", "sub/deep/main.go", true},
		{"src/**/*.rs", "src/main.rs", true},
		{"src/**/*.rs", "src/a/b/main.rs", true},
		{"src/**/*.rs", "other/main.rs", false},
		{"a/**", "a/x", true},
		{"a/**", "a/x/y", true},
		{"a/**", "a", false},
		{"**", "anything", true},
		{"**", "a/b/c", true},
		{"?.txt", "a.txt", true},
		{"?.txt", "ab.txt", false},
		{"[abc].go", "a.go", true},
		{"[abc].go", "d.go", false},
		{"[a-c].go", "b.go", true},
		{"[!a].go", "b.go", true},
		{"[!a].go", "a.go", false},
		{"data[01].bin", "data0.bin", true},
		{"data[01].bin", "data2.bin", false},
	}
	for _, tc := range cases {
		t.Run(tc.pattern+"~"+tc.path, func(t *testing.T) {
			matcher, err := compileGlob(tc.pattern)
			if err != nil {
				t.Fatalf("compileGlob(%q) 失败：%v", tc.pattern, err)
			}
			if got := matcher.MatchString(tc.path); got != tc.want {
				t.Fatalf("match(%q, %q) = %v，want %v", tc.pattern, tc.path, got, tc.want)
			}
		})
	}
}

func TestCompileGlobRejectsBadPatterns(t *testing.T) {
	for _, pattern := range []string{"[a", "[]", "x[!!!]y"} {
		if _, err := compileGlob(pattern); err == nil {
			t.Fatalf("compileGlob(%q) 应失败", pattern)
		}
	}
}

// buildTree creates fixture files under a fresh work directory and returns
// the directory.
func buildTree(t *testing.T, files map[string]string) string {
	t.Helper()
	dir := canonDir(t)
	for name, content := range files {
		mustWriteFile(t, filepath.Join(dir, name), content)
	}
	return dir
}

func TestGlobMatchesSkipsGitAndSorts(t *testing.T) {
	dir := buildTree(t, map[string]string{
		"main.go":         "package main\n",
		"sub/inner.go":    "package sub\n",
		"sub/deep/x.go":   "package deep\n",
		".hidden.go":      "package hidden\n",
		".git/tracked.go": "should skip\n",
		"README.md":       "# doc\n",
	})
	out := callTool(t, NewGlob(dir), `{"pattern":"**/*.go"}`)
	if out.IsError {
		t.Fatalf("Glob 不应失败：%s", out.Content)
	}
	got := strings.Split(out.Content, "\n")
	want := []string{
		filepath.ToSlash(filepath.Join(dir, ".hidden.go")),
		filepath.ToSlash(filepath.Join(dir, "main.go")),
		filepath.ToSlash(filepath.Join(dir, "sub", "deep", "x.go")),
		filepath.ToSlash(filepath.Join(dir, "sub", "inner.go")),
	}
	if len(got) != len(want) {
		t.Fatalf("Glob 结果 = %v，want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("Glob 结果[%d] = %q，want %q", i, got[i], want[i])
		}
	}

	// A top-level pattern never matches nested files.
	out = callTool(t, NewGlob(dir), `{"pattern":"*.go"}`)
	if out.IsError || !strings.Contains(out.Content, "main.go") || strings.Contains(out.Content, "inner.go") {
		t.Fatalf("顶层模式输出 = %q", out.Content)
	}
}

func TestGlobTruncatesAndReportsNoMatch(t *testing.T) {
	dir := buildTree(t, map[string]string{
		"a.txt": "a\n", "b.txt": "b\n", "c.txt": "c\n",
	})
	out := callTool(t, NewGlob(dir), `{"pattern":"*.txt","max_results":2}`)
	if out.IsError {
		t.Fatalf("Glob 失败：%s", out.Content)
	}
	lines := strings.Split(out.Content, "\n")
	if len(lines) != 3 || !strings.Contains(lines[len(lines)-1], "[结果已截断到 2 个文件]") {
		t.Fatalf("截断输出 = %q", out.Content)
	}

	out = callTool(t, NewGlob(dir), `{"pattern":"*.zzz"}`)
	if out.IsError || out.Content != "未找到匹配文件" {
		t.Fatalf("空结果输出 = %q", out.Content)
	}

	out = callTool(t, NewGlob(dir), `{"pattern":"*","path":"no/such/dir"}`)
	if !out.IsError || !strings.Contains(out.Content, "搜索根不是目录") {
		t.Fatalf("非法根目录输出 = %q", out.Content)
	}

	out = callTool(t, NewGlob(dir), `{"pattern":"[a"}`)
	if !out.IsError || !strings.Contains(out.Content, "Glob 无效") {
		t.Fatalf("非法模式输出 = %q", out.Content)
	}
}

func TestGrepContentWithContext(t *testing.T) {
	dir := buildTree(t, map[string]string{
		"code.txt": "alpha\nneedle here\ngamma\ndelta\nneedle two\n",
	})
	out := callTool(t, NewGrep(dir), `{"pattern":"needle","path":"code.txt","context_before":1,"context_after":1}`)
	if out.IsError {
		t.Fatalf("Grep 失败：%s", out.Content)
	}
	want := filepath.ToSlash(filepath.Join(dir, "code.txt")) + "\n" +
		"1-alpha\n2:needle here\n3-gamma\n4-delta\n5:needle two"
	if out.Content != want {
		t.Fatalf("Grep 输出 =\n%q\nwant\n%q", out.Content, want)
	}
}

func TestGrepModesAndFlags(t *testing.T) {
	dir := buildTree(t, map[string]string{
		"a.txt": "Find Me\nskip\nfind me again\n",
		"b.txt": "nothing here\n",
		"c.txt": "FIND ME\n",
	})

	out := callTool(t, NewGrep(dir), `{"pattern":"find me","case_insensitive":true,"output_mode":"files_with_matches"}`)
	if out.IsError {
		t.Fatalf("Grep 失败：%s", out.Content)
	}
	lines := strings.Split(out.Content, "\n")
	if len(lines) != 2 || !strings.HasSuffix(lines[0], "a.txt") || !strings.HasSuffix(lines[1], "c.txt") {
		t.Fatalf("files_with_matches 输出 = %q", out.Content)
	}

	out = callTool(t, NewGrep(dir), `{"pattern":"find me","case_insensitive":true,"output_mode":"count"}`)
	if out.IsError {
		t.Fatalf("Grep 失败：%s", out.Content)
	}
	if !strings.Contains(out.Content, filepath.ToSlash(filepath.Join(dir, "a.txt"))+":2") ||
		!strings.Contains(out.Content, filepath.ToSlash(filepath.Join(dir, "c.txt"))+":1") {
		t.Fatalf("count 输出 = %q", out.Content)
	}

	out = callTool(t, NewGrep(dir), `{"pattern":"find me","case_insensitive":true,"max_results":2}`)
	if out.IsError || !strings.Contains(out.Content, "[结果已截断到 2 项]") {
		t.Fatalf("截断输出 = %q", out.Content)
	}

	out = callTool(t, NewGrep(dir), `{"pattern":"zzz"}`)
	if out.IsError || out.Content != "未找到匹配内容" {
		t.Fatalf("空结果输出 = %q", out.Content)
	}
}

func TestGrepMultilineAndGlobFilter(t *testing.T) {
	dir := buildTree(t, map[string]string{
		"span.txt": "start\nmiddle end\n",
		"skip.log": "start\nmiddle end\n",
	})
	out := callTool(t, NewGrep(dir), `{"pattern":"start.middle","multiline":true,"output_mode":"content"}`)
	if out.IsError {
		t.Fatalf("multiline Grep 失败：%s", out.Content)
	}
	if !strings.Contains(out.Content, "1:start") || !strings.Contains(out.Content, "2:middle end") {
		t.Fatalf("跨行匹配应把两行都记为匹配行：%q", out.Content)
	}

	out = callTool(t, NewGrep(dir), `{"pattern":"start","glob":"*.txt","output_mode":"files_with_matches"}`)
	if out.IsError {
		t.Fatalf("Glob 过滤失败：%s", out.Content)
	}
	if strings.Contains(out.Content, "skip.log") {
		t.Fatalf("glob 过滤应排除 .log：%q", out.Content)
	}
}

func TestGrepSkipsBinaryLargeAndUnreadable(t *testing.T) {
	dir := t.TempDir()
	mustWriteFile(t, filepath.Join(dir, "bin.dat"), "text\x00here\n")
	mustWriteFile(t, filepath.Join(dir, "ok.txt"), "match\n")

	env, err := NewEnvironment(dir, WithLimits(Limits{
		MaxReadLines:           10,
		MaxReadOutputBytes:     64,
		MaxSearchResults:       10,
		MaxSearchFileBytes:     16,
		MaxMutationFileBytes:   1 << 20,
		DefaultCommandTimeout:  time.Second,
		MaxCommandTimeout:      time.Second,
		MaxCommandPreviewBytes: 256,
	}))
	if err != nil {
		t.Fatalf("NewEnvironment 失败：%v", err)
	}
	grep := &grepTool{env: env}
	out := callTool(t, grep, `{"pattern":"match|text"}`)
	if out.IsError {
		t.Fatalf("Grep 失败：%s", out.Content)
	}
	if !strings.Contains(out.Content, "[跳过：超大文件 0，二进制或非 UTF-8 文件 1") {
		t.Fatalf("跳过统计缺失：%q", out.Content)
	}
	if !strings.Contains(out.Content, "ok.txt") {
		t.Fatalf("应保留正常文件命中：%q", out.Content)
	}
}

func TestGrepValidation(t *testing.T) {
	dir := t.TempDir()
	grep := NewGrep(dir)
	cases := []struct {
		name    string
		input   string
		wantSub string
	}{
		{"empty pattern", `{"pattern":" "}`, "搜索模式不能为空"},
		{"empty glob", `{"pattern":"x","glob":" "}`, "glob 过滤器不能为空"},
		{"context too large", `{"pattern":"x","context_before":101}`, "不能超过 100"},
		{"context wrong mode", `{"pattern":"x","output_mode":"count","context_before":1}`, "上下文行只适用于"},
		{"bad output mode", `{"pattern":"x","output_mode":"json"}`, "output_mode"},
		{"invalid regex", `{"pattern":"a(b"}`, "正则无效"},
		{"zero max results", `{"pattern":"x","max_results":0}`, "必须大于零"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			out := callTool(t, grep, tc.input)
			if !out.IsError || !strings.Contains(out.Content, tc.wantSub) {
				t.Fatalf("输出 = %q，want 包含 %q", out.Content, tc.wantSub)
			}
		})
	}
}

func TestGrepSingleFileRoot(t *testing.T) {
	dir := buildTree(t, map[string]string{"one.txt": "hit\n", "two.txt": "hit\n"})
	out := callTool(t, NewGrep(dir), `{"pattern":"hit","path":"one.txt","output_mode":"count"}`)
	if out.IsError {
		t.Fatalf("Grep 失败：%s", out.Content)
	}
	if !strings.Contains(out.Content, filepath.ToSlash(filepath.Join(dir, "one.txt"))+":1") ||
		strings.Contains(out.Content, "two.txt") {
		t.Fatalf("单文件根输出 = %q", out.Content)
	}
}

func TestGlobGrepSandboxRejectsOutsideRoot(t *testing.T) {
	env, workDir := airtightEnv(t, nil)
	outsideDir, err := os.MkdirTemp("", "keencode-outside-*")
	if err != nil {
		t.Fatalf("MkdirTemp 失败：%v", err)
	}
	defer os.RemoveAll(outsideDir)
	mustWriteFile(t, filepath.Join(outsideDir, "f.txt"), "x\n")
	out := callTool(t, &globTool{env: env}, `{"pattern":"*","path":`+quote(outsideDir)+`}`)
	if !out.IsError || !strings.Contains(out.Content, "outside the workspace") {
		t.Fatalf("Glob 工作区外根应拒绝，得到 %q", out.Content)
	}
	out = callTool(t, &grepTool{env: env}, `{"pattern":"x","path":`+quote(filepath.Join(workDir, "..", ".."))+`}`)
	if !out.IsError || !strings.Contains(out.Content, "outside the workspace") {
		t.Fatalf("Grep 工作区外根应拒绝，得到 %q", out.Content)
	}
}
