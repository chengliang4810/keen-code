//go:build acceptance

// Live end-to-end acceptance for the Go migration stack
// (docs/go-migration.md §5.2-§5.5): the real providers.json → config.Store →
// protocol adapter → agent loop → runtime session → journal → subscription
// stream, driven against the model configured on this machine.
//
// The build tag keeps network traffic out of ordinary test runs. Execute it
// explicitly with:
//
//	go test -tags acceptance -run TestLive -v ./internal/agent/
//
// The API key is read from the configuration file only; it is never printed,
// logged, or persisted anywhere by this test. Failures are redacted through
// model.RedactErrorSecrets before they reach the test log.
package agent

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	goruntime "runtime"
	"strings"
	"testing"
	"time"

	"keencode/internal/config"
	"keencode/internal/model"
	"keencode/internal/provider/anthropic"
	"keencode/internal/provider/openai"
	"keencode/internal/runtime"
)

const (
	// livePrompt is the canonical smoke prompt: a deterministic one-word
	// answer that no tool usage can improve.
	livePrompt = "Reply with exactly: OK"
	// liveMaxAttempts bounds the real calls: one initial attempt plus at
	// most three retries on network, authentication, or protocol failures.
	liveMaxAttempts = 4
	// liveTurnTimeout bounds one live turn; a plain reply takes seconds, so
	// this only guards against a hung endpoint.
	liveTurnTimeout = 90 * time.Second
)

// TestLiveStreamingTurnReply loads the real provider configuration, creates
// a runtime session, sends livePrompt, and asserts the turn produced a
// non-empty streaming reply and ended with EventTurnCompleted. Missing or
// unusable configuration skips the test (a skip is not a failure).
func TestLiveStreamingTurnReply(t *testing.T) {
	state, source := loadLiveProvidersState(t)
	providerID, ok := state.ActiveProvider()
	if !ok {
		t.Skipf("配置来源 %s 未选择当前供应商，无可用供应商", source)
	}
	modelID, ok := state.ActiveModel()
	if !ok {
		t.Skipf("配置来源 %s 未选择当前模型，无可用供应商", source)
	}
	record, found := state.Provider(providerID)
	if !found {
		t.Skipf("配置来源 %s 的当前供应商 %s 不存在，无可用供应商", source, providerID)
	}
	provider := newLiveProvider(t, record)

	// Session data (journals, metadata) goes to a temporary directory so the
	// live run never writes into the user's ~/.keencode data roots. The
	// text-only assembly (no tool registry) mirrors a chat turn: the request
	// advertises no tools, so no permission bridge is needed. Provider
	// assembly otherwise matches internal/app/services.go providerFor.
	workDir := t.TempDir()
	mgr, err := runtime.OpenManager(t.TempDir(), runtime.ManagerOptions{
		Agent: func() (runtime.TurnRunner, error) {
			return liveTurnRunner{inner: New(Dependencies{
				Provider: provider,
				System:   BuildSystemPrompt(workDir, goruntime.GOOS),
			})}, nil
		},
		Model: func() string { return modelID },
	})
	if err != nil {
		t.Fatalf("打开会话运行时失败：%v", err)
	}

	var failures []string
	for attempt := 1; attempt <= liveMaxAttempts; attempt++ {
		ctx, cancel := context.WithTimeout(context.Background(), liveTurnTimeout)
		sess, err := mgr.Create(workDir)
		if err != nil {
			// A local runtime failure is not retryable; retrying cannot fix
			// the machine's own storage.
			cancel()
			t.Fatalf("创建会话失败：%v", err)
		}
		events, stop := sess.Subscribe()
		if err := sess.Send(ctx, livePrompt); err != nil {
			stop()
			cancel()
			failures = append(failures, model.RedactErrorSecretsBounded(
				fmtAttempt(attempt, "发送失败", err.Error()), 600))
			continue
		}
		result := waitLiveTurn(ctx, events)
		stop()
		cancel()

		if result.terminal == runtime.EventTurnCompleted && strings.TrimSpace(result.text.String()) != "" {
			t.Logf("打通：供应商 %s（%s）协议 %s 模型 %s", record.ID, record.Name, record.APIBackend, modelID)
			t.Logf("收到 %d 个文本增量，回复 %q，结束原因 %s，会话 %s",
				result.textDeltas, result.text.String(), string(result.stopReason), sess.Meta().ID)
			if result.usage != nil {
				t.Logf("token 用量：输入 %d / 输出 %d / 缓存 %d",
					result.usage.InputTokens, result.usage.OutputTokens, result.usage.CachedTokens)
			}
			return
		}
		switch {
		case result.terminal == "":
			failures = append(failures, fmtAttempt(attempt, "回合在超时内未产生终态事件",
				strings.TrimSpace(result.reasoning.String())))
		case result.terminal == runtime.EventTurnCompleted:
			failures = append(failures, fmtAttempt(attempt, "回合正常结束但文本回复为空",
				strings.TrimSpace(result.reasoning.String())))
		default:
			failures = append(failures, fmtAttempt(attempt,
				"回合以 "+string(result.terminal)+" 结束", result.failure))
		}
	}
	t.Fatalf("真实调用在 %d 次尝试后仍未打通（配置来源 %s，供应商 %s，模型 %s）：\n%s",
		liveMaxAttempts, source, providerID, modelID, strings.Join(failures, "\n"))
}

// loadLiveProvidersState loads the providers state the way the runtime does:
// config.DefaultRoot (KEENCODE_GO_HOME override, otherwise
// ~/.keencode/go-v1). When the Go data root carries no usable selection —
// the fresh-machine shape, since decision D4 keeps go-v1 empty — the test
// falls back to reading the Rust desktop build's
// ~/.keencode/providers.json READ-ONLY through the same loader. Nothing is
// written back and session data still goes to a temporary directory. Every
// dead end calls t.Skip with the concrete reason.
func loadLiveProvidersState(t *testing.T) (config.ProvidersState, string) {
	t.Helper()
	root, err := config.DefaultRoot()
	if err != nil {
		t.Skipf("无法定位数据根目录，跳过真实验收：%v", err)
	}
	store, err := config.OpenStore(root)
	if err != nil {
		t.Skipf("无法打开配置存储 %s，跳过真实验收：%v", root, err)
	}
	state, _, err := store.LoadProviders()
	if err != nil {
		t.Skipf("供应商配置 %s 读取失败（视为无可用供应商），跳过真实验收：%s",
			store.ProvidersPath(), model.RedactErrorSecretsBounded(err.Error(), 400))
	}
	if liveSelectionUsable(state) {
		return state, root
	}
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skipf("Go 根 %s 未配置可用供应商且无法定位用户目录读取回退配置：%v", root, err)
	}
	legacyPath := filepath.Join(home, ".keencode", "providers.json")
	legacy, _, err := config.LoadProvidersFromPath(legacyPath)
	if err != nil {
		t.Skipf("Go 根 %s 无可用供应商，回退读取 %s 也失败，跳过真实验收：%s",
			root, legacyPath, model.RedactErrorSecretsBounded(err.Error(), 400))
	}
	if !liveSelectionUsable(legacy) {
		t.Skipf("Go 根 %s 与回退配置 %s 均未配置可用供应商（缺少供应商、当前供应商或当前模型），跳过真实验收",
			root, legacyPath)
	}
	t.Logf("Go 根 %s 未配置供应商，回退使用旧栈配置（只读）：%s", root, legacyPath)
	return legacy, legacyPath
}

// liveSelectionUsable reports whether the state carries a resolvable active
// provider/model pair, the minimum the runtime needs to build a turn.
func liveSelectionUsable(state config.ProvidersState) bool {
	id, ok := state.ActiveProvider()
	if !ok {
		return false
	}
	if _, ok := state.ActiveModel(); !ok {
		return false
	}
	_, found := state.Provider(id)
	return found
}

// newLiveProvider maps one configured record onto the protocol adapters with
// the same mapping as internal/app/services.go providerFor (Messages →
// Anthropic adapter, Chat Completions → OpenAI adapter, Responses →
// unsupported in v1). Construction problems mean no usable provider, so they
// skip instead of fail.
func newLiveProvider(t *testing.T, record config.ProviderRecord) model.Provider {
	t.Helper()
	endpoint, err := record.Endpoint()
	if err != nil {
		t.Skipf("供应商 %s 的端点解析失败（无可用供应商），跳过真实验收：%s",
			record.ID, model.RedactErrorSecretsBounded(err.Error(), 400))
	}
	switch endpoint.Protocol {
	case config.ProtocolMessages:
		adapter, err := anthropic.New(anthropic.Options{
			BaseURL: endpoint.BaseURL,
			APIKey:  endpoint.APIKey,
		})
		if err != nil {
			t.Skipf("供应商 %s 的 Anthropic 适配器构建失败，跳过真实验收：%s",
				record.ID, model.RedactErrorSecretsBounded(err.Error(), 400))
		}
		return adapter
	case config.ProtocolChatCompletions:
		field := ""
		if record.ChatOutputTokenField == config.ChatOutputFieldMaxTokens {
			field = openai.OutputTokenFieldMaxTokens
		}
		adapter, err := openai.New(openai.Options{
			Endpoint:         strings.TrimRight(endpoint.BaseURL, "/") + "/chat/completions",
			APIKey:           endpoint.APIKey,
			OutputTokenField: field,
		})
		if err != nil {
			t.Skipf("供应商 %s 的 OpenAI 适配器构建失败，跳过真实验收：%s",
				record.ID, model.RedactErrorSecretsBounded(err.Error(), 400))
		}
		return adapter
	default:
		t.Skipf("供应商 %s 使用协议 %s，Go 运行时 v1 暂未支持（与 internal/app providerFor 一致），无可用供应商",
			record.ID, endpoint.Protocol)
		return nil
	}
}

// liveTurnResult summarizes what one live turn streamed.
type liveTurnResult struct {
	// textDeltas counts text_delta events (the streaming-reply witness).
	textDeltas int
	// text accumulates the assistant text payload.
	text strings.Builder
	// reasoning accumulates reasoning deltas for failure diagnostics.
	reasoning strings.Builder
	// terminal is the observed terminal event type, empty when the turn
	// never reached one inside the timeout.
	terminal runtime.EventType
	// stopReason is the unified stop reason of the terminal event.
	stopReason model.StopReason
	// failure carries the EventTurnFailed message.
	failure string
	// usage is the last usage snapshot of the turn.
	usage *model.TokenUsage
}

// waitLiveTurn drains the subscription channel until a terminal event, the
// channel closes, or ctx expires. It ignores replayed history events and
// never blocks past the context deadline.
func waitLiveTurn(ctx context.Context, events <-chan runtime.Event) liveTurnResult {
	var result liveTurnResult
	for {
		select {
		case <-ctx.Done():
			return result
		case event, ok := <-events:
			if !ok {
				return result
			}
			if event.Replay {
				continue
			}
			switch event.Type {
			case runtime.EventTextDelta:
				result.textDeltas++
				result.text.WriteString(event.Text)
			case runtime.EventReasoningDelta, runtime.EventReasoningContinuation:
				result.reasoning.WriteString(event.Text)
			case runtime.EventUsage:
				result.usage = event.Usage
			case runtime.EventTurnCompleted, runtime.EventTurnFailed, runtime.EventTurnCancelled:
				result.terminal = event.Type
				result.stopReason = event.StopReason
				result.failure = event.Text
				return result
			}
		}
	}
}

// fmtAttempt renders one redacted per-attempt failure line.
func fmtAttempt(attempt int, headline, detail string) string {
	trimmed := strings.TrimSpace(model.RedactErrorSecretsBounded(detail, 400))
	if trimmed == "" {
		return fmt.Sprintf("第 %d 次尝试：%s", attempt, headline)
	}
	return fmt.Sprintf("第 %d 次尝试：%s：%s", attempt, headline, trimmed)
}

// liveTurnRunner adapts the loop agent onto runtime.TurnRunner, the same
// field-by-field bridge as internal/app/runner.go turnRunnerAdapter; the two
// vocabularies are nominally distinct so the conversion must be spelled out.
type liveTurnRunner struct {
	inner *Agent
}

// RunTurn converts the request and the journal/emit callbacks and delegates
// to the loop agent.
func (r liveTurnRunner) RunTurn(ctx context.Context, req runtime.TurnRequest,
	journal func(runtime.Event) error, emit func(runtime.Event) error) error {
	areq := TurnRequest{
		SessionID: req.SessionID,
		TurnID:    req.TurnID,
		Model:     req.Model,
		History:   req.History,
		WorkDir:   req.WorkDir,
	}
	var toJournal func(Event) error
	if journal != nil {
		toJournal = func(ev Event) error { return journal(liveEventToRuntime(ev)) }
	}
	var toEmit func(Event) error
	if emit != nil {
		toEmit = func(ev Event) error { return emit(liveEventToRuntime(ev)) }
	}
	return r.inner.RunTurn(ctx, areq, toJournal, toEmit)
}

// liveEventToRuntime maps one loop event onto the runtime envelope; the
// fields are identical and the types nominal (see internal/app/runner.go
// eventToRuntime).
func liveEventToRuntime(ev Event) runtime.Event {
	out := runtime.Event{
		ID:         ev.ID,
		SessionID:  ev.SessionID,
		TurnID:     ev.TurnID,
		Seq:        ev.Seq,
		Replay:     ev.Replay,
		Time:       ev.Time,
		Type:       runtime.EventType(ev.Type),
		Text:       ev.Text,
		StopReason: ev.StopReason,
		Usage:      ev.Usage,
	}
	if ev.Tool != nil {
		out.Tool = &runtime.ToolEvent{
			CallID:       ev.Tool.CallID,
			Name:         ev.Tool.Name,
			Summary:      ev.Tool.Summary,
			Status:       ev.Tool.Status,
			Detail:       ev.Tool.Detail,
			AddedLines:   ev.Tool.AddedLines,
			RemovedLines: ev.Tool.RemovedLines,
		}
	}
	return out
}
