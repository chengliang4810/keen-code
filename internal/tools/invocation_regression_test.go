package tools

import (
	"os"
	"path/filepath"
	"testing"
)

func TestInvocationAcceptsSameWorkspaceThroughSymlink(t *testing.T) {
	dir := t.TempDir()
	alias := filepath.Join(t.TempDir(), "workspace-link")
	if err := os.Symlink(dir, alias); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	env, err := NewEnvironment(alias)
	if err != nil {
		t.Fatal(err)
	}
	if err := env.BindInvocation(Invocation{WorkDir: alias}); err != nil {
		t.Fatalf("same canonical workspace must be accepted: %v", err)
	}
	other := filepath.Join(t.TempDir(), "other-link")
	if err := os.Symlink(t.TempDir(), other); err != nil {
		t.Fatal(err)
	}
	if err := env.BindInvocation(Invocation{WorkDir: other}); err == nil {
		t.Fatal("different canonical workspace must still be rejected")
	}
}
