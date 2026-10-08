package tools

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

// ErrPathOutsideWorkspace is wrapped by every sandbox rejection; hosts and
// tests can classify failures with errors.Is.
var ErrPathOutsideWorkspace = errors.New("path_outside_workspace")

// Limits are the deterministic per-call resource ceilings shared by the
// file, search and shell tools (environment.rs:36-73, image limits dropped
// because v1 Read returns text only).
type Limits struct {
	// MaxReadLines caps the limit argument of one Read call.
	MaxReadLines int
	// MaxReadOutputBytes caps one Read result including header, line
	// numbers and the continuation marker.
	MaxReadOutputBytes int
	// MaxSearchResults caps the result count of one Glob or Grep call.
	MaxSearchResults int
	// MaxSearchFileBytes caps the size of one file Grep loads into memory.
	MaxSearchFileBytes int64
	// MaxMutationFileBytes caps the file size Edit or Write will load and
	// atomically replace.
	MaxMutationFileBytes int64
	// DefaultCommandTimeout is the Bash timeout when the input omits one.
	DefaultCommandTimeout time.Duration
	// MaxCommandTimeout is the largest Bash timeout a call may request.
	MaxCommandTimeout time.Duration
	// MaxCommandPreviewBytes is the per-stream head+tail preview budget of
	// a Bash result; the full stream is spilled to an artifact file when
	// the preview is truncated.
	MaxCommandPreviewBytes int
}

// DefaultLimits returns the conservative desktop-session defaults
// (environment.rs:58-73).
func DefaultLimits() Limits {
	return Limits{
		MaxReadLines:           20_000,
		MaxReadOutputBytes:     32 << 10,
		MaxSearchResults:       10_000,
		MaxSearchFileBytes:     16 << 20,
		MaxMutationFileBytes:   64 << 20,
		DefaultCommandTimeout:  120 * time.Second,
		MaxCommandTimeout:      time.Hour,
		MaxCommandPreviewBytes: 16 << 10,
	}
}

// Validate reports limits that would disable a bound entirely.
func (l Limits) Validate() error {
	zero := l.MaxReadLines <= 0 || l.MaxReadOutputBytes <= 0 || l.MaxSearchResults <= 0 ||
		l.MaxSearchFileBytes <= 0 || l.MaxMutationFileBytes <= 0 ||
		l.DefaultCommandTimeout <= 0 || l.MaxCommandTimeout <= 0 || l.MaxCommandPreviewBytes <= 0
	if zero {
		return fmt.Errorf("tools: 文件与搜索工具的资源上限必须全部大于零")
	}
	if l.DefaultCommandTimeout > l.MaxCommandTimeout {
		return fmt.Errorf("tools: 默认命令超时不能大于最大命令超时")
	}
	return nil
}

// Environment is the shared binding of one canonical work directory, the
// resource limits and the workspace sandbox (environment.rs:128-152). One
// session builds one Environment and passes it to every tool so path
// resolution and the sandbox boundary agree across tools.
type Environment struct {
	workDir      string
	tempRoot     string
	systemRoots  []string
	allowedRoots []string
	limits       Limits
}

// EnvironmentOption customizes an Environment at construction time.
type EnvironmentOption func(*Environment) error

// WithLimits replaces the default resource limits.
func WithLimits(limits Limits) EnvironmentOption {
	return func(e *Environment) error {
		if err := limits.Validate(); err != nil {
			return err
		}
		e.limits = limits
		return nil
	}
}

// WithAllowedRoots widens the sandbox with additional directory roots the
// host explicitly trusts (for example a session artifacts directory). Every
// root must exist and be a directory; symlinks are resolved.
func WithAllowedRoots(roots ...string) EnvironmentOption {
	return func(e *Environment) error {
		for _, root := range roots {
			canonical, err := canonicalExisting(root)
			if err != nil {
				return fmt.Errorf("tools: 无法解析允许根目录 %s：%w", root, err)
			}
			info, err := os.Stat(canonical)
			if err != nil || !info.IsDir() {
				return fmt.Errorf("tools: 允许根目录 %s 不是目录", root)
			}
			e.allowedRoots = append(e.allowedRoots, canonical)
		}
		return nil
	}
}

// WithTempRoot pins the scratch root (command output artifacts and the
// /tmp mapping of the command boundary check) to an explicit directory.
// It exists for hosts that want a project-local scratch space and for
// tests that need an airtight sandbox; unset, the OS temp directory is
// used.
func WithTempRoot(dir string) EnvironmentOption {
	return func(e *Environment) error {
		canonical, err := canonicalExisting(dir)
		if err != nil {
			return fmt.Errorf("tools: 无法解析临时根目录 %s：%w", dir, err)
		}
		info, err := os.Stat(canonical)
		if err != nil || !info.IsDir() {
			return fmt.Errorf("tools: 临时根目录 %s 不是目录", dir)
		}
		e.tempRoot = canonical
		return nil
	}
}

// NewEnvironment resolves and canonicalizes the work directory and prepares
// the sandbox roots (workspace, temp directory, and — mirroring the old
// stack guard roots — the standard system directories).
func NewEnvironment(workDir string, opts ...EnvironmentOption) (*Environment, error) {
	canonical, err := canonicalExisting(workDir)
	if err != nil {
		return nil, fmt.Errorf("tools: 无法解析 Session 工作目录：%w", err)
	}
	info, err := os.Stat(canonical)
	if err != nil {
		return nil, fmt.Errorf("tools: 无法解析 Session 工作目录：%w", err)
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("tools: Session 工作目录不是目录")
	}
	env := &Environment{
		workDir: canonical,
		limits:  DefaultLimits(),
	}
	for _, root := range systemRootNames() {
		canonicalRoot, err := canonicalExisting(root)
		if err != nil {
			continue // a missing system directory simply grants nothing
		}
		if info, err := os.Stat(canonicalRoot); err == nil && info.IsDir() {
			env.systemRoots = append(env.systemRoots, canonicalRoot)
		}
	}
	tempRoot, err := canonicalExisting(os.TempDir())
	if err != nil {
		tempRoot = os.TempDir()
	}
	env.tempRoot = tempRoot
	for _, opt := range opts {
		if err := opt(env); err != nil {
			return nil, err
		}
	}
	return env, nil
}

// systemRootNames returns the read-exempt system directories of the
// sandbox (environment.rs:222-236): the workspace keeps write access, but
// reading toolchains and configuration from the system tree stays legal.
func systemRootNames() []string {
	if runtime.GOOS == "windows" {
		var roots []string
		for _, name := range []string{"SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData"} {
			if value := os.Getenv(name); value != "" {
				roots = append(roots, value)
			}
		}
		return roots
	}
	return []string{"/usr", "/bin", "/etc", "/opt"}
}

// WorkingDir returns the canonical absolute work directory.
func (e *Environment) WorkingDir() string { return e.workDir }

// Limits returns the configured resource ceilings.
func (e *Environment) Limits() Limits { return e.limits }

// BindInvocation fails closed when the host supplies a per-call work
// directory that disagrees with the environment the tool was constructed
// with. Empty means "use the constructed directory".
func (e *Environment) BindInvocation(inv Invocation) error {
	if inv.WorkDir == "" {
		return nil
	}
	// The environment resolves symlinks at construction; compare the host
	// binding in the same namespace (notably /tmp → /private/tmp on macOS).
	supplied, err := canonicalExisting(inv.WorkDir)
	if err != nil {
		return fmt.Errorf("tools: 调用工作目录 %s 无法解析：%w", inv.WorkDir, err)
	}
	if samePath(filepath.Clean(supplied), e.workDir) {
		return nil
	}
	return fmt.Errorf("tools: 调用工作目录 %s 与工具绑定的工作目录 %s 不一致", displayPath(supplied), displayPath(e.workDir))
}

// ResolvePath turns a non-empty absolute or work-directory-relative path
// into its canonical absolute form (environment.rs:532-551): symlinks in
// every existing ancestor are resolved so the sandbox check below sees the
// real location the OS would touch.
func (e *Environment) ResolvePath(raw string) (string, error) {
	if strings.TrimSpace(raw) == "" {
		return "", fmt.Errorf("tools: 路径不能为空")
	}
	candidate := raw
	if !filepath.IsAbs(candidate) {
		candidate = filepath.Join(e.workDir, candidate)
	}
	// Lexical absoluteness only, like the old stack's std::path::absolute
	// (environment.rs:532-551): symlinks stay visible to the symlink
	// rejection of Edit and Write; the sandbox containment check below
	// resolves them separately.
	return filepath.Clean(candidate), nil
}

// CheckWorkspacePath rejects paths outside the sandbox roots: the work
// directory, the temp directory (scratch space for tool output artifacts),
// the exempted system directories, and any host-granted root. The check
// compares canonical forms, so a symlink inside the workspace pointing
// outside is rejected (environment.rs:253-275).
func (e *Environment) CheckWorkspacePath(path string) error {
	if e.pathWithinRoots(path) {
		return nil
	}
	return fmt.Errorf("%w: refused: %s is outside the workspace %s. The harness keeps tool access inside the workspace; use a path under it, or ask the user to widen the boundary.",
		ErrPathOutsideWorkspace, displayPath(path), displayPath(e.workDir))
}

// pathWithinRoots reports whether the canonical form of path equals or sits
// below one of the sandbox roots.
func (e *Environment) pathWithinRoots(path string) bool {
	candidate := normalizeGuardPath(canonicalPath(path))
	roots := make([]string, 0, len(e.allowedRoots)+len(e.systemRoots)+2)
	roots = append(roots, e.workDir, e.tempRoot)
	roots = append(roots, e.systemRoots...)
	roots = append(roots, e.allowedRoots...)
	for _, root := range roots {
		guard := normalizeGuardPath(root)
		if candidate == guard || strings.HasPrefix(candidate, guard+string(os.PathSeparator)) {
			return true
		}
	}
	return false
}

// CheckCommandBoundary inspects the path-shaped tokens of a shell command
// and rejects the ones that verifiably point outside the sandbox
// (environment.rs:282-303). This is a boundary check, not a shell parser:
// a command that smuggles a path past the token scan (inside `python -c`
// text, generated at runtime) remains the model's own responsibility.
func (e *Environment) CheckCommandBoundary(command string) error {
	for _, token := range strings.Fields(command) {
		trimmed := strings.Trim(token, `"'`)
		mapped, ok := e.mapBoundaryToken(trimmed)
		if !ok {
			continue
		}
		if !e.pathWithinRoots(mapped) {
			return fmt.Errorf("%w: refused: command touches %q which is outside the workspace %s. The harness keeps command access inside the workspace; use paths under it, or ask the user to widen the boundary.",
				ErrPathOutsideWorkspace, trimmed, displayPath(e.workDir))
		}
	}
	return nil
}

// mapBoundaryToken maps one command token with a verifiable location shape
// to an absolute path; shapes the boundary cannot verify return false and
// pass (environment.rs:312-378).
func (e *Environment) mapBoundaryToken(token string) (string, bool) {
	if token == "" {
		return "", false
	}
	if len(token) >= 2 && token[1] == ':' { // Windows drive-absolute (C:\...)
		return token, true
	}
	if strings.HasPrefix(token, `\\`) { // Windows UNC
		return token, true
	}
	if token == ".." || strings.HasPrefix(token, "../") || strings.HasPrefix(token, `..\`) {
		return filepath.Clean(filepath.Join(e.workDir, token)), true
	}
	if strings.HasPrefix(token, "~") {
		rest := strings.TrimLeft(strings.TrimPrefix(token, "~"), `/\`)
		home, err := os.UserHomeDir()
		if err != nil {
			return "", false
		}
		if rest == "" {
			return home, true
		}
		return filepath.Join(home, filepath.FromSlash(rest)), true
	}
	if !strings.HasPrefix(token, "/") {
		return "", false
	}
	for _, exempt := range []string{"/dev", "/proc", "/sys", "/nul"} {
		if token == exempt || strings.HasPrefix(token, exempt+"/") {
			return "", false
		}
	}
	if token == "/tmp" || strings.HasPrefix(token, "/tmp/") {
		rest := strings.TrimPrefix(token, "/tmp")
		return filepath.Join(e.tempRoot, filepath.FromSlash(strings.TrimLeft(rest, "/"))), true
	}
	if token == "/var/tmp" || strings.HasPrefix(token, "/var/tmp/") {
		rest := strings.TrimPrefix(token, "/var/tmp")
		return filepath.Join(e.tempRoot, filepath.FromSlash(strings.TrimLeft(rest, "/"))), true
	}
	return token, true
}

// ArtifactDir returns the directory full command output is spilled to; it
// sits inside the temp sandbox root so the model can Read spilled output
// back through the same sandbox.
func (e *Environment) ArtifactDir() string {
	return filepath.Join(e.tempRoot, "keencode", "tool-output")
}

// canonicalExisting resolves symlinks of an existing path.
func canonicalExisting(path string) (string, error) {
	absolute, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return filepath.EvalSymlinks(absolute)
}

// canonicalPath resolves symlinks through the deepest existing ancestor and
// leaves the not-yet-existing tail untouched (environment.rs:106-116).
func canonicalPath(path string) string {
	if resolved, err := filepath.EvalSymlinks(path); err == nil {
		return resolved
	}
	dir, file := filepath.Split(path)
	if dir == "" {
		return path
	}
	resolvedDir, err := filepath.EvalSymlinks(filepath.Clean(dir))
	if err != nil {
		resolvedDir = canonicalPath(filepath.Clean(dir))
	}
	return filepath.Join(resolvedDir, file)
}

// normalizeGuardPath prepares a path for containment comparison:
// case-folded and backslash-normalized on Windows, unchanged elsewhere
// (environment.rs:119-126).
func normalizeGuardPath(path string) string {
	if runtime.GOOS == "windows" {
		return strings.ToLower(strings.ReplaceAll(path, "/", "\\"))
	}
	return path
}

// samePath compares two already-clean paths the way the local filesystem
// compares them.
func samePath(left, right string) bool {
	return normalizeGuardPath(left) == normalizeGuardPath(right)
}

// displayPath renders a path with forward slashes for stable model output
// (environment.rs:597-599).
func displayPath(path string) string {
	return strings.ReplaceAll(path, "\\", "/")
}
