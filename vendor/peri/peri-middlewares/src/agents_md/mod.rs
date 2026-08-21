use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

/// AgentsMdMiddleware - injects project instruction files (AGENTS.md / CLAUDE.md).
///
/// During `before_agent`, instruction files are searched in priority order and
/// their content is exposed as a system-prompt contribution.
///
/// Search priority:
/// 1. `{cwd}/AGENTS.md`
/// 2. `{cwd}/CLAUDE.md`
/// 3. `{cwd}/.agents/AGENTS.md`
/// 4. `{home}/.keencode/AGENTS.md` (user-level)
pub struct AgentsMdMiddleware {
    extra_search_paths: Vec<PathBuf>,
    excludes: Vec<String>,
    /// Frozen CLAUDE.md main content (resolved @import). When set, skip disk read.
    frozen_main: Option<String>,
    /// Frozen CLAUDE.local.md content.
    frozen_local: Option<String>,
    /// Cached prompt contribution (populated in before_agent, returned by prompt_contribution).
    cached_contribution: Arc<RwLock<Option<String>>>,
}

impl AgentsMdMiddleware {
    pub fn new() -> Self {
        Self {
            extra_search_paths: Vec::new(),
            excludes: Vec::new(),
            frozen_main: None,
            frozen_local: None,
            cached_contribution: Arc::new(RwLock::new(None)),
        }
    }

    /// Add application-provided search paths.
    pub fn with_extra_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.extra_search_paths = paths;
        self
    }

    /// Set glob patterns that exclude CLAUDE.md files.
    pub fn with_excludes(mut self, patterns: Vec<String>) -> Self {
        self.excludes = patterns;
        self
    }

    /// Inject frozen instruction content and optional CLAUDE.local.md content.
    ///
    /// When set, `before_agent` skips disk I/O entirely and uses the frozen
    /// content directly.
    pub fn with_frozen_content(mut self, main: Option<String>, local: Option<String>) -> Self {
        // Populate cached_contribution at construction time so
        // prompt_contribution is available before before_agent runs (the
        // SubAgent chain collects contributions during construction).
        // Keep frozen_main / frozen_local so before_agent can still use the
        // same build_contribution path.
        let mut combined = main.clone().unwrap_or_default();
        if let Some(ref local) = local {
            if !local.trim().is_empty() {
                if !combined.trim().is_empty() {
                    combined.push_str("\n\n");
                }
                combined.push_str(local);
            }
        }
        if !combined.trim().is_empty() {
            *self.cached_contribution.write().unwrap() = Some(combined);
        }
        self.frozen_main = main;
        self.frozen_local = local;
        self
    }

    /// Read and freeze CLAUDE.md content once (with @import resolution).
    ///
    /// Returns `(main_content, local_content)`, either may be `None`.
    /// Called at session creation so the content never drifts mid-session.
    pub fn read_frozen_content(cwd: &str) -> (Option<String>, Option<String>) {
        let home = dirs_next::home_dir();
        Self::read_frozen_content_with_home(cwd, home.as_deref())
    }

    fn read_frozen_content_with_home(
        cwd: &str,
        home: Option<&Path>,
    ) -> (Option<String>, Option<String>) {
        let candidates = Self::candidate_paths_for(cwd, home);
        let main_content = candidates
            .into_iter()
            .find(|p| p.is_file())
            .and_then(|path| {
                let content = std::fs::read_to_string(&path).ok()?;
                if content.trim().is_empty() {
                    return None;
                }
                let is_claude_md = path
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with("CLAUDE"))
                    .unwrap_or(false);
                if is_claude_md {
                    let dir = path.parent().unwrap_or(Path::new("."));
                    let mut visited = HashSet::new();
                    if let Ok(canonical) = path.canonicalize() {
                        visited.insert(canonical);
                    }
                    Some(resolve_imports(&content, dir, 3, &mut visited))
                } else {
                    Some(content)
                }
            });
        let local_content = {
            let local_path = Path::new(cwd).join("CLAUDE.local.md");
            if local_path.is_file() {
                let c = std::fs::read_to_string(&local_path).unwrap_or_default();
                if c.trim().is_empty() {
                    None
                } else {
                    Some(c)
                }
            } else {
                None
            }
        };
        (main_content, local_content)
    }

    fn candidate_paths_for(cwd: &str, home: Option<&Path>) -> Vec<PathBuf> {
        let mut candidates = vec![
            Path::new(cwd).join("AGENTS.md"),
            Path::new(cwd).join("CLAUDE.md"),
            Path::new(cwd).join(".agents").join("AGENTS.md"),
        ];
        if let Some(home) = home {
            candidates.push(home.join(".keencode").join("AGENTS.md"));
        }
        candidates
    }

    /// Build the candidate path list for `cwd` (default paths plus extras).
    fn candidate_paths(&self, cwd: &str) -> Vec<PathBuf> {
        let home = dirs_next::home_dir();
        let mut candidates = Self::candidate_paths_for(cwd, home.as_deref());

        candidates.extend(self.extra_search_paths.iter().cloned());

        candidates
    }

    /// Find the first existing file in priority order, excluding matching paths.
    fn find_file(&self, cwd: &str) -> Option<PathBuf> {
        self.candidate_paths(cwd).into_iter().find(|p| {
            if !p.is_file() {
                return false;
            }
            if self.excludes.is_empty() {
                return true;
            }
            let path_str = p.to_string_lossy();
            !self.excludes.iter().any(|pat| {
                glob::Pattern::new(pat)
                    .map(|g| g.matches(&path_str))
                    .unwrap_or(false)
            })
        })
    }

    /// Build the CLAUDE.md / AGENTS.md contribution string.
    /// When frozen content is set, uses it directly (no disk I/O).
    async fn build_contribution(&self, cwd: &str) -> AgentResult<Option<String>> {
        // Use frozen content when available — skip all disk I/O. A local-only
        // CLAUDE.local.md is a valid frozen state as well.
        if self.frozen_main.is_some() || self.frozen_local.is_some() {
            let mut content = self.frozen_main.clone().unwrap_or_default();
            if let Some(ref local) = self.frozen_local {
                if !local.trim().is_empty() {
                    if !content.trim().is_empty() {
                        content.push_str("\n\n");
                    }
                    content.push_str(local);
                }
            }
            if !content.trim().is_empty() {
                return Ok(Some(content));
            }
            return Ok(None);
        }

        let Some(path) = self.find_file(cwd) else {
            // Even without a main file, try to read CLAUDE.local.md.
            let local_path = Path::new(cwd).join("CLAUDE.local.md");
            if local_path.is_file() {
                let lp = local_path.clone();
                let local_content =
                    tokio::task::spawn_blocking(move || std::fs::read_to_string(&lp))
                        .await
                        .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                            middleware: "AgentsMdMiddleware".to_string(),
                            reason: format!("spawn_blocking failed: {e}"),
                        })?
                        .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                            middleware: "AgentsMdMiddleware".to_string(),
                            reason: format!("failed to read CLAUDE.local.md: {e}"),
                        })?;
                if !local_content.trim().is_empty() {
                    return Ok(Some(local_content));
                }
            }
            return Ok(None);
        };

        let path_display = path.display().to_string();
        let is_claude_md = path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("CLAUDE"))
            .unwrap_or(false);
        let import_dir = path.parent().map(|p| p.to_path_buf());
        let main_file_canonical = path.canonicalize().ok();
        let content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path))
            .await
            .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                middleware: "AgentsMdMiddleware".to_string(),
                reason: format!("spawn_blocking failed: {e}"),
            })?
            .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                middleware: "AgentsMdMiddleware".to_string(),
                reason: format!("failed to read {}: {e}", path_display),
            })?;

        let content = if content.trim().is_empty() {
            return Ok(None);
        } else {
            content
        };

        // Append CLAUDE.local.md (project-local and not committed).
        let local_path = Path::new(cwd).join("CLAUDE.local.md");
        let content = if local_path.is_file() {
            let lp = local_path.clone();
            let local_content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&lp))
                .await
                .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                    middleware: "AgentsMdMiddleware".to_string(),
                    reason: format!("spawn_blocking failed: {e}"),
                })?
                .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                    middleware: "AgentsMdMiddleware".to_string(),
                    reason: format!("failed to read CLAUDE.local.md: {e}"),
                })?;
            if local_content.trim().is_empty() {
                content
            } else {
                format!("{content}\n\n{local_content}")
            }
        } else {
            content
        };

        // Resolve @import only for CLAUDE.md files (not AGENTS.md).
        let content = if is_claude_md {
            let dir = import_dir.as_deref().unwrap_or(Path::new(cwd));
            let mut visited = HashSet::new();
            if let Some(canonical) = main_file_canonical {
                visited.insert(canonical);
            }
            resolve_imports(&content, dir, 3, &mut visited)
        } else {
            content
        };

        Ok(Some(content))
    }
}

/// Recursively resolve `<!-- @import path -->` references, replacing each
/// reference with the imported file content.
/// `base_dir` is the directory containing the file with the @import.
/// Recursion is limited to depth 3; `visited` prevents cycles.
pub(crate) fn resolve_imports(
    content: &str,
    base_dir: &Path,
    depth: u32,
    visited: &mut HashSet<PathBuf>,
) -> String {
    if depth == 0 {
        return content.to_string();
    }
    let mut result = String::with_capacity(content.len());
    let mut pos = 0;
    while pos < content.len() {
        if let Some(offset) = content[pos..].find("<!-- @import ") {
            let abs_pos = pos + offset;
            result.push_str(&content[pos..abs_pos]);
            // Extract the path between "<!-- @import " and " -->".
            let after = &content[abs_pos + 13..]; // 13 = "<!-- @import ".len()
            if let Some(end) = after.find(" -->") {
                let import_path = after[..end].trim();
                let resolved = base_dir
                    .join(import_path)
                    .canonicalize()
                    .unwrap_or_else(|_| base_dir.join(import_path));
                if visited.contains(&resolved) || !resolved.is_file() {
                    // Preserve the placeholder for cycles and missing files.
                    result.push_str(&content[abs_pos..abs_pos + 13 + end + 4]);
                } else {
                    visited.insert(resolved.clone());
                    let imported_content = std::fs::read_to_string(&resolved).unwrap_or_default();
                    let import_dir = resolved.parent().unwrap_or(base_dir);
                    let resolved_content =
                        resolve_imports(&imported_content, import_dir, depth - 1, visited);
                    result.push_str(&resolved_content);
                }
                pos = abs_pos + 13 + end + 4; // 4 = " -->".len()
            } else {
                // No closing " -->": this is not a valid @import; preserve it.
                result.push_str("<!-- @import ");
                pos = abs_pos + 13;
            }
        } else {
            result.push_str(&content[pos..]);
            break;
        }
    }
    result
}

impl Default for AgentsMdMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for AgentsMdMiddleware {
    fn name(&self) -> &str {
        "AgentsMdMiddleware"
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.cached_contribution.read().unwrap().clone()
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        let contribution = self.build_contribution(state.cwd()).await?;
        *self.cached_contribution.write().unwrap() = contribution;
        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
