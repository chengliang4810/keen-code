package model

import (
	"strings"
	"testing"
)

func TestRedactErrorSecretsHeadersAndAssignments(t *testing.T) {
	tests := []struct {
		name string
		raw  string
		want string
	}{
		{
			name: "bearer header keeps scheme",
			raw:  "HTTP 401 request_id=req-1 AUTHORIZATION: Bearer header-secret",
			want: "HTTP 401 request_id=req-1 AUTHORIZATION: Bearer [REDACTED]",
		},
		{
			name: "basic header keeps scheme",
			raw:  "Proxy-Authorization: Basic cHJveHk6c2VjcmV0 request_id=req-2",
			want: "Proxy-Authorization: Basic [REDACTED] request_id=req-2",
		},
		{
			name: "structured digest params consumed",
			raw:  "Authorization: Digest username=alice, response=digest-response, nonce=digest-nonce request_id=req-3",
			want: "Authorization: Digest [REDACTED] request_id=req-3",
		},
		{
			name: "spaced assignment with trailing context",
			raw:  "x-api-key = api-secret, Retry-After: 30",
			want: "x-api-key = [REDACTED], Retry-After: 30",
		},
		{
			name: "arrow assignment keeps quotes",
			raw:  `Client_Secret=>"client-secret"; status=401`,
			want: `Client_Secret=>"[REDACTED]"; status=401`,
		},
		{
			name: "single quoted value with space",
			raw:  "PASSWORD: 'two words' ok=yes",
			want: "PASSWORD: '[REDACTED]' ok=yes",
		},
		{
			name: "json quoted field name and value",
			raw:  `{"apiKey":"密钥值","plain":"可见"}`,
			want: `{"apiKey":"[REDACTED]","plain":"可见"}`,
		},
		{
			name: "json escaped nested quoted value",
			raw:  `payload={\"Refresh-Token\":\"refresh-secret\"} code=E401`,
			want: `payload={\"Refresh-Token\":\"[REDACTED]\"} code=E401`,
		},
		{
			name: "human readable key with space",
			raw:  "API key: readable-secret; status=401",
			want: "API key: [REDACTED]; status=401",
		},
		{
			name: "cookie header redacted whole with context kept",
			raw:  "Cookie: sid=cookie-secret; refresh=refresh-cookie-secret request_id=req-4",
			want: "Cookie: [REDACTED] request_id=req-4",
		},
		{
			name: "unquoted secret stops at delimiter",
			raw:  "token=abc123, status=401",
			want: "token=[REDACTED], status=401",
		},
		{
			name: "bearer standalone word boundary",
			raw:  "Bearer token-value here",
			want: "Bearer [REDACTED] here",
		},
		{
			name: "prose bearer redacts the next word (Rust parity)",
			raw:  "the bearer of bad news",
			want: "the bearer [REDACTED] bad news",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := RedactErrorSecrets(tt.raw); got != tt.want {
				t.Fatalf("RedactErrorSecrets() =\n %q\nwant\n %q", got, tt.want)
			}
		})
	}
}

func TestRedactErrorSecretsURLs(t *testing.T) {
	tests := []struct {
		name string
		raw  string
		want string
	}{
		{
			name: "userinfo query and fragment",
			raw:  "connect https://user:password@example.invalid/v1?api_key=query-secret&request_id=req-9&token_count=88#access_token=fragment-secret failed",
			want: "connect https://example.invalid/v1?api_key=[REDACTED]&request_id=req-9&token_count=88#access_token=[REDACTED] failed",
		},
		{
			name: "sensitive path assignment",
			raw:  "GET https://example.invalid/token=path-secret request_id=req-10",
			want: "GET https://example.invalid/token=[REDACTED] request_id=req-10",
		},
		{
			name: "nested url in query value",
			raw:  "follow https://outer.invalid/callback?redirect=https://inner.invalid/v1 inner=1",
			want: "follow https://outer.invalid/callback?redirect=https://inner.invalid/v1 inner=1",
		},
		{
			name: "nested url userinfo redacted through query value",
			raw:  "follow https://outer.invalid/callback?redirect=https://inner-user:inner-password@inner.invalid/v1 request_id=req-11",
			want: "follow https://outer.invalid/callback?redirect=https://inner.invalid/v1 request_id=req-11",
		},
		{
			name: "plain url without secrets unchanged",
			raw:  "see https://example.invalid/docs and more",
			want: "see https://example.invalid/docs and more",
		},
		{
			name: "trailing punctuation preserved",
			raw:  "failed at https://user:pass@example.invalid/v1), status=401",
			want: "failed at https://example.invalid/v1), status=401",
		},
		{
			name: "percent encoded sensitive query name",
			raw:  "GET https://example.invalid/v1?%61pi_key=encoded-secret&ok=1",
			want: "GET https://example.invalid/v1?%61pi_key=[REDACTED]&ok=1",
		},
		{
			name: "non http scheme left alone",
			raw:  "postgres://user:password@db.invalid/main",
			want: "postgres://user:password@db.invalid/main",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := RedactErrorSecrets(tt.raw)
			if got != tt.want {
				t.Fatalf("RedactErrorSecrets() =\n %q\nwant\n %q", got, tt.want)
			}
		})
	}
}

func TestRedactErrorSecretsRemovesSecretEverywhere(t *testing.T) {
	raw := "远端失败 access_token=密钥值-東京 request_id=req-long error_code=E429"
	safe := RedactErrorSecrets(raw)
	if strings.Contains(safe, "密钥值") {
		t.Fatalf("secret survived: %q", safe)
	}
	if !strings.Contains(safe, "access_token=[REDACTED]") || !strings.Contains(safe, "request_id=req-long") {
		t.Fatalf("context lost: %q", safe)
	}
}

func TestRedactErrorSecretsPreservesOrdinaryContext(t *testing.T) {
	tests := []string{
		"maximum context length: max_tokens=4096 input_tokens=5000 token_count=5000",
		"Authorization failed; password policy rejected; secret service unavailable",
		"Bearer\nrequest_id=req-next",
		"status=401 detail=quota exceeded",
		"https://example.invalid/v1?request_id=req-7&token_count=9",
	}
	for _, raw := range tests {
		t.Run(raw, func(t *testing.T) {
			if got := RedactErrorSecrets(raw); got != raw {
				t.Fatalf("RedactErrorSecrets() = %q, want unchanged %q", got, raw)
			}
		})
	}
}

func TestRedactErrorSecretsIsIdempotent(t *testing.T) {
	raw := "Authorization: Bearer secret token=[REDACTED] url=https://u:p@host.invalid/?sig=x"
	once := RedactErrorSecrets(raw)
	if !strings.Contains(once, "Authorization: Bearer [REDACTED]") {
		t.Fatalf("bearer not redacted: %q", once)
	}
	if strings.Contains(once, "sig=x") || strings.Contains(once, "u:p@") {
		t.Fatalf("url secrets not redacted: %q", once)
	}
	if twice := RedactErrorSecrets(once); twice != once {
		t.Fatalf("not idempotent:\n once  %q\n twice %q", once, twice)
	}
}

func TestRedactErrorSecretsBounded(t *testing.T) {
	raw := "前缀 access_token=秘密值 request_id=req-b"
	safe := RedactErrorSecretsBounded(raw, 1000)
	if !strings.Contains(safe, "access_token=[REDACTED]") {
		t.Fatalf("bounded output lost redaction: %q", safe)
	}
	// Truncation respects UTF-8 rune boundaries.
	limit := 4
	bounded := RedactErrorSecretsBounded("测试文本", limit)
	if len(bounded) > limit {
		t.Fatalf("bounded output too long: %d", len(bounded))
	}
	for _, r := range bounded {
		if r == '�' {
			t.Fatalf("bounded output cut inside a rune: %q", bounded)
		}
	}
	if got := RedactErrorSecretsBounded("abc", 0); got != "" {
		t.Fatalf("zero limit should return empty, got %q", got)
	}
}

func TestRedactPlaceholderPrefixCannotHideSuffix(t *testing.T) {
	raw := "token=[REDACTED]opaque-secret request_id=req-p"
	safe := RedactErrorSecrets(raw)
	if strings.Contains(safe, "opaque-secret") {
		t.Fatalf("secret hidden behind placeholder survived: %q", safe)
	}
	if !strings.Contains(safe, "request_id=req-p") {
		t.Fatalf("context lost: %q", safe)
	}
}
