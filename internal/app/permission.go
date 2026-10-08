package app

import (
	"context"
	"fmt"

	"github.com/egoist/mygo"

	"keencode/internal/agent"
	"keencode/internal/config"
)

// The Authorize bridge (docs/go-migration.md §5.7 permission.go, §6.8):
// the agent loop calls it from the turn goroutine before a side-effect
// tool executes. Read-only tools never reach it (the loop classifies by
// Effect first). The bridge resolves the app-level policy, the
// session-scoped allowances, and — failing those — the native three-option
// dialog: 允许一次 (Enter) / 本会话允许 / 拒绝 (Esc). Every failure path
// denies: the gate fails closed (docs/go-migration.md D7).

// authorize implements agent.Dependencies.Authorize.
func (r *runnerSource) authorize(ctx context.Context, req agent.PermissionRequest) (bool, error) {
	switch r.settings.Loaded().ToolPermissionPolicy {
	case config.ToolPermissionAllowAll:
		return true, nil
	case config.ToolPermissionReadOnly:
		return false, nil
	}
	if r.sessionAllows(req.SessionID, req.ToolName) {
		return true, nil
	}
	// The turn was stopped while queued behind another dialog: answer
	// without surfacing UI.
	if err := ctx.Err(); err != nil {
		return false, nil
	}
	choice, err := r.askPermission(req)
	if err != nil {
		return false, err // fail closed
	}
	if choice == permissionSession {
		r.allowForSession(req.SessionID, req.ToolName)
	}
	return choice != permissionDeny, nil
}

// permissionChoice is the outcome of the permission prompt.
type permissionChoice uint8

const (
	permissionDeny    permissionChoice = iota // 拒绝
	permissionOnce                            // 允许一次
	permissionSession                         // 本会话允许
)

// askPermission shows the native dialog and maps the answer. It runs on
// the turn goroutine; mygo forwards the blocking call to the main thread
// and waits (mygo.go:44-49). A window that has not been bound yet (early
// startup) passes no parent, matching Dialog.Parent's nil tolerance. The
// dialog error surfaces to authorize, which denies and reports it.
func (r *runnerSource) askPermission(req agent.PermissionRequest) (permissionChoice, error) {
	ask := r.ask
	result, err := ask(mygo.MessageOptions{
		Parent:  r.win.Load(),
		Type:    mygo.MessageQuestion,
		Title:   PermissionTitle,
		Message: permissionBody(req),
		Detail:  fmt.Sprintf("%s：%s", PermissionDetailPrefix, requestJSON(req.Input)),
		Buttons: []string{
			PermissionAllowOnce,    // Enter, 差异 9
			PermissionAllowSession, // scope: this session only
			PermissionDeny,         // Esc
		},
		DefaultButton: 0,
		CancelButton:  2,
	})
	if err != nil {
		return permissionDeny, err
	}
	switch result.Button {
	case 0:
		return permissionOnce, nil
	case 1:
		return permissionSession, nil
	default:
		return permissionDeny, nil
	}
}

// permissionBody renders the dialog's main line: the loop's display-safe
// summary already names the tool and the first command/path field.
func permissionBody(req agent.PermissionRequest) string {
	if req.Summary != "" {
		return req.Summary
	}
	return PermissionMessage
}

// requestJSON renders the raw arguments for the detail area, falling back
// to a placeholder when empty.
func requestJSON(input []byte) string {
	if len(input) == 0 {
		return "{}"
	}
	return string(input)
}
