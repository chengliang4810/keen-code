//go:build !unix

package tools

import "os/exec"

// configureProcessGroup has no portable process-group equivalent outside
// Unix; the child is killed directly on timeout instead of via its group.
func configureProcessGroup(cmd *exec.Cmd) {}

// killProcessTree kills the child process directly (no tree semantics
// without a process group).
func killProcessTree(cmd *exec.Cmd) {
	if cmd.Process != nil {
		_ = cmd.Process.Kill()
	}
}
