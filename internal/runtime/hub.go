package runtime

import (
	"sync"
)

// hub is the per-session event broadcaster. Dispatch never blocks: every
// subscriber owns an unbounded pending queue drained by a dedicated pump
// goroutine, so a slow consumer only grows its own queue
// (docs/go-migration.md §5.5; Go equivalent of the Rust broadcast publisher
// in core/runtime/src/publisher.rs:133-237 without a lag signal).
//
// The seamless replay contract lives in the Session: subscription and
// dispatch take the same session mutex, so the history preload of a new
// subscriber and the first live event can never interleave or drop.
type hub struct {
	mu     sync.Mutex
	subs   map[int64]*hubSubscriber
	next   int64
	closed bool
}

// hubSubscriber is one subscription: a consumer channel fed by a pump
// goroutine from an unbounded pending queue.
type hubSubscriber struct {
	id       int64
	ch       chan Event
	mu       sync.Mutex
	pending  []Event
	wake     chan struct{}
	done     chan struct{}
	doneOnce sync.Once
}

// newHub returns an empty broadcaster.
func newHub() *hub {
	return &hub{subs: make(map[int64]*hubSubscriber)}
}

// subscribe registers a subscriber preloaded with the given history events
// (flagged Replay). Callers that must guarantee the seam against concurrent
// dispatch hold their own ordering lock around subscribe and publish.
func (h *hub) subscribe(history []Event) *hubSubscriber {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		// Degenerate subscription: an already-closed consumer channel and a
		// cancel that stays inert.
		ch := make(chan Event)
		close(ch)
		return &hubSubscriber{
			ch:   ch,
			wake: make(chan struct{}, 1),
			done: make(chan struct{}),
		}
	}
	h.next++
	s := &hubSubscriber{
		id:   h.next,
		ch:   make(chan Event, 256),
		wake: make(chan struct{}, 1),
		done: make(chan struct{}),
	}
	h.subs[s.id] = s
	for _, e := range history {
		e.Replay = true
		s.enqueue(e.clone())
	}
	go s.pump()
	return s
}

// publish fans events out to every subscriber without ever blocking on a
// consumer. Events are cloned per subscriber; after close the call is a
// no-op.
func (h *hub) publish(events ...Event) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	for _, s := range h.subs {
		for _, e := range events {
			s.enqueue(e.clone())
		}
	}
}

// cancel unsubscribes: the subscriber is detached (no further dispatch
// reaches it), the pump stops delivering and the channel closes promptly —
// undelivered queue content is dropped, the journal stays the source of
// truth. Idempotent.
func (h *hub) cancel(s *hubSubscriber) {
	h.mu.Lock()
	if h.subs != nil {
		delete(h.subs, s.id)
	}
	h.mu.Unlock()
	s.beginClose()
}

// close detaches every subscriber: pumps stop and consumer channels close
// promptly (queued events are not guaranteed to drain). Later publishes
// are dropped.
func (h *hub) close() {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	h.closed = true
	for _, s := range h.subs {
		s.beginClose()
	}
	h.subs = make(map[int64]*hubSubscriber)
}

// count reports the number of live subscribers (diagnostics and tests).
func (h *hub) count() int {
	h.mu.Lock()
	defer h.mu.Unlock()
	return len(h.subs)
}

// enqueue appends events to the pending queue and wakes the pump.
func (s *hubSubscriber) enqueue(events ...Event) {
	s.mu.Lock()
	s.pending = append(s.pending, events...)
	s.mu.Unlock()
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

// beginClose signals the pump to drain and exit.
func (s *hubSubscriber) beginClose() {
	s.doneOnce.Do(func() { close(s.done) })
}

// Channel exposes the consumer side of the subscription. It closes after
// cancel or hub close, once every queued event has been delivered.
func (s *hubSubscriber) Channel() <-chan Event { return s.ch }

// pump drains the pending queue into the consumer channel. A blocked send
// only grows the pending queue; dispatch (enqueue) never waits.
func (s *hubSubscriber) pump() {
	defer close(s.ch)
	for {
		s.mu.Lock()
		pending := s.pending
		s.pending = nil
		s.mu.Unlock()
		for _, e := range pending {
			select {
			case s.ch <- e:
			case <-s.done:
				// Cancelled mid-queue: the consumer opted out, stop
				// delivering.
				return
			}
		}
		select {
		case <-s.wake:
		case <-s.done:
			s.mu.Lock()
			remaining := len(s.pending)
			s.mu.Unlock()
			if remaining == 0 {
				return
			}
		}
	}
}
