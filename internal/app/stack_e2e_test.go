package app

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
	"keencode/internal/config"
	"keencode/internal/runtime"
)

// These scenarios use the production service assembly, HTTP/SSE adapters,
// agent loop, actual tools and disk journal. Only the model endpoint and
// native permission/picker answers are controlled; no scripted runner.
type e2eCall struct{ name, args string }

func e2eStream(w http.ResponseWriter, protocol config.Protocol, call *e2eCall, index int) {
	w.Header().Set("Content-Type", "text/event-stream")
	data := func(v any) { b, _ := json.Marshal(v); fmt.Fprintf(w, "data: %s\n\n", b) }
	if protocol == config.ProtocolMessages {
		data(map[string]any{"type": "message_start", "message": map[string]any{"id": fmt.Sprint(index), "role": "assistant", "content": []any{}}})
		block := map[string]any{"type": "text", "text": "STACK_OK"}
		reason := "end_turn"
		if call != nil {
			var args any
			_ = json.Unmarshal([]byte(call.args), &args)
			block = map[string]any{"type": "tool_use", "id": fmt.Sprintf("call-%d", index), "name": call.name, "input": args}
			reason = "tool_use"
		}
		data(map[string]any{"type": "content_block_start", "index": 0, "content_block": block})
		data(map[string]any{"type": "content_block_stop", "index": 0})
		data(map[string]any{"type": "message_delta", "delta": map[string]any{"stop_reason": reason}, "usage": map[string]any{"output_tokens": 1}})
		data(map[string]any{"type": "message_stop"})
	} else {
		delta := map[string]any{"content": "STACK_OK"}
		reason := "stop"
		if call != nil {
			delta = map[string]any{"tool_calls": []any{map[string]any{"index": 0, "id": fmt.Sprintf("call-%d", index), "type": "function", "function": map[string]any{"name": call.name, "arguments": call.args}}}}
			reason = "tool_calls"
		}
		data(map[string]any{"choices": []any{map[string]any{"index": 0, "delta": delta, "finish_reason": nil}}})
		data(map[string]any{"choices": []any{map[string]any{"index": 0, "delta": map[string]any{}, "finish_reason": reason}}})
		fmt.Fprint(w, "data: [DONE]\n\n")
	}
}

func e2eApp(t *testing.T, endpoint string, protocol config.Protocol, choice int) (*Services, *App, *ui.Tester, string) {
	t.Helper()
	root, dir := t.TempDir(), t.TempDir()
	// Exercise an alias even on hosts whose temp directory is canonical.
	alias := filepath.Join(t.TempDir(), "project")
	if err := os.Symlink(dir, alias); err == nil {
		dir = alias
	}
	store, _ := config.OpenStore(root)
	settings := config.DefaultSettings()
	settings.WorkingDirectory = dir
	if err := store.SaveSettings(settings); err != nil {
		t.Fatal(err)
	}
	record, err := config.NewProviderRecord("e2e", "E2E", endpoint+"/v1", protocol, []string{"e2e-model"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	svcs := NewServices(store)
	if err := svcs.Settings.UpsertProvider(record); err != nil {
		t.Fatal(err)
	}
	svcs.Sessions.runner.setAsk(func(mygo.MessageOptions) (mygo.MessageResult, error) { return mygo.MessageResult{Button: choice}, nil })
	mgr, err := runtime.OpenManager(root, svcs.RuntimeOptions())
	if err != nil {
		t.Fatal(err)
	}
	svcs.AttachManager(mgr)
	a := Root(svcs)
	tt := ui.NewTester(a.View, 1280, 800)
	if err := tt.Click("Draft"); err != nil {
		t.Fatal(err)
	}
	tt.Type("Exercise the project tools")
	tt.Key(0, ui.KeyEnter)
	return svcs, a, tt, dir
}

func TestStackE2EAllToolsAndRestart(t *testing.T) {
	for _, protocol := range []config.Protocol{config.ProtocolMessages, config.ProtocolChatCompletions} {
		t.Run(string(protocol), func(t *testing.T) {
			calls := []e2eCall{
				{"Write", `{"file_path":"acceptance.txt","content":"BEFORE"}`},
				{"Read", `{"file_path":"acceptance.txt"}`},
				{"Edit", `{"file_path":"acceptance.txt","old_string":"BEFORE","new_string":"AFTER"}`},
				{"Glob", `{"pattern":"*.txt"}`},
				{"Grep", `{"pattern":"AFTER","path":"acceptance.txt"}`},
				{"Bash", `{"command":"printf SHELL_OK"}`},
			}
			var mu sync.Mutex
			var requests []string
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				body, _ := io.ReadAll(r.Body)
				mu.Lock()
				index := len(requests)
				requests = append(requests, string(body))
				mu.Unlock()
				var call *e2eCall
				if index < len(calls) {
					call = &calls[index]
				}
				e2eStream(w, protocol, call, index)
			}))
			defer server.Close()
			svcs, a, tt, dir := e2eApp(t, server.URL, protocol, 0)
			id := a.activeID
			if id == "" {
				t.Fatal("Enter did not create a session")
			}
			waitFor(t, func() bool { return !svcs.Sessions.Running(id) }, "real stack completion")
			replayInto(t, svcs, a, id)
			tt.Frame()
			if !tt.HasText("STACK_OK") {
				t.Fatalf("reply missing: %q", tt.Texts())
			}
			content, err := os.ReadFile(filepath.Join(dir, "acceptance.txt"))
			if err != nil || string(content) != "AFTER" {
				t.Fatalf("actual file = %q, err=%v", content, err)
			}
			history, err := svcs.Sessions.History(id)
			if err != nil {
				t.Fatal(err)
			}
			completed := 0
			for _, event := range history {
				if event.Type == runtime.EventToolEnd {
					if event.Tool.Status != runtime.ToolStatusCompleted {
						t.Fatalf("tool failed: %+v", event.Tool)
					}
					completed++
				}
			}
			if completed != 6 {
				t.Fatalf("completed tools = %d", completed)
			}
			mu.Lock()
			got := append([]string(nil), requests...)
			mu.Unlock()
			if len(got) != 7 || !strings.Contains(got[6], "SHELL_OK") {
				t.Fatalf("tool results did not return to provider: requests=%d", len(got))
			}
			// Recreate the service and manager just as process startup does.
			store, _ := config.OpenStore(svcs.Settings.store.Root())
			restarted := NewServices(store)
			mgr, err := runtime.OpenManager(store.Root(), restarted.RuntimeOptions())
			if err != nil {
				t.Fatal(err)
			}
			restarted.AttachManager(mgr)
			restored := Root(restarted)
			view := ui.NewTester(restored.View, 1280, 800)
			if restored.activeID != id || !view.HasText("STACK_OK") || restored.activeView().running {
				t.Fatalf("restart lost completed conversation: id=%s texts=%q", restored.activeID, view.Texts())
			}
			if restored.activeView().draft != "" {
				t.Fatal("sent draft resurrected on restart")
			}
		})
	}
}

func TestStackE2EDeniedWriteLeavesNoFile(t *testing.T) {
	var mu sync.Mutex
	count := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		index := count
		count++
		mu.Unlock()
		var call *e2eCall
		if index == 0 {
			call = &e2eCall{"Write", `{"file_path":"denied.txt","content":"MUST_NOT_EXIST"}`}
		}
		e2eStream(w, config.ProtocolMessages, call, index)
	}))
	defer server.Close()
	svcs, a, tt, dir := e2eApp(t, server.URL, config.ProtocolMessages, 2)
	id := a.activeID
	waitFor(t, func() bool { return !svcs.Sessions.Running(id) }, "denied tool completion")
	replayInto(t, svcs, a, id)
	tt.Frame()
	if _, err := os.Stat(filepath.Join(dir, "denied.txt")); !os.IsNotExist(err) {
		t.Fatalf("denied tool mutated disk: %v", err)
	}
	history, _ := svcs.Sessions.History(id)
	denied := false
	for _, e := range history {
		if e.Tool != nil && e.Tool.Status == runtime.ToolStatusDenied {
			denied = true
		}
	}
	if !denied || !tt.HasText("STACK_OK") {
		t.Fatalf("denial did not round trip: denied=%v texts=%q", denied, tt.Texts())
	}
}

func TestStackE2EHTTPFailureAndEscapeCancellation(t *testing.T) {
	for _, failure := range []bool{true, false} {
		name := "escape-cancels-stream"
		if failure {
			name = "http-error-detail"
		}
		t.Run(name, func(t *testing.T) {
			started := make(chan struct{})
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if failure {
					http.Error(w, `{"error":{"type":"invalid_request_error","message":"E2E_INVALID_REQUEST"}}`, http.StatusBadRequest)
					return
				}
				w.Header().Set("Content-Type", "text/event-stream")
				fmt.Fprint(w, "data: {\"type\":\"message_start\",\"message\":{}}\n\n")
				w.(http.Flusher).Flush()
				close(started)
				<-r.Context().Done()
			}))
			defer server.Close()
			svcs, a, tt, _ := e2eApp(t, server.URL, config.ProtocolMessages, 0)
			id := a.activeID
			defer svcs.Sessions.Stop(id)
			if !failure {
				waitFor(t, func() bool {
					select {
					case <-started:
						return true
					default:
						return false
					}
				}, "SSE connection")
				tt.Key(0, ui.KeyEscape)
			}
			waitFor(t, func() bool { return !svcs.Sessions.Running(id) }, "terminal state")
			replayInto(t, svcs, a, id)
			tt.Frame()
			history, _ := svcs.Sessions.History(id)
			want := runtime.EventTurnCancelled
			if failure {
				want = runtime.EventTurnFailed
			}
			found := false
			for _, e := range history {
				if e.Type == want {
					found = true
				}
			}
			if !found || a.activeView().running {
				t.Fatalf("missing terminal %s", want)
			}
			if failure {
				if err := tt.Click("查看详情"); err != nil {
					t.Fatal(err)
				}
				if !tt.HasText("E2E_INVALID_REQUEST") {
					t.Fatalf("full error unavailable: %q", tt.Texts())
				}
			}
		})
	}
}
