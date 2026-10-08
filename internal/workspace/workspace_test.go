package workspace

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestFileInspectionIsConfinedAndBounded(t *testing.T) {
	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, "src"), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "note.txt"), []byte("中文预览"), 0600); err != nil {
		t.Fatal(err)
	}
	entries, err := List(root, ".")
	if err != nil || len(entries) != 2 || !entries[0].Directory {
		t.Fatalf("entries=%+v err=%v", entries, err)
	}
	text, err := Preview(root, "note.txt")
	if err != nil || text != "中文预览" {
		t.Fatalf("preview=%q err=%v", text, err)
	}
	outside := filepath.Join(t.TempDir(), "private.txt")
	if err := os.WriteFile(outside, []byte("private"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(outside, filepath.Join(root, "escape")); err == nil {
		if _, err := Preview(root, "escape"); err == nil {
			t.Fatal("symlink escaped project")
		}
	}
	if _, err := Preview(root, "../private.txt"); err == nil {
		t.Fatal("traversal accepted")
	}
	if _, err := Preview(root, "src"); err == nil {
		t.Fatal("directory preview accepted")
	}
	for name, data := range map[string][]byte{"binary": {0, 1, 2}, "large": make([]byte, MaxPreviewBytes+1)} {
		if err := os.WriteFile(filepath.Join(root, name), data, 0600); err != nil {
			t.Fatal(err)
		}
		if _, err := Preview(root, name); err == nil {
			t.Fatalf("%s accepted", name)
		}
	}
}

func git(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", append([]string{"-C", dir}, args...)...)
	if output, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git %v: %v %s", args, err, output)
	}
}

func TestDiffIncludesIndexWorktreeAndUntracked(t *testing.T) {
	dir := t.TempDir()
	git(t, dir, "init", "-q")
	write := func(name, text string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(dir, name), []byte(text), 0600); err != nil {
			t.Fatal(err)
		}
	}
	write("note.txt", "index text\n")
	git(t, dir, "add", "note.txt")
	write("note.txt", "worktree text\n")
	write("new file.txt", "untracked\n")
	changes, err := Diff(context.Background(), dir)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(changes.Staged, "+index text") || !strings.Contains(changes.Unstaged, "+worktree text") || len(changes.Untracked) != 1 || changes.Untracked[0] != "new file.txt" {
		t.Fatalf("unborn repo diff=%+v", changes)
	}
	git(t, dir, "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "initial")
	changes, err = Diff(context.Background(), dir)
	if err != nil || changes.Staged != "" || !strings.Contains(changes.Unstaged, "+worktree text") {
		t.Fatalf("committed diff=%+v err=%v", changes, err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := Diff(ctx, dir); err == nil {
		t.Fatal("cancelled diff succeeded")
	}
	if _, err := Diff(context.Background(), t.TempDir()); err == nil {
		t.Fatal("non-repo silently shown clean")
	}
}
