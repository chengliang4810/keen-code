package anthropic

import (
	"errors"
	"reflect"
	"testing"

	"keencode/internal/model"
)

// sseText feeds one decoder every chunk and returns all frames.
func sseText(t *testing.T, chunks ...[]byte) []sseFrame {
	t.Helper()
	decoder := newSSEDecoder(1024)
	var frames []sseFrame
	for _, chunk := range chunks {
		completed, err := decoder.push(chunk)
		if err != nil {
			t.Fatalf("push(%q): %v", chunk, err)
		}
		frames = append(frames, completed...)
	}
	final, err := decoder.finish()
	if err != nil {
		t.Fatalf("finish: %v", err)
	}
	return append(frames, final...)
}

// TestSSEDecoderHandlesArbitraryChunksCRLFAndMultilineData ports
// tests.rs:1465-1477.
func TestSSEDecoderHandlesArbitraryChunksCRLFAndMultilineData(t *testing.T) {
	frames := sseText(t, []byte("event: demo\r\nda"), []byte("ta: first\r\ndata: second\r\n\r\n"))
	if len(frames) != 1 {
		t.Fatalf("frames = %d, want 1", len(frames))
	}
	if frames[0].event == nil || *frames[0].event != "demo" {
		t.Fatalf("event = %v, want demo", frames[0].event)
	}
	if frames[0].data != "first\nsecond" {
		t.Fatalf("data = %q, want %q", frames[0].data, "first\nsecond")
	}
}

// TestSSEDecoderAcceptsBareCRAndSplitCRLFBoundaries ports tests.rs:1480-1505.
func TestSSEDecoderAcceptsBareCRAndSplitCRLFBoundaries(t *testing.T) {
	frames := sseText(t, []byte("event: demo\rdata: first\r"), []byte("\ndata: second\r\rdata: third\r\n\r\n"))
	want := []sseFrame{
		{event: strPtr("demo"), data: "first\nsecond"},
		{event: nil, data: "third"},
	}
	if !reflect.DeepEqual(frames, want) {
		t.Fatalf("frames = %+v, want %+v", frames, want)
	}
}

// TestSSEDecoderEmptyEventFieldResetsEventName ports tests.rs:1508-1517.
func TestSSEDecoderEmptyEventFieldResetsEventName(t *testing.T) {
	frames := sseText(t, []byte("event: named\ndata: first\n\nevent:\ndata: second\n\n"))
	if len(frames) != 2 {
		t.Fatalf("frames = %d, want 2", len(frames))
	}
	if frames[0].event == nil || *frames[0].event != "named" {
		t.Fatalf("first event = %v, want named", frames[0].event)
	}
	if frames[1].event != nil {
		t.Fatalf("second event = %v, want nil", frames[1].event)
	}
}

// TestSSEDecoderAcceptsSplitUTF8BOMAtStreamStart ports tests.rs:1520-1535.
func TestSSEDecoderAcceptsSplitUTF8BOMAtStreamStart(t *testing.T) {
	frames := sseText(t, []byte("\xEF"), []byte("\xBB"), []byte("\xBFevent: demo\ndata: KC_OK\n\n"))
	want := []sseFrame{{event: strPtr("demo"), data: "KC_OK"}}
	if !reflect.DeepEqual(frames, want) {
		t.Fatalf("frames = %+v, want %+v", frames, want)
	}
	// A BOM in the middle of the stream is not stripped.
	frames = sseText(t, []byte("data: a\n\ndata: \xEF\xBB\xBFb\n\n"))
	if len(frames) != 2 || frames[1].data != "\uFEFFb" {
		t.Fatalf("mid-stream BOM handling changed data: %+v", frames)
	}
}

// TestSSEDecoderLimitsEachEventInsteadOfNetworkChunk ports tests.rs:1538-1551.
func TestSSEDecoderLimitsEachEventInsteadOfNetworkChunk(t *testing.T) {
	decoder := newSSEDecoder(16)
	frames, err := decoder.push([]byte("data: a\n\ndata: b\n\n"))
	if err != nil {
		t.Fatalf("push: %v", err)
	}
	if len(frames) != 2 || frames[0].data != "a" || frames[1].data != "b" {
		t.Fatalf("frames = %+v, want two small frames", frames)
	}

	oversized := newSSEDecoder(8)
	if _, err := oversized.push([]byte("data: too-long\n\n")); err == nil {
		t.Fatalf("oversized event accepted")
	} else {
		var modelErr *model.ModelError
		if !errors.As(err, &modelErr) || modelErr.Kind != model.ErrorProtocol {
			t.Fatalf("error = %v, want protocol ModelError", err)
		}
	}
}

// TestSSEDecoderFinishFlushesTrailingEvent covers a body ending without a
// final blank line.
func TestSSEDecoderFinishFlushesTrailingEvent(t *testing.T) {
	frames := sseText(t, []byte("data: tail"))
	if len(frames) != 1 || frames[0].data != "tail" {
		t.Fatalf("frames = %+v, want one tail frame", frames)
	}
}

// TestSSEDecoderRejectsInvalidUTF8 covers the UTF-8 field validation.
func TestSSEDecoderRejectsInvalidUTF8(t *testing.T) {
	decoder := newSSEDecoder(1024)
	if _, err := decoder.push([]byte("data: \xFF\n\n")); err == nil {
		t.Fatalf("invalid UTF-8 accepted")
	}
}

func strPtr(value string) *string { return &value }
