package workspace

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
)

type Changes struct {
	Unstaged  string
	Staged    string
	Untracked []string
}

type cappedOutput struct{ bytes.Buffer }

func (w *cappedOutput) Write(p []byte) (int, error) {
	if w.Len()+len(p) > 2<<20 {
		return 0, errors.New("Git输出超过2 MiB，请在终端查看")
	}
	return w.Buffer.Write(p)
}

// Diff shows both working-tree and index changes (including unborn repos).
// Untracked files are listed separately, since git diff doesn't include them.
func Diff(ctx context.Context, dir string) (Changes, error) {
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	run := func(args ...string) (string, error) {
		cmd := exec.CommandContext(ctx, "git", append([]string{"--no-pager", "-C", dir}, args...)...)
		cmd.Env = append(os.Environ(), "GIT_TERMINAL_PROMPT=0", "GIT_OPTIONAL_LOCKS=0")
		out, stderr := &cappedOutput{}, &cappedOutput{}
		cmd.Stdout, cmd.Stderr = out, stderr
		if err := cmd.Run(); err != nil {
			if ctx.Err() != nil {
				return "", ctx.Err()
			}
			return "", fmt.Errorf("Git读取失败：%s (%w)", strings.TrimSpace(stderr.String()), err)
		}
		return out.String(), nil
	}
	var result Changes
	var err error
	result.Unstaged, err = run("diff", "--no-ext-diff", "--no-textconv", "--no-color", "--", ".")
	if err != nil {
		return result, err
	}
	result.Staged, err = run("diff", "--cached", "--no-ext-diff", "--no-textconv", "--no-color", "--", ".")
	if err != nil {
		return result, err
	}
	untracked, err := run("ls-files", "--others", "--exclude-standard", "-z", "--", ".")
	if err != nil {
		return result, err
	}
	if untracked != "" {
		result.Untracked = strings.Split(strings.TrimSuffix(untracked, "\x00"), "\x00")
	}
	return result, nil
}
