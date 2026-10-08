// Package model defines the provider-neutral model domain layer shared by the
// agent loop and every protocol adapter: messages and content blocks, request
// parameters, stream events, token usage, response shapes, normalized errors,
// and the Provider boundary.
//
// The package mirrors the semantics of the Rust core/model crate
// (core/model/src/*.rs) with the reduced v1 type surface fixed by
// docs/go-migration.md §5.1/§5.2: tool results are carried in user messages
// instead of a dedicated role, reasoning continuation state is an opaque
// signature string, and usage is reported with three counters where -1 means
// "not reported".
//
// The package depends on no other internal package and on no third-party
// module; only the Go standard library is used.
package model
