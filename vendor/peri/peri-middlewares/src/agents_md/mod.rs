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

/// AgentsMdMiddleware - injects global and project instruction files.
///
/// During `before_agent`, instruction files are searched in priority order and
/// their content is exposed as a system-prompt contribution.
///
/// Merge order:
/// 1. `{home}/.keencode/AGENTS.md` (always included when non-empty)
/// 2. The first non-empty project file: `{cwd}/AGENTS.md`,
///    `{cwd}/CLAUDE.md`, or `{cwd}/.agents/AGENTS.md`
/// 3. `{cwd}/CLAUDE.local.md`
pub struct AgentsMdMiddleware {
    home_dir: Option<PathBuf>,
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
            #[cfg(not(test))]
            home_dir: dirs_next::home_dir(),
            #[cfg(test)]
            home_dir: None,
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

    #[cfg(test)]
    fn with_home_dir(mut self, home_dir: Option<PathBuf>) -> Self {
        self.home_dir = home_dir;
        self
    }

    /// Set glob patterns that exclude project instruction files.
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

    /// Read and freeze global and project instruction content once.
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
        let global_content = Self::global_path(home)
            .filter(|path| path.is_file())
            .and_then(|path| Self::read_main_content(&path).ok().flatten());
        let project_content = Self::project_candidate_paths_for(cwd)
            .into_iter()
            .filter(|path| path.is_file())
            .find_map(|path| Self::read_main_content(&path).ok().flatten());
        let main_content = {
            let contents = [global_content, project_content]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            (!contents.is_empty()).then(|| contents.join("\n\n"))
        };
        let local_path = Path::new(cwd).join("CLAUDE.local.md");
        let local_content = local_path
            .is_file()
            .then(|| Self::read_non_empty_content(&local_path).ok().flatten())
            .flatten();
        (main_content, local_content)
    }

    fn global_path(home: Option<&Path>) -> Option<PathBuf> {
        home.map(|home| home.join(".keencode").join("AGENTS.md"))
    }

    fn project_candidate_paths_for(cwd: &str) -> Vec<PathBuf> {
        vec![
            Path::new(cwd).join("AGENTS.md"),
            Path::new(cwd).join("CLAUDE.md"),
            Path::new(cwd).join(".agents").join("AGENTS.md"),
        ]
    }

    fn read_non_empty_content(path: &Path) -> std::io::Result<Option<String>> {
        let content = std::fs::read_to_string(path)?;
        Ok((!content.trim().is_empty()).then_some(content))
    }

    fn read_main_content(path: &Path) -> std::io::Result<Option<String>> {
        let Some(content) = Self::read_non_empty_content(path)? else {
            return Ok(None);
        };
        let is_claude_md = path
            .file_name()
            .map(|name| name.to_string_lossy().starts_with("CLAUDE"))
            .unwrap_or(false);
        if !is_claude_md {
            return Ok(Some(content));
        }
        let mut visited = HashSet::new();
        if let Ok(canonical) = path.canonicalize() {
            visited.insert(canonical);
        }
        Ok(Some(resolve_imports(
            &content,
            path.parent().unwrap_or(Path::new(".")),
            3,
            &mut visited,
        )))
    }

    /// Build the project candidate path list for `cwd` (default paths plus extras).
    fn project_candidate_paths(&self, cwd: &str) -> Vec<PathBuf> {
        let mut candidates = Self::project_candidate_paths_for(cwd);
        candidates.extend(self.extra_search_paths.iter().cloned());
        candidates
    }

    /// Build the global and project instruction contribution string.
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

        let global_path = Self::global_path(self.home_dir.as_deref()).filter(|path| path.is_file());
        let project_paths = self
            .project_candidate_paths(cwd)
            .into_iter()
            .filter(|path| {
                path.is_file()
                    && !self.excludes.iter().any(|pattern| {
                        glob::Pattern::new(pattern)
                            .map(|glob| glob.matches(&path.to_string_lossy()))
                            .unwrap_or(false)
                    })
            })
            .collect::<Vec<_>>();
        let local_path = Path::new(cwd).join("CLAUDE.local.md");
        let contents = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<String>> {
            let mut contents = Vec::new();
            if let Some(path) = global_path {
                if let Some(content) = Self::read_main_content(&path)? {
                    contents.push(content);
                }
            }
            for path in project_paths {
                if let Some(content) = Self::read_main_content(&path)? {
                    contents.push(content);
                    break;
                }
            }
            if local_path.is_file() {
                if let Some(content) = Self::read_non_empty_content(&local_path)? {
                    contents.push(content);
                }
            }
            Ok(contents)
        })
        .await
        .map_err(|error| peri_agent::error::AgentError::MiddlewareError {
            middleware: "AgentsMdMiddleware".to_string(),
            reason: format!("spawn_blocking failed: {error}"),
        })?
        .map_err(|error| peri_agent::error::AgentError::MiddlewareError {
            middleware: "AgentsMdMiddleware".to_string(),
            reason: format!("failed to read instruction file: {error}"),
        })?;

        Ok((!contents.is_empty()).then(|| contents.join("\n\n")))
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
