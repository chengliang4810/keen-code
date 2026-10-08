package model

import (
	"encoding/json"
	"errors"
	"reflect"
	"testing"
)

func TestModelErrorIsRetryable(t *testing.T) {
	tests := []struct {
		name      string
		err       *ModelError
		retryable bool
	}{
		{name: "rate limited always retryable", err: &ModelError{Kind: ErrorRateLimited}, retryable: true},
		{name: "provider unavailable flagged", err: &ModelError{Kind: ErrorProviderUnavailable, Retryable: true}, retryable: true},
		{name: "provider unavailable unflagged", err: &ModelError{Kind: ErrorProviderUnavailable}, retryable: false},
		{name: "transport flagged", err: &ModelError{Kind: ErrorTransport, Retryable: true}, retryable: true},
		{name: "stream interrupted flagged", err: &ModelError{Kind: ErrorStreamInterrupted, Retryable: true}, retryable: true},
		{name: "stream interrupted unflagged", err: &ModelError{Kind: ErrorStreamInterrupted}, retryable: false},
		{name: "authentication final", err: &ModelError{Kind: ErrorAuthentication}, retryable: false},
		{name: "invalid request final", err: &ModelError{Kind: ErrorInvalidRequest}, retryable: false},
		{name: "cancelled final", err: &ModelError{Kind: ErrorCancelled}, retryable: false},
		{name: "protocol final", err: &ModelError{Kind: ErrorProtocol}, retryable: false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.err.IsRetryable(); got != tt.retryable {
				t.Fatalf("IsRetryable() = %v, want %v", got, tt.retryable)
			}
		})
	}
}

func TestModelErrorDisplayStrings(t *testing.T) {
	tests := []struct {
		name string
		err  *ModelError
		want string
	}{
		{
			name: "authentication",
			err:  &ModelError{Kind: ErrorAuthentication, Message: "密钥无效"},
			want: "模型服务认证失败：密钥无效",
		},
		{
			name: "unsupported capability includes name",
			err:  &ModelError{Kind: ErrorUnsupportedCapability, Capability: "reasoning", Message: "端点不支持"},
			want: "模型能力不受支持（reasoning）：端点不支持",
		},
		{
			name: "stream interrupted",
			err:  interrupted("事件流在响应结束事件之前关闭", "部分文本"),
			want: "模型响应流中断：事件流在响应结束事件之前关闭",
		},
		{
			name: "unknown kind falls back to message",
			err:  &ModelError{Kind: ErrorKind("mystery"), Message: "未知"},
			want: "未知",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.err.Error(); got != tt.want {
				t.Fatalf("Error() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestModelErrorPartialTextHelpers(t *testing.T) {
	interruption := interrupted("中断", "")
	if interruption.StreamPartialText() != "" {
		t.Fatalf("fresh interruption should carry no partial text")
	}
	interruption.WithPartialText("已流出的正文")
	if interruption.StreamPartialText() != "已流出的正文" {
		t.Fatalf("partial text = %q, want 已流出的正文", interruption.StreamPartialText())
	}
	// Empty partial text never sticks.
	fresh := interrupted("中断", "")
	fresh.WithPartialText("")
	if fresh.PartialText != "" {
		t.Fatalf("empty partial text should keep field unset, got %q", fresh.PartialText)
	}
	// Other kinds ignore the helper.
	other := &ModelError{Kind: ErrorTransport}
	other.WithPartialText("文本")
	if other.PartialText != "" {
		t.Fatalf("transport error should ignore partial text, got %q", other.PartialText)
	}
	if other.StreamPartialText() != "" {
		t.Fatalf("transport error should report no partial text")
	}
}

func TestModelErrorJSONRoundTrip(t *testing.T) {
	original := &ModelError{
		Kind:         ErrorRateLimited,
		Message:      "请求过于频繁",
		StatusCode:   429,
		RetryAfterMS: 1500,
	}
	var err error = original
	data, err2 := json.Marshal(err)
	if err2 != nil {
		t.Fatalf("marshal: %v", err2)
	}
	var decoded *ModelError
	if err2 := json.Unmarshal(data, &decoded); err2 != nil {
		t.Fatalf("unmarshal: %v", err2)
	}
	if !reflect.DeepEqual(decoded, original) {
		t.Fatalf("round trip mismatch:\n want %+v\n got  %+v", original, decoded)
	}
	// errors.As recovers the typed error from a plain error variable.
	var target *ModelError
	var wrapped error = &ModelError{Kind: ErrorStreamInterrupted, Message: "中断", PartialText: "部分"}
	if !errors.As(wrapped, &target) || target.Kind != ErrorStreamInterrupted {
		t.Fatalf("errors.As failed to recover *ModelError")
	}
}

func TestModelErrorConstructors(t *testing.T) {
	err := InvalidRequest("消息角色 %q 不受支持", "tool")
	if err.Kind != ErrorInvalidRequest || err.Message != `消息角色 "tool" 不受支持` {
		t.Fatalf("InvalidRequest = %+v", err)
	}
	proto := ProtocolError("内容块 %d 重复", 3)
	if proto.Kind != ErrorProtocol || proto.Message != "内容块 3 重复" {
		t.Fatalf("ProtocolError = %+v", proto)
	}
	cancel := CancelledError("上下文取消")
	if cancel.Kind != ErrorCancelled || cancel.IsRetryable() {
		t.Fatalf("CancelledError = %+v", cancel)
	}
	generic := NewError(ErrorQuotaExceeded, "额度不足")
	if generic.Kind != ErrorQuotaExceeded {
		t.Fatalf("NewError = %+v", generic)
	}
}
