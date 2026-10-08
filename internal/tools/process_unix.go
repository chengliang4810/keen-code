//go:build unix

package tools

import (
	"os/exec"
	"syscall"
)

// configureProcessGroup puts the child into its own process group so a
// timeout or cancellation can kill the whole tree, not just the shell
// leader (command.rs:1033-1062 via command-group; Go uses setpgid).
func configureProcessGroup(cmd *exec.Cmd) {
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.Setpgid = true
}

// killProcessTree force-kills the child's whole process group and ignores
// the ESRCH of an already-dead group (command.rs:1158-1182).
func killProcessTree(cmd *exec.Cmd) {
	if cmd.Process == nil {
		return
	}
	_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
	_ = cmd.Process.Kill()
}
