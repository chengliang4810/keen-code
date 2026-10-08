package runtime

import (
	"fmt"
	"testing"
	"time"
)

// collectEvents drains n events from a subscription with a deadline.
func collectEvents(t *testing.T, ch <-chan Event, n int) []Event {
	t.Helper()
	out := make([]Event, 0, n)
	deadline := time.After(2 * time.Second)
	for len(out) < n {
		select {
		case ev, ok := <-ch:
			if !ok {
				t.Fatalf("channel closed after %d/%d events", len(out), n)
			}
			out = append(out, ev)
		case <-deadline:
			t.Fatalf("timed out after %d/%d events", len(out), n)
		}
	}
	return out
}

func TestHubReplayThenLiveSeam(t *testing.T) {
	h := newHub()
	defer h.close()

	history := []Event{
		{ID: "h1", Type: EventUserMessage, Text: "历史一", Seq: 1},
		{ID: "h2", Type: EventTextDelta, Text: "历史二", Seq: 2},
	}
	sub := h.subscribe(history)

	// Live events published right after subscription must arrive after the
	// replayed history, without loss or duplication.
	h.publish(Event{ID: "l1", Type: EventTextDelta, Text: "直播一", Seq: 3})
	h.publish(Event{ID: "l2", Type: EventTextDelta, Text: "直播二", Seq: 4})

	got := collectEvents(t, sub.Channel(), 4)
	wantText := []string{"历史一", "历史二", "直播一", "直播二"}
	wantSeq := []int64{1, 2, 3, 4}
	for i, ev := range got {
		if ev.Text != wantText[i] || ev.Seq != wantSeq[i] {
			t.Fatalf("event %d = (%s, %d), want (%s, %d)", i, ev.Text, ev.Seq, wantText[i], wantSeq[i])
		}
		wantReplay := i < 2
		if ev.Replay != wantReplay {
			t.Fatalf("event %d Replay = %v, want %v", i, ev.Replay, wantReplay)
		}
	}
}

func TestHubSlowConsumerDoesNotBlockDispatch(t *testing.T) {
	h := newHub()
	defer h.close()
	sub := h.subscribe(nil)
	ch := sub.Channel()

	// Publish far more than the buffered channel holds without reading.
	const total = 5000
	for i := 0; i < total; i++ {
		h.publish(Event{ID: fmt.Sprintf("e%d", i), Type: EventTextDelta, Seq: int64(i + 1)})
	}
	got := collectEvents(t, ch, total)
	for i, ev := range got {
		if ev.Seq != int64(i+1) {
			t.Fatalf("event %d has seq %d, want %d", i, ev.Seq, i+1)
		}
	}
}

func TestHubMultipleSubscribersIndependent(t *testing.T) {
	h := newHub()
	defer h.close()
	a := h.subscribe(nil)
	b := h.subscribe(nil)
	h.publish(Event{ID: "x", Type: EventTextDelta, Text: "广播"})

	if len(collectEvents(t, a.Channel(), 1)) != 1 {
		t.Fatal("subscriber A missed the event")
	}
	if len(collectEvents(t, b.Channel(), 1)) != 1 {
		t.Fatal("subscriber B missed the event")
	}
}

func TestHubCancelClosesChannel(t *testing.T) {
	h := newHub()
	sub := h.subscribe(nil)
	ch := sub.Channel()
	h.cancel(sub)
	select {
	case _, ok := <-ch:
		if ok {
			t.Fatal("expected closed channel")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("channel did not close after cancel")
	}
	if got := h.count(); got != 0 {
		t.Fatalf("hub count after cancel = %d, want 0", got)
	}
}

func TestHubCloseClosesChannels(t *testing.T) {
	h := newHub()
	sub := h.subscribe([]Event{{ID: "h", Type: EventUserMessage, Text: "历史"}})
	ch := sub.Channel()
	const extra = 100
	for i := 0; i < extra; i++ {
		h.publish(Event{ID: "x", Type: EventTextDelta, Text: "排队", Seq: int64(i + 1)})
	}
	h.close()

	// The channel closes promptly; queued events may or may not drain (the
	// journal stays the source of truth).
	delivered := 0
	deadline := time.After(2 * time.Second)
	for {
		select {
		case _, ok := <-ch:
			if !ok {
				if delivered > extra+1 { // +1: the history preload event
					t.Fatalf("delivered %d events, more than published", delivered)
				}
				return
			}
			delivered++
		case <-deadline:
			t.Fatal("channel did not close after hub close")
		}
	}
}

// TestHubSubscriberIsolation pins the deep-copy contract: a consumer
// mutating received pointer payloads must not corrupt the broadcast source.
func TestHubSubscriberIsolation(t *testing.T) {
	h := newHub()
	sub := h.subscribe(nil)
	source := Event{ID: "t", Type: EventToolStart, Tool: &ToolEvent{CallID: "c1"}}
	h.publish(source)
	got := collectEvents(t, sub.Channel(), 1)[0]
	got.Tool.CallID = "mutated"
	if source.Tool.CallID != "c1" {
		t.Fatalf("source mutated: %+v", source.Tool)
	}
}
