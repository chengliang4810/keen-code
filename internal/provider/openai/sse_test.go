package openai

import (
	"reflect"
	"strings"
	"testing"
)

// TestSSEDecoderArbitraryChunksCRLFAndMultilineData ports
// core/provider/src/tests.rs:1466-1475: an event split across network
// chunks with CRLF breaks and two data fields joins into one frame.
func TestSSEDecoderArbitraryChunksCRLFAndMultilineData(t *testing.T) {
	decoder := newSSEDecoder(1024)
	frames, err := decoder.push([]byte("event: demo\r\nda"))
	if err != nil {
		t.Fatalf("push 首分块失败：%v", err)
	}
	if len(frames) != 0 {
		t.Fatalf("首分块不应产生事件，实际 %d 个", len(frames))
	}
	frames, err = decoder.push([]byte("ta: first\r\ndata: second\r\n\r\n"))
	if err != nil {
		t.Fatalf("push 尾分块失败：%v", err)
	}
	want := []sseFrame{{Event: "demo", HasEvent: true, Data: "first\nsecond"}}
	if !reflect.DeepEqual(frames, want) {
		t.Fatalf("跨分块事件不符：got %v want %v", framesDescription(frames), framesDescription(want))
	}
}

// TestSSEDecoderBareCRAndSplitCRLF ports core/provider/src/tests.rs:1477-1497:
// bare CR line breaks and a CRLF split across chunks produce no empty
// events.
func TestSSEDecoderBareCRAndSplitCRLF(t *testing.T) {
	decoder := newSSEDecoder(1024)
	if _, err := decoder.push([]byte("event: demo\rdata: first\r")); err != nil {
		t.Fatalf("push 失败：%v", err)
	}
	frames, err := decoder.push([]byte("\ndata: second\r\rdata: third\r\n\r\n"))
	if err != nil {
		t.Fatalf("push 失败：%v", err)
	}
	want := []sseFrame{
		{Event: "demo", HasEvent: true, Data: "first\nsecond"},
		{Data: "third"},
	}
	if !reflect.DeepEqual(frames, want) {
		t.Fatalf("裸 CR 事件不符：got %v want %v", framesDescription(frames), framesDescription(want))
	}
}

// TestSSEDecoderEmptyEventFieldResetsEventName ports
// core/provider/src/tests.rs:1500-1510: an empty event field clears the
// previous event name instead of sticking.
func TestSSEDecoderEmptyEventFieldResetsEventName(t *testing.T) {
	decoder := newSSEDecoder(1024)
	frames, err := decoder.push([]byte("event: named\ndata: first\n\nevent:\ndata: second\n\n"))
	if err != nil {
		t.Fatalf("push 失败：%v", err)
	}
	if len(frames) != 2 {
		t.Fatalf("应产出 2 个事件，实际 %d 个", len(frames))
	}
	if !frames[0].HasEvent || frames[0].Event != "named" {
		t.Fatalf("第一个事件名不符：%v", frames[0])
	}
	if frames[1].HasEvent {
		t.Fatalf("空 event 字段应清除事件名，实际 %q", frames[1].Event)
	}
}

// TestSSEDecoderSplitUTF8BOMAtStreamStart ports
// core/provider/src/tests.rs:1513-1529: a BOM split across three chunks is
// stripped only at stream start.
func TestSSEDecoderSplitUTF8BOMAtStreamStart(t *testing.T) {
	decoder := newSSEDecoder(1024)
	for _, chunk := range [][]byte{{0xEF}, {0xBB}, {0xBF}} {
		if _, err := decoder.push(chunk); err != nil {
			t.Fatalf("push BOM 分块失败：%v", err)
		}
	}
	frames, err := decoder.push([]byte("event: demo\ndata: KC_OK\n\n"))
	if err != nil {
		t.Fatalf("push 失败：%v", err)
	}
	want := []sseFrame{{Event: "demo", HasEvent: true, Data: "KC_OK"}}
	if !reflect.DeepEqual(frames, want) {
		t.Fatalf("BOM 后事件不符：got %v want %v", framesDescription(frames), framesDescription(want))
	}
}

// TestSSEDecoderLimitsEachEventNotEachChunk ports
// core/provider/src/tests.rs:1532-1545: the byte ceiling applies per
// assembled event, and an oversized event fails with a protocol error.
func TestSSEDecoderLimitsEachEventNotEachChunk(t *testing.T) {
	decoder := newSSEDecoder(16)
	frames, err := decoder.push([]byte("data: a\n\ndata: b\n\n"))
	if err != nil {
		t.Fatalf("同一分块中的多个小事件不应拒绝：%v", err)
	}
	if len(frames) != 2 || frames[0].Data != "a" || frames[1].Data != "b" {
		t.Fatalf("小事件不符：%v", framesDescription(frames))
	}

	oversized := newSSEDecoder(8)
	if _, err := oversized.push([]byte("data: too-long\n\n")); err == nil {
		t.Fatal("超限事件应报错")
	} else if got := err.Error(); !containsAll(got, "8 字节安全上限") {
		t.Fatalf("超限错误应携带上限信息，实际 %q", got)
	}
}

// TestSSEDecoderFinishFlushesTail verifies the EOF path: a trailing field
// line without a newline and an event without its blank separator still
// produce their frame.
func TestSSEDecoderFinishFlushesTail(t *testing.T) {
	decoder := newSSEDecoder(1024)
	if _, err := decoder.push([]byte("data: incomplete")); err != nil {
		t.Fatalf("push 失败：%v", err)
	}
	frames, err := decoder.finish()
	if err != nil {
		t.Fatalf("finish 失败：%v", err)
	}
	want := []sseFrame{{Data: "incomplete"}}
	if !reflect.DeepEqual(frames, want) {
		t.Fatalf("尾帧不符：got %v want %v", framesDescription(frames), framesDescription(want))
	}
}

// TestSSEDecoderIgnoresCommentsAndOtherFields verifies comment lines and
// id/retry fields do not become events.
func TestSSEDecoderIgnoresCommentsAndOtherFields(t *testing.T) {
	decoder := newSSEDecoder(1024)
	frames, err := decoder.push([]byte(": keep-alive\nid: 42\nretry: 100\ndata: payload\n\n"))
	if err != nil {
		t.Fatalf("push 失败：%v", err)
	}
	want := []sseFrame{{Data: "payload"}}
	if !reflect.DeepEqual(frames, want) {
		t.Fatalf("注释与 id/retry 应被忽略：got %v", framesDescription(frames))
	}
}

// TestSSEDecoderRejectsInvalidUTF8 verifies a field line that is not valid
// UTF-8 fails closed.
func TestSSEDecoderRejectsInvalidUTF8(t *testing.T) {
	decoder := newSSEDecoder(1024)
	if _, err := decoder.push([]byte("data: \xff\xfe\n\n")); err == nil {
		t.Fatal("非 UTF-8 字段应报错")
	}
}

// framesDescription renders frames for failure messages.
func framesDescription(frames []sseFrame) []string {
	descriptions := make([]string, 0, len(frames))
	for _, frame := range frames {
		descriptions = append(descriptions, frameDescription(frame))
	}
	return descriptions
}

// containsAll reports whether the haystack contains every needle.
func containsAll(haystack string, needles ...string) bool {
	for _, needle := range needles {
		if !strings.Contains(haystack, needle) {
			return false
		}
	}
	return true
}
