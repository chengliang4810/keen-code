package openai

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

// utf8BOM is the byte order mark stripped once from the very first SSE line.
var utf8BOM = []byte{0xEF, 0xBB, 0xBF}

// sseFrame is one fully bounded server-sent event
// (core/provider/src/sse.rs:3-10).
type sseFrame struct {
	// Event is the event field value; meaningful only when HasEvent is set.
	// An empty `event:` field clears a previously seen event name, so the
	// pair models the Rust Option<String> faithfully.
	Event    string
	HasEvent bool
	// Data joins every data field of the event with "\n" per the SSE spec.
	Data string
}

// sseDecoder is an incremental server-sent event decoder. It tolerates
// arbitrary byte chunking, CRLF and bare CR line breaks, multi-line data
// fields, comment lines, and a leading UTF-8 BOM, and enforces a byte
// ceiling per assembled event. It is a direct port of
// core/provider/src/sse.rs:12-169.
type sseDecoder struct {
	buffer        []byte
	event         string
	hasEvent      bool
	data          strings.Builder
	hasData       bool
	atStreamStart bool
	pendingCR     bool
	maxEventBytes int
}

// newSSEDecoder returns a decoder enforcing the given per-event byte limit.
func newSSEDecoder(maxEventBytes int) *sseDecoder {
	return &sseDecoder{atStreamStart: true, maxEventBytes: maxEventBytes}
}

// push consumes one network chunk and returns every event completed inside
// it (core/provider/src/sse.rs:41-64).
func (d *sseDecoder) push(chunk []byte) ([]sseFrame, error) {
	var frames []sseFrame
	for _, b := range chunk {
		if d.pendingCR {
			d.pendingCR = false
			if b == '\n' {
				// CRLF split across chunks is a single line break.
				continue
			}
		}
		switch b {
		case '\n':
			line := d.takeBuffer()
			if err := d.consumeLine(line, &frames); err != nil {
				return nil, err
			}
		case '\r':
			line := d.takeBuffer()
			if err := d.consumeLine(line, &frames); err != nil {
				return nil, err
			}
			d.pendingCR = true
		default:
			d.buffer = append(d.buffer, b)
			if err := d.checkSize(); err != nil {
				return nil, err
			}
		}
	}
	return frames, nil
}

// finish flushes the last unterminated field line and any pending event when
// the HTTP body ends (core/provider/src/sse.rs:67-77).
func (d *sseDecoder) finish() ([]sseFrame, error) {
	var frames []sseFrame
	if len(d.buffer) > 0 {
		line := d.takeBuffer()
		if err := d.consumeLine(line, &frames); err != nil {
			return nil, err
		}
	}
	if d.hasEvent || d.hasData {
		frames = append(frames, d.takeFrame())
	}
	return frames, nil
}

// takeBuffer detaches the pending field line bytes.
func (d *sseDecoder) takeBuffer() []byte {
	line := d.buffer
	d.buffer = nil
	return line
}

// consumeLine applies one field line or event boundary
// (core/provider/src/sse.rs:79-115).
func (d *sseDecoder) consumeLine(line []byte, frames *[]sseFrame) error {
	if d.atStreamStart {
		d.atStreamStart = false
		if len(line) >= len(utf8BOM) && string(line[:len(utf8BOM)]) == string(utf8BOM) {
			line = line[len(utf8BOM):]
		}
	}
	if !utf8.Valid(line) {
		return protocolError("SSE 字段不是有效 UTF-8")
	}
	text := string(line)
	if text == "" {
		if d.hasEvent || d.hasData {
			*frames = append(*frames, d.takeFrame())
		}
		return nil
	}
	if strings.HasPrefix(text, ":") {
		// Comment lines keep the connection alive; they are not events.
		return nil
	}
	field, value, _ := strings.Cut(text, ":")
	value = strings.TrimPrefix(value, " ")
	switch field {
	case "event":
		d.hasEvent = value != ""
		d.event = value
	case "data":
		if d.hasData {
			d.data.WriteByte('\n')
		}
		d.data.WriteString(value)
		d.hasData = true
	default:
		// "id", "retry", and unknown fields carry no event payload here.
	}
	return d.checkSize()
}

// takeFrame detaches the assembled event (core/provider/src/sse.rs:117-123).
func (d *sseDecoder) takeFrame() sseFrame {
	d.hasData = false
	frame := sseFrame{Event: d.event, HasEvent: d.hasEvent, Data: d.data.String()}
	d.data.Reset()
	d.hasEvent = false
	d.event = ""
	return frame
}

// checkSize rejects assembled events beyond the configured byte ceiling
// (core/provider/src/sse.rs:147-161).
func (d *sseDecoder) checkSize() error {
	total := len(d.buffer) + len(d.event) + d.data.Len()
	if total > d.maxEventBytes {
		return protocolError("SSE 事件超过 %d 字节安全上限", d.maxEventBytes)
	}
	return nil
}

// frameDescription renders a frame for test failure messages.
func frameDescription(frame sseFrame) string {
	if frame.HasEvent {
		return fmt.Sprintf("{event=%q data=%q}", frame.Event, frame.Data)
	}
	return fmt.Sprintf("{data=%q}", frame.Data)
}
