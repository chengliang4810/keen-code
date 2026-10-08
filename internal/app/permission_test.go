package app

import (
	"context"
	"testing"

	"github.com/egoist/mygo"

	"keencode/internal/agent"
	"keencode/internal/config"
)

// The Authorize bridge: policy shortcuts, the three-option dialog, the
// session-scoped allowance, and fail-closed error paths. The dialog seam is
// injected, so nothing here opens a native window.

// permRequest builds an authorization request for one tool.
func permRequest(session, tool string) agent.PermissionRequest {
	return agent.PermissionRequest{
		SessionID: session,
		TurnID:    "turn-1",
		CallID:    "call-1",
		ToolName:  tool,
		Input:     []byte(`{"command":"rm -rf build"}`),
		Summary:   "工具 Bash 请求执行副作用操作：rm -rf build",
	}
}

// askStub records the dialogs it was asked to show and answers with a
// fixed button index.
type askStub struct {
	calls   int
	buttons []mygo.MessageOptions
	answer  int
	err     error
}

// ask implements the seam.
func (s *askStub) ask(opts mygo.MessageOptions) (mygo.MessageResult, error) {
	s.calls++
	s.buttons = append(s.buttons, opts)
	if s.err != nil {
		return mygo.MessageResult{}, s.err
	}
	return mygo.MessageResult{Button: s.answer}, nil
}

// TestAuthorizePolicyShortcuts checks the settings-level gates, which never
// show a dialog.
func TestAuthorizePolicyShortcuts(t *testing.T) {
	cases := []struct {
		name   string
		policy config.ToolPermissionPolicy
		want   bool
	}{
		{"allow-all", config.ToolPermissionAllowAll, true},
		{"read-only", config.ToolPermissionReadOnly, false},
	}
	for _, tc := range cases {
		svcs, _ := newTestHarness(t, nil, nil)
		if err := svcs.Settings.SetToolPermissionPolicy(tc.policy); err != nil {
			t.Fatalf("%s: save policy: %v", tc.name, err)
		}
		ask := &askStub{answer: 2}
		svcs.Sessions.runner.setAsk(ask.ask)
		allowed, err := svcs.Sessions.runner.authorize(context.Background(), permRequest("s", "Bash"))
		if err != nil || allowed != tc.want {
			t.Errorf("%s: authorize = %v, %v; want %v, nil", tc.name, allowed, err, tc.want)
		}
		if ask.calls != 0 {
			t.Errorf("%s: policy shortcut opened %d dialogs", tc.name, ask.calls)
		}
	}
}

// TestAuthorizeDialogChoices drives the ask policy through all three
// buttons and the session allowance.
func TestAuthorizeDialogChoices(t *testing.T) {
	cases := []struct {
		name       string
		button     int
		want       bool
		wantCalls  int // dialogs across both asks
		wantSticky bool
	}{
		{"deny", 2, false, 2, false},
		{"allow once", 0, true, 2, false},
		{"allow session", 1, true, 1, true},
	}
	for _, tc := range cases {
		svcs, _ := newTestHarness(t, nil, nil)
		ask := &askStub{answer: tc.button}
		svcs.Sessions.runner.setAsk(ask.ask)

		first, err := svcs.Sessions.runner.authorize(context.Background(), permRequest("s", "Bash"))
		if err != nil || first != tc.want {
			t.Errorf("%s: first authorize = %v, %v; want %v, nil", tc.name, first, err, tc.want)
		}
		// A second identical request only re-prompts without the sticky
		// session allowance.
		second, err := svcs.Sessions.runner.authorize(context.Background(), permRequest("s", "Bash"))
		if err != nil || second != tc.want {
			t.Errorf("%s: second authorize = %v, %v; want %v, nil", tc.name, second, err, tc.want)
		}
		if ask.calls != tc.wantCalls {
			t.Errorf("%s: dialogs = %d, want %d", tc.name, ask.calls, tc.wantCalls)
		}
		if got := svcs.Sessions.runner.sessionAllows("s", "Bash"); got != tc.wantSticky {
			t.Errorf("%s: session allowance = %v, want %v", tc.name, got, tc.wantSticky)
		}

		// The dialog contract: three options, Enter=允许一次, Esc=拒绝.
		if ask.calls > 0 {
			opts := ask.buttons[0]
			if len(opts.Buttons) != 3 {
				t.Fatalf("%s: buttons = %q", tc.name, opts.Buttons)
			}
			if opts.Buttons[0] != PermissionAllowOnce || opts.Buttons[1] != PermissionAllowSession || opts.Buttons[2] != PermissionDeny {
				t.Errorf("%s: buttons = %q", tc.name, opts.Buttons)
			}
			if opts.DefaultButton != 0 || opts.CancelButton != 2 {
				t.Errorf("%s: default=%d cancel=%d, want 0/2", tc.name, opts.DefaultButton, opts.CancelButton)
			}
			if opts.Message != "工具 Bash 请求执行副作用操作：rm -rf build" {
				t.Errorf("%s: dialog body = %q", tc.name, opts.Message)
			}
		}
	}
}

// TestAuthorizeSessionScopeIsolation checks that an allowance for one
// session (or tool) does not leak to another.
func TestAuthorizeSessionScopeIsolation(t *testing.T) {
	svcs, _ := newTestHarness(t, nil, nil)
	svcs.Sessions.runner.allowForSession("s1", "Bash")
	if !svcs.Sessions.runner.sessionAllows("s1", "Bash") {
		t.Fatal("the recorded allowance is missing")
	}
	if svcs.Sessions.runner.sessionAllows("s2", "Bash") {
		t.Error("the allowance leaked to another session")
	}
	if svcs.Sessions.runner.sessionAllows("s1", "Write") {
		t.Error("the allowance leaked to another tool")
	}
	svcs.Sessions.runner.forgetSession("s1")
	if svcs.Sessions.runner.sessionAllows("s1", "Bash") {
		t.Error("forgetSession left the allowance behind")
	}
}

// TestAuthorizeFailsClosedOnDialogError checks the error path.
func TestAuthorizeFailsClosedOnDialogError(t *testing.T) {
	svcs, _ := newTestHarness(t, nil, nil)
	ask := &askStub{err: context.DeadlineExceeded}
	svcs.Sessions.runner.setAsk(ask.ask)
	allowed, err := svcs.Sessions.runner.authorize(context.Background(), permRequest("s", "Bash"))
	if err == nil {
		t.Fatal("the dialog error must surface")
	}
	if allowed {
		t.Error("a dialog failure must deny")
	}
}

// TestAuthorizeCancelledContextNeverPrompts checks the pre-dialog
// cancellation guard.
func TestAuthorizeCancelledContextNeverPrompts(t *testing.T) {
	svcs, _ := newTestHarness(t, nil, nil)
	ask := &askStub{answer: 0}
	svcs.Sessions.runner.setAsk(ask.ask)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	allowed, err := svcs.Sessions.runner.authorize(ctx, permRequest("s", "Bash"))
	if err != nil || allowed {
		t.Errorf("cancelled authorize = %v, %v; want false, nil", allowed, err)
	}
	if ask.calls != 0 {
		t.Errorf("a cancelled turn opened %d dialogs", ask.calls)
	}
}
