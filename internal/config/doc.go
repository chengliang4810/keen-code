// Package config defines and persists the KeenCode user configuration under
// the Go data root (DefaultRoot, $HOME/.keencode/go-v1 by default):
//
//   - providers.json (schema "keencode/providers", version 1) — custom model
//     providers plus the active provider/model selection;
//   - settings.json (schema "keencode/app-settings", version 1) — theme,
//     default model, working directory, and tool permission policy.
//
// Both files follow the tolerant-loading contract of the Rust desktop stack
// (apps/desktop/src/providers.rs:773-1051 and apps/desktop/src/app_settings.rs):
//
//   - unknown or removed fields are ignored and reported as warnings — never
//     as errors;
//   - stale per-model provider entries and stale active selections are
//     dropped with warnings instead of failing the load;
//   - in providers.json a key differing from a known field only by ASCII
//     case (e.g. "apikey") is treated as a suspected typo and blocks
//     loading, because it would silently drop authentication;
//   - structural errors (invalid JSON, schema/version drift, wrong types,
//     non-canonical values) fail closed in providers.json and fall back to
//     defaults in settings.json, which must never block startup;
//   - saving preserves unknown fields found in the existing file instead of
//     dropping them, and refuses to overwrite an existing file that is not
//     valid JSON.
//
// Provider base URLs carry one more piece of semantics: a single trailing
// '#' marks an exact request path. The marker persists verbatim, resolution
// (ProviderRecord.Endpoint) strips it together with the protocol endpoint
// suffix, and a marker URL must end with the chosen protocol's generation
// endpoint so the runtime never appends a path to the wrong address
// (providers.rs:1305-1350).
//
// This package uses only the Go standard library and takes no dependencies
// on other internal packages, so the app layer can map its types onto the
// runtime and provider adapters.
package config
