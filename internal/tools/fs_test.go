package tools

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

// canonDir returns a canonical temp directory: on macOS t.TempDir() sits
// behind the /var -> /private/var symlink, and the tools report canonical
// paths.
func canonDir(t *testing.T) string {
	t.Helper()
	dir, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatalf("EvalSymlinks 失败：%v", err)
	}
	return dir
}

// airtightEnv builds an Environment whose scratch root is the work
// directory itself, so anything outside it (including sibling temp dirs)
// is genuinely outside the sandbox.
func airtightEnv(t *testing.T, mutate func(*Limits)) (*Environment, string) {
	t.Helper()
	dir := canonDir(t)
	limits := DefaultLimits()
	if mutate != nil {
		mutate(&limits)
	}
	env, err := NewEnvironment(dir, WithLimits(limits), WithTempRoot(dir))
	if err != nil {
		t.Fatalf("NewEnvironment 失败：%v", err)
	}
	return env, dir
}

// callTool executes one call and fails the test on infra errors.
func callTool(t *testing.T, tool Tool, input string) ToolOutput {
	t.Helper()
	out, err := tool.Execute(context.Background(), Invocation{
		CallID:  "call-1",
		Name:    tool.Definition().Name,
		Input:   json.RawMessage(input),
		WorkDir: workDirOf(t, tool),
	})
	if err != nil {
		t.Fatalf("Execute 返回基础设施错误：%v", err)
	}
	return out
}

// workDirOf extracts the bound work directory of a built-in tool.
func workDirOf(t *testing.T, tool Tool) string {
	t.Helper()
	switch impl := tool.(type) {
	case *ReadTool:
		return impl.env.WorkingDir()
	case *WriteTool:
		return impl.env.WorkingDir()
	case *EditTool:
		return impl.env.WorkingDir()
	case *globTool:
		return impl.env.WorkingDir()
	case *grepTool:
		return impl.env.WorkingDir()
	case *bashTool:
		return impl.env.WorkingDir()
	}
	t.Fatalf("未知工具实现 %T", tool)
	return ""
}

// mustWriteFile writes a fixture file or fails the test.
func mustWriteFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("创建夹具目录失败：%v", err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("写入夹具失败：%v", err)
	}
}

func TestReadWholeFileNumberedLines(t *testing.T) {
	dir := canonDir(t)
	path := filepath.Join(dir, "note.txt")
	mustWriteFile(t, path, "alpha\nbeta\ngamma\n")
	out := callTool(t, NewRead(dir), `{"file_path":"note.txt"}`)
	if out.IsError {
		t.Fatalf("Read 不应失败：%s", out.Content)
	}
	want := "文件：" + filepath.ToSlash(path) + "\n     1→alpha\n     2→beta\n     3→gamma"
	if out.Content != want {
		t.Fatalf("Read 输出 = %q，want %q", out.Content, want)
	}
	if out.Summary != filepath.ToSlash(path) {
		t.Fatalf("Summary = %q，want 路径", out.Summary)
	}
}

func TestReadPaginationAndContinuation(t *testing.T) {
	dir := canonDir(t)
	path := filepath.Join(dir, "big.txt")
	var lines []string
	for i := 1; i <= 5; i++ {
		lines = append(lines, strings.Repeat("x", 20)+strconv.Itoa(i))
	}
	mustWriteFile(t, path, strings.Join(lines, "\n")+"\n")

	out := callTool(t, NewRead(dir), `{"file_path":"big.txt","offset":2,"limit":2}`)
	if out.IsError {
		t.Fatalf("分页读取不应失败：%s", out.Content)
	}
	if !strings.Contains(out.Content, "     2→") || !strings.Contains(out.Content, "     3→") {
		t.Fatalf("应包含第 2、3 行：%q", out.Content)
	}
	if strings.Contains(out.Content, "     4→") {
		t.Fatalf("不应包含第 4 行：%q", out.Content)
	}
	wantMarker := "[仍有后续内容；下一次使用 offset=4]"
	if !strings.Contains(out.Content, wantMarker) {
		t.Fatalf("缺少续读提示 %q：%q", wantMarker, out.Content)
	}

	// The last page reports no continuation marker.
	out = callTool(t, NewRead(dir), `{"file_path":"big.txt","offset":4,"limit":10}`)
	if out.IsError {
		t.Fatalf("末页读取不应失败：%s", out.Content)
	}
	if strings.Contains(out.Content, "仍有后续内容") {
		t.Fatalf("文件读完不应有续读提示：%q", out.Content)
	}
}

func TestReadEmptyFileAndOutOfRange(t *testing.T) {
	dir := canonDir(t)
	path := filepath.Join(dir, "empty.txt")
	mustWriteFile(t, path, "")

	out := callTool(t, NewRead(dir), `{"file_path":"empty.txt"}`)
	if out.IsError {
		t.Fatalf("空文件不应失败：%s", out.Content)
	}
	if !strings.HasSuffix(out.Content, emptyReadBody) {
		t.Fatalf("空文件输出 = %q，want 以 %q 结尾", out.Content, emptyReadBody)
	}

	mustWriteFile(t, filepath.Join(dir, "three.txt"), "1\n2\n3\n")
	out = callTool(t, NewRead(dir), `{"file_path":"three.txt","offset":9}`)
	if !out.IsError || !strings.Contains(out.Content, "offset 9 超出文件末尾") {
		t.Fatalf("offset 越界应报错，得到 %q", out.Content)
	}
}

func TestReadRejectsBinaryAndInvalidUTF8(t *testing.T) {
	dir := canonDir(t)
	cases := []struct {
		name    string
		content string
		wantSub string
	}{
		{"nul byte", "hello\x00world", "NUL"},
		{"invalid utf8", "caf\xc3\xa9\xff\xfe", "有效 UTF-8"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			mustWriteFile(t, filepath.Join(dir, "data.bin"), tc.content)
			out := callTool(t, NewRead(dir), `{"file_path":"data.bin"}`)
			if !out.IsError || !strings.Contains(out.Content, tc.wantSub) {
				t.Fatalf("输出 = %q，want 包含 %q", out.Content, tc.wantSub)
			}
		})
	}
}

func TestReadStripsBOMAndCR(t *testing.T) {
	dir := canonDir(t)
	mustWriteFile(t, filepath.Join(dir, "win.txt"), "\xEF\xBB\xBFfirst\r\nsecond\r\n")
	out := callTool(t, NewRead(dir), `{"file_path":"win.txt"}`)
	if out.IsError {
		t.Fatalf("读取失败：%s", out.Content)
	}
	if strings.Contains(out.Content, "\r") {
		t.Fatalf("输出不应保留 CR：%q", out.Content)
	}
	if strings.ContainsRune(out.Content, '\ufeff') {
		t.Fatalf("输出不应保留 BOM：%q", out.Content)
	}
	if !strings.Contains(out.Content, "first") || !strings.Contains(out.Content, "second") {
		t.Fatalf("输出缺少正文：%q", out.Content)
	}
}

func TestReadLineTooLarge(t *testing.T) {
	env, err := NewEnvironment(canonDir(t), WithLimits(Limits{
		MaxReadLines:           10,
		MaxReadOutputBytes:     256,
		MaxSearchResults:       10,
		MaxSearchFileBytes:     1 << 20,
		MaxMutationFileBytes:   1 << 20,
		DefaultCommandTimeout:  time.Second,
		MaxCommandTimeout:      time.Second,
		MaxCommandPreviewBytes: 256,
	}))
	if err != nil {
		t.Fatalf("NewEnvironment 失败：%v", err)
	}
	dir := env.WorkingDir()
	mustWriteFile(t, filepath.Join(dir, "huge-line.txt"), strings.Repeat("a", 2000)+"\nsecond\n")
	out := callTool(t, &ReadTool{env: env}, `{"file_path":"huge-line.txt"}`)
	if !out.IsError || !strings.Contains(out.Content, "单行内容无法在 Read 输出字节上限内") {
		t.Fatalf("超长单行应报稳定错误，得到 %q", out.Content)
	}
}

func TestReadCancelReturnsStableResult(t *testing.T) {
	dir := canonDir(t)
	mustWriteFile(t, filepath.Join(dir, "f.txt"), "text\n")
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	out, err := NewRead(dir).Execute(ctx, Invocation{
		CallID: "c", Name: "Read", Input: json.RawMessage(`{"file_path":"f.txt"}`),
		WorkDir: dir,
	})
	if err != nil {
		t.Fatalf("取消路径不应返回基础设施错误：%v", err)
	}
	if !out.IsError || !strings.Contains(out.Content, "工具调用已取消") {
		t.Fatalf("取消应返回稳定结果，得到 %+v", out)
	}
}

func TestReadValidationAndSandbox(t *testing.T) {
	dir := canonDir(t)
	read := NewRead(dir)
	cases := []struct {
		name    string
		input   string
		wantSub string
	}{
		{"empty path", `{"file_path":"  "}`, "读取路径不能为空"},
		{"zero offset", `{"file_path":"f","offset":0}`, "offset 必须从 1 开始"},
		{"zero limit", `{"file_path":"f","limit":0}`, "limit 必须大于零"},
		{"unknown field", `{"file_path":"f","extra":1}`, "工具输入无效"},
		{"missing file", `{"file_path":"nope.txt"}`, "读取目标不存在"},
		{"directory", `{"file_path":"."}`, "读取目标不是普通文件"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			out := callTool(t, read, tc.input)
			if !out.IsError || !strings.Contains(out.Content, tc.wantSub) {
				t.Fatalf("输出 = %q，want 包含 %q", out.Content, tc.wantSub)
			}
		})
	}
	env, dir2 := airtightEnv(t, nil)
	mustWriteFile(t, filepath.Join(filepath.Dir(dir2), "outside.txt"), "x")
	out := callTool(t, &ReadTool{env: env}, `{"file_path":`+quote(filepath.Join(filepath.Dir(dir2), "outside.txt"))+`}`)
	if !out.IsError || !strings.Contains(out.Content, "outside the workspace") {
		t.Fatalf("工作区外读取应拒绝，得到 %q", out.Content)
	}
}

func TestWriteCreatesOverwritesAndPreserves(t *testing.T) {
	dir := canonDir(t)
	write := NewWrite(dir)

	out := callTool(t, write, `{"file_path":"nested/dir/new.txt","content":"hello\n"}`)
	if out.IsError {
		t.Fatalf("创建失败：%s", out.Content)
	}
	data, err := os.ReadFile(filepath.Join(dir, "nested", "dir", "new.txt"))
	if err != nil || string(data) != "hello\n" {
		t.Fatalf("落盘内容 = %q, err=%v", data, err)
	}
	if !strings.Contains(out.Content, "已原子创建") {
		t.Fatalf("输出 = %q", out.Content)
	}

	out = callTool(t, write, `{"file_path":"nested/dir/new.txt","content":"replaced\n"}`)
	if out.IsError || !strings.Contains(out.Content, "已原子覆盖") {
		t.Fatalf("覆盖输出 = %q", out.Content)
	}

	out = callTool(t, write, `{"file_path":"nested/dir/new.txt","content":"replaced\n"}`)
	if out.IsError || !strings.Contains(out.Content, "文件内容未变化") {
		t.Fatalf("未变化输出 = %q", out.Content)
	}
}

func TestWritePreservesCRLFStyle(t *testing.T) {
	dir := canonDir(t)
	path := filepath.Join(dir, "win.txt")
	mustWriteFile(t, path, "old1\r\nold2\r\n")
	out := callTool(t, NewWrite(dir), `{"file_path":"win.txt","content":"new1\nnew2\n"}`)
	if out.IsError {
		t.Fatalf("写入失败：%s", out.Content)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("回读失败：%v", err)
	}
	if string(data) != "new1\r\nnew2\r\n" {
		t.Fatalf("CRLF 应保留，得到 %q", data)
	}
}

func TestWritePreservesMode(t *testing.T) {
	dir := canonDir(t)
	path := filepath.Join(dir, "script.sh")
	mustWriteFile(t, path, "#!/bin/sh\n")
	if err := os.Chmod(path, 0o755); err != nil {
		t.Fatalf("chmod 失败：%v", err)
	}
	out := callTool(t, NewWrite(dir), `{"file_path":"script.sh","content":"#!/bin/sh\necho hi\n"}`)
	if out.IsError {
		t.Fatalf("写入失败：%s", out.Content)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat 失败：%v", err)
	}
	if info.Mode().Perm() != 0o755 {
		t.Fatalf("权限应保留为 0755，得到 %v", info.Mode().Perm())
	}
}

func TestWriteRejectsSymlinkAndTooLargeContent(t *testing.T) {
	dir := canonDir(t)
	target := filepath.Join(filepath.Dir(dir), "target.txt")
	mustWriteFile(t, target, "far\n")
	link := filepath.Join(dir, "link.txt")
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("符号链接不可用：%v", err)
	}
	out := callTool(t, NewWrite(dir), `{"file_path":"link.txt","content":"x"}`)
	if !out.IsError || !strings.Contains(out.Content, "符号链接") {
		t.Fatalf("符号链接写入应拒绝，得到 %q", out.Content)
	}

	env, err := NewEnvironment(canonDir(t), WithLimits(Limits{
		MaxReadLines:           10,
		MaxReadOutputBytes:     256,
		MaxSearchResults:       10,
		MaxSearchFileBytes:     1 << 20,
		MaxMutationFileBytes:   8,
		DefaultCommandTimeout:  time.Second,
		MaxCommandTimeout:      time.Second,
		MaxCommandPreviewBytes: 256,
	}))
	if err != nil {
		t.Fatalf("NewEnvironment 失败：%v", err)
	}
	out = callTool(t, &WriteTool{env: env}, `{"file_path":"big.txt","content":"0123456789ABC"}`)
	if !out.IsError || !strings.Contains(out.Content, "超过上限 8 字节") {
		t.Fatalf("超大内容应拒绝，得到 %q", out.Content)
	}
}

func TestEditExactReplaceSemantics(t *testing.T) {
	dir := canonDir(t)
	mustWriteFile(t, filepath.Join(dir, "code.txt"), "alpha beta gamma\nbeta again\n")
	edit := NewEdit(dir)

	out := callTool(t, edit, `{"file_path":"code.txt","old_string":"beta","new_string":"BETA"}`)
	if !out.IsError || !strings.Contains(out.Content, "精确匹配 2 次") {
		t.Fatalf("非唯一匹配应拒绝，得到 %q", out.Content)
	}

	out = callTool(t, edit, `{"file_path":"code.txt","old_string":"beta","new_string":"BETA","replace_all":true}`)
	if out.IsError || !strings.Contains(out.Content, "替换 2 处") {
		t.Fatalf("replace_all 输出 = %q", out.Content)
	}
	data, _ := os.ReadFile(filepath.Join(dir, "code.txt"))
	if string(data) != "alpha BETA gamma\nBETA again\n" {
		t.Fatalf("替换结果 = %q", data)
	}

	out = callTool(t, edit, `{"file_path":"code.txt","old_string":"alpha BETA gamma\nBETA again\n","new_string":"alpha BETA gamma\nBETA again\n"}`)
	if out.IsError || !strings.Contains(out.Content, "文件内容未变化") {
		t.Fatalf("无变化输出 = %q", out.Content)
	}

	out = callTool(t, edit, `{"file_path":"code.txt","old_string":"missing text","new_string":"x"}`)
	if !out.IsError || !strings.Contains(out.Content, "没有精确匹配") {
		t.Fatalf("未命中输出 = %q", out.Content)
	}

	out = callTool(t, edit, `{"file_path":"code.txt","old_string":"","new_string":"x"}`)
	if !out.IsError || !strings.Contains(out.Content, "old_string 不能为空") {
		t.Fatalf("空 old_string 输出 = %q", out.Content)
	}
}

func TestEditPreservesBOMAndPerms(t *testing.T) {
	dir := canonDir(t)
	path := filepath.Join(dir, "bom.txt")
	mustWriteFile(t, path, "\xEF\xBB\xBFkeep\n")
	if err := os.Chmod(path, 0o600); err != nil {
		t.Fatalf("chmod 失败：%v", err)
	}
	out := callTool(t, NewEdit(dir), `{"file_path":"bom.txt","old_string":"keep","new_string":"kept"}`)
	if out.IsError {
		t.Fatalf("编辑失败：%s", out.Content)
	}
	data, _ := os.ReadFile(path)
	if string(data) != "\xEF\xBB\xBFkept\n" {
		t.Fatalf("BOM 应保留，得到 %q", data)
	}
	info, _ := os.Stat(path)
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("权限应保留，得到 %v", info.Mode().Perm())
	}
}

func TestEditPromotesCRLFNeedles(t *testing.T) {
	dir := canonDir(t)
	path := filepath.Join(dir, "win.txt")
	mustWriteFile(t, path, "first\r\nsecond\r\n")
	out := callTool(t, NewEdit(dir), `{"file_path":"win.txt","old_string":"first\nsecond","new_string":"1st\n2nd"}`)
	if out.IsError {
		t.Fatalf("CRLF 提升匹配应成功：%s", out.Content)
	}
	data, _ := os.ReadFile(path)
	if string(data) != "1st\r\n2nd\r\n" {
		t.Fatalf("替换后应保持 CRLF，得到 %q", data)
	}
}

func TestEditRejectsBinaryAndSymlink(t *testing.T) {
	dir := canonDir(t)
	mustWriteFile(t, filepath.Join(dir, "bin.dat"), "a\x00b")
	out := callTool(t, NewEdit(dir), `{"file_path":"bin.dat","old_string":"a","new_string":"b"}`)
	if !out.IsError || !strings.Contains(out.Content, "NUL") {
		t.Fatalf("二进制编辑应拒绝，得到 %q", out.Content)
	}

	target := filepath.Join(filepath.Dir(dir), "t.txt")
	mustWriteFile(t, target, "text\n")
	link := filepath.Join(dir, "l.txt")
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("符号链接不可用：%v", err)
	}
	out = callTool(t, NewEdit(dir), `{"file_path":"l.txt","old_string":"text","new_string":"more"}`)
	if !out.IsError || !strings.Contains(out.Content, "符号链接") {
		t.Fatalf("符号链接编辑应拒绝，得到 %q", out.Content)
	}
}

// quote renders s as a JSON string literal for inline inputs.
func quote(s string) string {
	data, _ := json.Marshal(s)
	return string(data)
}
