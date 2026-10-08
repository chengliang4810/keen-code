package anthropic

import (
	"bytes"
	"unicode/utf8"
)

// sseFrame is one fully delimited Server-Sent Event (core/provider/src/sse.rs).
type sseFrame struct {
	// event is the optional "event:" field; nil when the frame carried none.
	event *string
	// data is every "data:" field joined with newlines per the SSE rules.
	data string
}

// utf8BOM is the byte order mark stripped once at the very start of the
// stream.
var utf8BOM = []byte{0xEF, 0xBB, 0xBF}

// sseDecoder is an incremental decoder that supports arbitrary byte chunking,
// CRLF and bare-CR line endings, multi-line data fields, and a per-event byte
// ceiling. It is a direct port of the Rust SseDecoder
// (core/provider/src/sse.rs:13-169) so wire behavior stays identical.
type sseDecoder struct {
	buffer        []byte
	event         *string
	data          bytes.Buffer
	hasData       bool
	atStreamStart bool
	// pendingCR records that the previous byte was a bare CR; a following LF
	// is consumed as part of the same line break.
	pendingCR bool
	// maxEventBytes caps the accumulated bytes of one in-flight event.
	maxEventBytes int
}

// newSSEDecoder returns a decoder enforcing the given single-event byte
// ceiling.
func newSSEDecoder(maxEventBytes int) *sseDecoder {
	return &sseDecoder{atStreamStart: true, maxEventBytes: maxEventBytes}
}

// push appends one network byte chunk and returns the events completed inside
// it.
func (d *sseDecoder) push(chunk []byte) ([]sseFrame, error) {
	var frames []sseFrame
	for _, b := range chunk {
		if d.pendingCR {
			d.pendingCR = false
			if b == '\n' {
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
			// Validate the ceiling before copying, one byte at a time without
			// allocating a wrapper slice.
			if len(d.buffer)+1+d.eventSize()+d.data.Len() > d.maxEventBytes {
				return nil, d.tooLargeError()
			}
			d.buffer = append(d.buffer, b)
		}
	}
	return frames, nil
}

// finish processes the final unterminated field and the last undelimited
// event when the HTTP body ends.
func (d *sseDecoder) finish() ([]sseFrame, error) {
	var frames []sseFrame
	if len(d.buffer) > 0 {
		line := d.takeBuffer()
		if err := d.consumeLine(line, &frames); err != nil {
			return nil, err
		}
	}
	if d.event != nil || d.hasData {
		frames = append(frames, d.takeFrame())
	}
	return frames, nil
}

// takeBuffer hands out the accumulated line bytes and resets the buffer.
func (d *sseDecoder) takeBuffer() []byte {
	line := d.buffer
	d.buffer = nil
	return line
}

// consumeLine interprets one SSE field line and closes an event on empty
// lines.
func (d *sseDecoder) consumeLine(line []byte, frames *[]sseFrame) error {
	if d.atStreamStart {
		d.atStreamStart = false
		line = bytes.TrimPrefix(line, utf8BOM)
	}
	if !utf8.Valid(line) {
		return protocolError("SSE 字段不是有效 UTF-8")
	}
	if len(line) == 0 {
		if d.event != nil || d.hasData {
			*frames = append(*frames, d.takeFrame())
		}
		return nil
	}
	if line[0] == ':' {
		// Comment lines keep the connection alive; they never frame events.
		return nil
	}
	field, value := line, []byte(nil)
	if index := bytes.IndexByte(line, ':'); index >= 0 {
		field, value = line[:index], line[index+1:]
	}
	if len(value) > 0 && value[0] == ' ' {
		value = value[1:]
	}
	switch string(field) {
	case "event":
		if len(value) == 0 {
			// An empty event field resets the previous event name per the
			// SSE specification.
			d.event = nil
		} else {
			text := string(value)
			d.event = &text
		}
	case "data":
		if d.hasData {
			d.data.WriteByte('\n')
		}
		d.data.Write(value)
		d.hasData = true
	default:
		// "id", "retry", and unknown fields are ignored, matching the Rust
		// decoder and the SSE user-agent rules.
	}
	return d.checkSize()
}

// takeFrame finalizes the in-flight event and resets its state, matching the
// Rust take_frame: the event name is always taken and hasData always resets.
func (d *sseDecoder) takeFrame() sseFrame {
	d.hasData = false
	data := d.data.String()
	d.data.Reset()
	event := d.event
	d.event = nil
	return sseFrame{event: event, data: data}
}

// checkSize validates the assembled event body against the configured byte
// ceiling.
func (d *sseDecoder) checkSize() error {
	if len(d.buffer)+d.eventSize()+d.data.Len() > d.maxEventBytes {
		return d.tooLargeError()
	}
	return nil
}

// eventSize returns the retained size of the in-flight event name.
func (d *sseDecoder) eventSize() int {
	if d.event == nil {
		return 0
	}
	return len(*d.event)
}

// tooLargeError builds the stable over-limit error without echoing server
// payload.
func (d *sseDecoder) tooLargeError() error {
	return protocolError("SSE 事件超过 %d 字节安全上限", d.maxEventBytes)
}
