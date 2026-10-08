package tools

import (
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

// testEnv builds an Environment over a fresh temp directory, optionally
// mutating the default limits.
func testEnv(t *testing.T, mutate func(*Limits)) *Environment {
	t.Helper()
	limits := DefaultLimits()
	if mutate != nil {
		mutate(&limits)
	}
	env, err := NewEnvironment(t.TempDir(), WithLimits(limits))
	if err != nil {
		t.Fatalf("NewEnvironment 失败：%v", err)
	}
	return env
}

func TestNewEnvironmentRejectsNonDirectory(t *testing.T) {
	if _, err := NewEnvironment(filepath.Join(t.TempDir(), "missing")); err == nil {
		t.Fatalf("不存在的目录应失败")
	}
	file := filepath.Join(t.TempDir(), "file.txt")
	if err := os.WriteFile(file, []byte("x"), 0o644); err != nil {
		t.Fatalf("写入夹具失败：%v", err)
	}
	if _, err := NewEnvironment(file); err == nil {
		t.Fatalf("文件路径作为工作目录应失败")
	}
}

func TestLimitsValidate(t *testing.T) {
	if err := DefaultLimits().Validate(); err != nil {
		t.Fatalf("默认限额应有效：%v", err)
	}
	broken := DefaultLimits()
	broken.MaxReadLines = 0
	if err := broken.Validate(); err == nil {
		t.Fatalf("零值限额应失败")
	}
	broken = DefaultLimits()
	broken.DefaultCommandTimeout = 2 * time.Hour
	broken.MaxCommandTimeout = time.Hour
	if err := broken.Validate(); err == nil {
		t.Fatalf("默认超时大于最大超时应失败")
	}
}

func TestResolvePathJoinsAndCleans(t *testing.T) {
	env := testEnv(t, nil)
	cases := []struct {
		raw  string
		want string
	}{
		{"a.txt", filepath.Join(env.WorkingDir(), "a.txt")},
		{"sub/../b.txt", filepath.Join(env.WorkingDir(), "b.txt")},
		{env.WorkingDir(), env.WorkingDir()},
	}
	for _, tc := range cases {
		got, err := env.ResolvePath(tc.raw)
		if err != nil {
			t.Fatalf("ResolvePath(%q) 失败：%v", tc.raw, err)
		}
		if got != tc.want {
			t.Fatalf("ResolvePath(%q) = %q，want %q", tc.raw, got, tc.want)
		}
	}
	if _, err := env.ResolvePath("   "); err == nil {
		t.Fatalf("空白路径应失败")
	}
}

func TestCheckWorkspacePath(t *testing.T) {
	env, _ := airtightEnv(t, nil)
	inside := filepath.Join(env.WorkingDir(), "sub", "file.txt")
	if err := env.CheckWorkspacePath(inside); err != nil {
		t.Fatalf("工作区内路径应通过：%v", err)
	}
	outsideDir, err := os.MkdirTemp("", "keencode-outside-*")
	if err != nil {
		t.Fatalf("MkdirTemp 失败：%v", err)
	}
	defer os.RemoveAll(outsideDir)
	outside := filepath.Join(outsideDir, "elsewhere.txt")
	if err := env.CheckWorkspacePath(outside); !errors.Is(err, ErrPathOutsideWorkspace) {
		t.Fatalf("工作区外路径应拒绝，得到 %v", err)
	}
	// An escape attempt through .. lands outside and is rejected.
	escape, err := env.ResolvePath("../escape.txt")
	if err != nil {
		t.Fatalf("ResolvePath 失败：%v", err)
	}
	if err := env.CheckWorkspacePath(escape); !errors.Is(err, ErrPathOutsideWorkspace) {
		t.Fatalf(".. 逃逸应拒绝，得到 %v", err)
	}
}

func TestCheckWorkspacePathExemptions(t *testing.T) {
	env := testEnv(t, nil) // default roots: OS temp + system directories
	if err := env.CheckWorkspacePath(os.TempDir()); err != nil {
		t.Fatalf("临时目录应豁免：%v", err)
	}
	if runtime.GOOS != "windows" {
		if _, err := os.Stat("/etc/hosts"); err == nil {
			if err := env.CheckWorkspacePath("/etc/hosts"); err != nil {
				t.Fatalf("系统目录应豁免：%v", err)
			}
		}
	}
}

func TestCheckWorkspacePathRejectsSymlinkEscape(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("符号链接需要特权")
	}
	env, _ := airtightEnv(t, nil)
	outsideDir, err := os.MkdirTemp("", "keencode-outside-*")
	if err != nil {
		t.Fatalf("MkdirTemp 失败：%v", err)
	}
	defer os.RemoveAll(outsideDir)
	outsideFile := filepath.Join(outsideDir, "secret.txt")
	if err := os.WriteFile(outsideFile, []byte("secret"), 0o644); err != nil {
		t.Fatalf("写入夹具失败：%v", err)
	}
	link := filepath.Join(env.WorkingDir(), "link.txt")
	if err := os.Symlink(outsideFile, link); err != nil {
		t.Fatalf("创建符号链接失败：%v", err)
	}
	resolved, err := env.ResolvePath(link)
	if err != nil {
		t.Fatalf("ResolvePath 失败：%v", err)
	}
	if err := env.CheckWorkspacePath(resolved); !errors.Is(err, ErrPathOutsideWorkspace) {
		t.Fatalf("符号链接逃逸应拒绝，得到 %v", err)
	}
}

func TestCheckCommandBoundary(t *testing.T) {
	env, workDir := airtightEnv(t, nil)
	_ = workDir
	outsideDir, err := os.MkdirTemp("", "keencode-outside-*")
	if err != nil {
		t.Fatalf("MkdirTemp 失败：%v", err)
	}
	defer os.RemoveAll(outsideDir)
	outside := filepath.Join(outsideDir, "elsewhere.txt")
	cases := []struct {
		name    string
		command string
		wantErr bool
	}{
		{"plain command", "go test ./...", false},
		{"relative path", "cat README.md", false},
		{"escape token", "cat ../secrets.txt", true},
		{"absolute outside", "cat " + outside, true},
		{"parent of workspace", "cat ../elsewhere.txt", true},
		{"quoted outside", `cat "` + outside + `"`, true},
		{"tmp is allowed", "ls /tmp", false},
		{"tmp subpath", "touch /tmp/keencode-test-marker", false},
		{"dev exempt", "dd if=/dev/zero of=/dev/null count=1", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := env.CheckCommandBoundary(tc.command)
			if tc.wantErr && err == nil {
				t.Fatalf("命令 %q 应拒绝", tc.command)
			}
			if !tc.wantErr && err != nil {
				t.Fatalf("命令 %q 应通过，得到 %v", tc.command, err)
			}
		})
	}
}

func TestBindInvocationRejectsMismatch(t *testing.T) {
	env := testEnv(t, nil)
	if err := env.BindInvocation(Invocation{}); err != nil {
		t.Fatalf("空 WorkDir 应放行：%v", err)
	}
	if err := env.BindInvocation(Invocation{WorkDir: env.WorkingDir()}); err != nil {
		t.Fatalf("一致 WorkDir 应放行：%v", err)
	}
	err := env.BindInvocation(Invocation{WorkDir: t.TempDir()})
	if err == nil || !contains(err.Error(), "不一致") {
		t.Fatalf("不一致 WorkDir 应拒绝，得到 %v", err)
	}
}

func TestWithAllowedRootsWidensSandbox(t *testing.T) {
	// Nested NewEnvironment: the extra root must exist before construction.
	extra := t.TempDir()
	base := t.TempDir()
	env, err := NewEnvironment(base, WithAllowedRoots(extra))
	if err != nil {
		t.Fatalf("NewEnvironment 失败：%v", err)
	}
	if err := env.CheckWorkspacePath(filepath.Join(extra, "f.txt")); err != nil {
		t.Fatalf("允许根内路径应通过：%v", err)
	}
	if _, err := NewEnvironment(base, WithAllowedRoots(filepath.Join(base, "missing"))); err == nil {
		t.Fatalf("不存在的允许根应失败")
	}
}

// contains is a small alias keeping table bodies terse.
func contains(haystack, needle string) bool {
	return strings.Contains(haystack, needle)
}
