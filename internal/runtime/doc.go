// Package runtime owns session lifecycle and persistence: creation,
// listing, rename and deletion, the append-only JSONL journal with startup
// replay and truncated-tail recovery, per-session event subscription, and
// per-session turn scheduling against a TurnRunner (the agent loop is
// injected as an interface so runtime and agent are implemented and tested
// independently).
//
// The journal record envelope and the durability discipline port
// core/resources (SessionEventRecord, append_idempotent with event-id
// idempotency and a sequence CAS, 64-record/100ms batched fsync, cross
// process append lock, evidence-preserving truncated-tail repair) into the
// fresh Go v1 data namespace.
package runtime
