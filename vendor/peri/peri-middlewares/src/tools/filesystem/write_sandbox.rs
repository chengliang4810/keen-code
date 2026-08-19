//! WriteSandbox 工具——只允许写入 frontmatter 声明的沙箱目录白名单。
//!
//! 用于 readonly subagent（如 plan），给它们一个能力最小化的写入通道：
//! 只能写沙箱目录内的文件，不能碰项目代码。路径安全通过词法校验 +
//! canonicalize 前缀匹配实现 symlink 逃逸防护。

use peri_agent::tools::BaseTool;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::draft::{draft_hint_zh, DraftStore};

const WRITE_SANDBOX_DESC_PREFIX: &str = "Write a file ONLY into your sandbox directories: ";

const WRITE_SANDBOX_DESC_SUFFIX: &str = r#"
 Paths are relative to the project root. Overwriting is allowed.
 Absolute paths and '..' are rejected.
 Do NOT use this tool for files outside the sandbox directories listed above."#;

/// 外部沙箱基目录环境变量。设置后，构造工具时将方案/报告文件写入
/// 应用数据目录而非项目内 `.peri/` 等路径。
const PERI_SANDBOX_WRITE_BASE_ENV: &str = "PERI_SANDBOX_WRITE_BASE";

const WRITE_SANDBOX_DESC_EXTERNAL_PREFIX: &str = "Write a file ONLY into your sandbox directory: ";

const WRITE_SANDBOX_DESC_EXTERNAL_SUFFIX: &str = r#"
 Paths are relative to the sandbox directory declared in its tool description.
 Overwriting is allowed. Absolute paths and '..' are rejected.
 Do NOT use this tool for files outside the sandbox directory."#;

/// 读取外部沙箱基目录（桌面设置 `PERI_SANDBOX_WRITE_BASE` 指向应用数据目录）。
///
/// 返回 `Some(base)` 表示外部模式：所有项目的方案/报告文件统一写入
/// `base/<项目键>/`；`None` 为项目模式（原始行为）。
///
/// 仅接受非空绝对路径；相对路径或空值视为配置错误，返回 `None`。
fn external_base_from_env() -> Option<PathBuf> {
    std::env::var_os(PERI_SANDBOX_WRITE_BASE_ENV)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// 为项目派生稳定的沙箱子目录键（外部沙箱模式）。
///
/// 格式 `<清洗目录名>-<哈希前 8 位>`，保证：同项目同键、异项目高概率异键。
/// 先归一化路径（统一分隔符 + trim 尾斜杠）再哈希，避免 `/x/` 与 `/x` 分歧。
pub(crate) fn project_sandbox_key(cwd: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // 归一化：统一分隔符 + trim 尾斜杠（保证 "/x/" 与 "/x" 同键）
    let normalized = cwd.replace('\\', "/").trim_end_matches('/').to_string();

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    let hash = hasher.finish();
    let hash_hex = format!("{:08x}", (hash & 0xFFFF_FFFF) as u32);

    let raw_name = Path::new(&normalized)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let sanitized: String = raw_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let name = if sanitized.is_empty() {
        "project"
    } else {
        &sanitized
    };

    format!("{}-{}", name, hash_hex)
}

/// 沙箱写工具——只能写入构造时指定的目录白名单。
pub struct WriteSandboxTool {
    /// 工作目录（项目根或外部沙箱基目录）
    pub cwd: String,
    /// canonicalized 沙箱根路径列表（构造时已校验合法性）
    sandbox_roots: Vec<PathBuf>,
    /// 原始相对路径列表（用于错误消息展示，避免绝对路径与工具要求矛盾）
    allowed_dirs: Vec<String>,
    /// 动态生成的 description
    description: String,
    /// 失败草稿存储(进程级内存);None = PERI_WRITE_DRAFT=0 关闭
    drafts: Option<Arc<Mutex<DraftStore>>>,
    /// 路径基准（项目模式=canonicalize 项目根；外部模式=canonicalize 沙箱根）
    path_base: PathBuf,
}

impl WriteSandboxTool {
    /// 构造 WriteSandbox 工具。
    ///
    /// `allowed_dirs` 是 frontmatter 声明的相对目录列表（基于 cwd）。
    /// 目录不存在时自动创建，创建失败才报错。
    pub fn new(
        cwd: impl Into<String>,
        allowed_dirs: Vec<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_draft(cwd, allowed_dirs, super::draft::draft_enabled())
    }

    /// 测试注入构造;全部现有构造逻辑(沙箱目录自动创建/canonicalize/description)原样保留。
    /// `enabled=false` 时完全禁用草稿(不创建 store)。
    pub(crate) fn with_draft(
        cwd: impl Into<String>,
        allowed_dirs: Vec<String>,
        enabled: bool,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_draft_and_base(cwd, allowed_dirs, enabled, external_base_from_env())
    }

    /// 内部统一构造，支持显式 `external_base`（外部沙箱模式）。
    ///
    /// `external_base = Some(base)` 时，方案/报告写入 `base/<项目键>/`；
    /// `None` 为项目模式（原始行为：沙箱目录在项目内）。
    fn with_draft_and_base(
        cwd: impl Into<String>,
        allowed_dirs: Vec<String>,
        enabled: bool,
        external_base: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let cwd_raw = cwd.into();
        if let Some(base) = external_base {
            Self::build_external(&cwd_raw, base, enabled)
        } else {
            Self::build_project(cwd_raw, allowed_dirs, enabled)
        }
    }

    /// 外部沙箱模式构造（桌面设置 `PERI_SANDBOX_WRITE_BASE` 指向应用数据目录）。
    ///
    /// 方案/报告文件统一写入 `base/<项目键>/`，按项目划分沙箱，不在项目内产生写入。
    fn build_external(
        cwd_raw: &str,
        base: PathBuf,
        enabled: bool,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let key = project_sandbox_key(cwd_raw);
        let sandbox = base.join(&key);
        std::fs::create_dir_all(&sandbox).map_err(|e| {
            format!(
                "WriteSandbox 外部沙箱：无法创建沙箱目录 '{}': {}",
                sandbox.display(),
                e
            )
        })?;
        let canonical = sandbox.canonicalize().map_err(|e| {
            format!(
                "WriteSandbox 外部沙箱：无法 canonicalize 沙箱目录 '{}': {}",
                sandbox.display(),
                e
            )
        })?;

        // 外部模式 description 单数形式，不暴露项目内路径
        let description = format!(
            "{}{}{}",
            WRITE_SANDBOX_DESC_EXTERNAL_PREFIX,
            canonical.display(),
            WRITE_SANDBOX_DESC_EXTERNAL_SUFFIX
        );

        Ok(Self {
            cwd: cwd_raw.to_string(),
            sandbox_roots: vec![canonical.clone()],
            allowed_dirs: Vec::new(),
            description,
            drafts: enabled.then(|| Arc::new(Mutex::new(DraftStore::new()))),
            path_base: canonical,
        })
    }

    /// 项目沙箱模式构造（原始行为：沙箱目录在项目内 `.peri/plans/` 等）。
    fn build_project(
        cwd_raw: String,
        allowed_dirs: Vec<String>,
        enabled: bool,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // 构造时 canonicalize cwd，确保后续前缀校验和 strip_prefix 同源。
        let cwd_path = Path::new(&cwd_raw)
            .canonicalize()
            .map_err(|error| format!("WriteSandbox: 无法定位项目根 '{}': {error}", cwd_raw))?;
        let cwd = cwd_path.display().to_string();
        // 构造沙箱根路径：目录不存在则自动创建
        let mut sandbox_roots = Vec::new();
        for dir in &allowed_dirs {
            validate_allowed_directory(dir)?;
            let raw = cwd_path.join(dir);
            // 创建前先校验最长已存在祖先，避免 create_dir_all 跟随项目内
            // 符号链接或 junction，在项目外产生目录副作用。
            let mut existing_ancestor = raw.as_path();
            while !existing_ancestor.exists() {
                existing_ancestor = existing_ancestor.parent().ok_or_else(|| {
                    format!("WriteSandbox: 无法定位沙箱目录 '{}' 的已有祖先", dir)
                })?;
            }
            let canonical_ancestor = existing_ancestor.canonicalize().map_err(|error| {
                format!(
                    "WriteSandbox: 无法 canonicalize 沙箱目录 '{}' 的已有祖先: {}",
                    dir, error
                )
            })?;
            if canonical_ancestor != cwd_path && !canonical_ancestor.starts_with(&cwd_path) {
                return Err(format!(
                    "WriteSandbox: 沙箱目录 '{}' 的已有祖先不在项目根目录内",
                    dir
                )
                .into());
            }
            // 目录不存在则自动创建（避免 subagent 启动时因目录不存在而缺少工具）
            if !raw.exists() {
                std::fs::create_dir_all(&raw)
                    .map_err(|e| format!("WriteSandbox: 无法创建沙箱目录 '{}': {}", dir, e))?;
            }
            let canonical = raw.canonicalize().map_err(|e| {
                format!("WriteSandbox: 无法 canonicalize 沙箱目录 '{}': {}", dir, e)
            })?;
            // 确保是目录
            if !canonical.is_dir() {
                return Err(format!("WriteSandbox: 沙箱路径 '{}' 不是目录", dir).into());
            }
            if canonical == cwd_path || !canonical.starts_with(&cwd_path) {
                return Err(
                    format!("WriteSandbox: 沙箱目录 '{}' 必须严格位于项目根目录内", dir).into(),
                );
            }
            sandbox_roots.push(canonical);
        }

        // 构造动态 description
        let dirs_display = allowed_dirs.join(", ");
        let description = format!(
            "{}{}{}",
            WRITE_SANDBOX_DESC_PREFIX, dirs_display, WRITE_SANDBOX_DESC_SUFFIX
        );

        Ok(Self {
            cwd: cwd.clone(),
            sandbox_roots,
            allowed_dirs,
            description,
            drafts: enabled.then(|| Arc::new(Mutex::new(DraftStore::new()))),
            path_base: cwd_path,
        })
    }

    /// 保存失败内容为草稿;禁用时返回 None
    fn save_draft(&self, target: &str, content: &str, append: bool) -> Option<String> {
        self.drafts.as_ref().map(|store| {
            store
                .lock()
                .unwrap()
                .save(target, content.to_string(), append)
        })
    }

    /// 恢复前 peek + take:先校验 target 一致,不匹配则不消费草稿(LLM 可用原路径重试)
    fn restore_draft(&self, draft_id: &str, target: &str) -> Result<(String, bool), String> {
        let Some(store) = self.drafts.as_ref() else {
            // 禁用 == 不存在,统一优雅降级
            return Err(format!(
                "WriteSandbox: 草稿 '{draft_id}' 不存在或已失效,请改用 'content' 参数重试"
            ));
        };
        let verdict = {
            let guard = store.lock().unwrap();
            match guard.peek(draft_id) {
                None => None,                                 // 未知/失效
                Some(e) if e.target != target => Some(false), // 路径不符
                Some(_) => Some(true),                        // 匹配
            }
        };
        match verdict {
            None => Err(format!(
                "WriteSandbox: 草稿 '{draft_id}' 不存在或已失效,请改用 'content' 参数重试"
            )),
            Some(false) => Err(format!(
                "WriteSandbox: 草稿 '{draft_id}' 属于其他路径,请使用原 file_path,或改用 'content' 参数重试"
            )),
            Some(true) => {
                // peek 与 take 之间存在竞态窗口(同 draft_id 被并发恢复),优雅降级为 unknown
                let Some(entry) = store.lock().unwrap().take(draft_id) else {
                    return Err(format!(
                        "WriteSandbox: 草稿 '{draft_id}' 不存在或已失效,请改用 'content' 参数重试"
                    ));
                };
                Ok((entry.content, entry.append))
            }
        }
    }

    /// 成功写入后清理同 target 草稿(幂等;禁用时 no-op)
    fn remove_draft(&self, target: &str) {
        if let Some(store) = &self.drafts {
            store.lock().unwrap().remove_by_target(target);
        }
    }

    /// 格式化允许的沙箱目录列表，用于错误信息。
    /// 使用原始相对路径（如 `.peri/plans/`），而非 canonicalized 绝对路径，
    /// 因为工具只接受相对路径，展示绝对路径会与约束矛盾。
    fn allowed_dirs_display(&self) -> String {
        format!("允许的目录: {:?}", self.allowed_dirs)
    }

    /// 全路径安全校验链。
    ///
    /// 返回 canonicalized 目标路径，或错误描述。
    fn validate_path(&self, path: &str) -> Result<PathBuf, String> {
        // ① 词法拒绝绝对路径
        if Path::new(path).is_absolute() {
            return Err(format!(
                "WriteSandbox: 拒绝绝对路径 '{}'。请使用基于项目根的相对路径。{}",
                path,
                self.allowed_dirs_display()
            ));
        }
        // ② 词法拒绝路径穿越（含 ../、..\\ 等变体）
        let normalized = path.replace('\\', "/");
        for segment in normalized.split('/') {
            if segment == ".." {
                return Err(format!(
                    "WriteSandbox: 拒绝路径穿越 '{}'（含 '..'）。请使用沙箱目录内的相对路径。{}",
                    path,
                    self.allowed_dirs_display()
                ));
            }
        }

        let raw = self.path_base.join(path);

        // ③ 寻找最长存在祖先并 canonicalize + 沙箱校验，防止 create_dir_all
        //    跟随 symlink 在沙箱外创建目录（副作用逃逸）
        // ④ 然后创建剩余父目录 + canonicalize 目标（防 symlink 逃逸）
        let canonical_target = if raw.exists() {
            // 文件已存在：直接 canonicalize 目标本身
            raw.canonicalize().map_err(|e| {
                format!(
                    "WriteSandbox: canonicalize 失败 '{}': {}。{}",
                    path,
                    e,
                    self.allowed_dirs_display()
                )
            })?
        } else {
            // 文件不存在：找到最长存在的祖先路径
            let ancestor = {
                let mut p = raw.as_path();
                loop {
                    match p.parent() {
                        Some(parent) if !parent.exists() => p = parent,
                        Some(parent) => break parent.to_path_buf(),
                        None => break p.to_path_buf(),
                    }
                }
            };
            // canonicalize 已有祖先 + 校验沙箱前缀
            let canon_ancestor = ancestor.canonicalize().map_err(|e| {
                format!(
                    "WriteSandbox: 无法 canonicalize 路径 '{}': {}。{}",
                    ancestor.display(),
                    e,
                    self.allowed_dirs_display()
                )
            })?;
            let is_ancestor_in_sandbox = self
                .sandbox_roots
                .iter()
                .any(|root| canon_ancestor.starts_with(root));
            if !is_ancestor_in_sandbox {
                return Err(format!(
                    "WriteSandbox: 路径 '{}' 的已有祖先不在沙箱目录内。{}",
                    path,
                    self.allowed_dirs_display()
                ));
            }
            // 创建剩余父目录（祖先已校验，新创建的目录在祖先子树内 = 安全）
            if let Some(parent) = raw.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    format!(
                        "WriteSandbox: 创建父目录失败 '{}': {}。{}",
                        path,
                        e,
                        self.allowed_dirs_display()
                    )
                })?;
            }
            // 再 canonicalize 父目录 + 文件名
            if let (Some(parent), Some(file_name)) = (raw.parent(), raw.file_name()) {
                match parent.canonicalize() {
                    Ok(canon_parent) => canon_parent.join(file_name),
                    Err(e) => {
                        return Err(format!(
                            "WriteSandbox: 无法 canonicalize 父目录 '{}': {}。{}",
                            parent.display(),
                            e,
                            self.allowed_dirs_display()
                        ));
                    }
                }
            } else {
                return Err(format!(
                    "WriteSandbox: 无法解析路径 '{}'——缺少父目录或文件名。{}",
                    path,
                    self.allowed_dirs_display()
                ));
            }
        };

        // ⑤ 最终以沙箱根为前缀校验
        let is_in_sandbox = self
            .sandbox_roots
            .iter()
            .any(|root| canonical_target.starts_with(root));
        if !is_in_sandbox {
            return Err(format!(
                "WriteSandbox: 路径 '{}' 不在沙箱目录内。允许的目录: {:?}",
                path,
                self.sandbox_roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
            ));
        }

        Ok(canonical_target)
    }
}

/// 校验沙箱根使用规范相对目录，拒绝绝对路径、空目录与父目录跳转。
fn validate_allowed_directory(
    directory: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let normalized = directory.replace('\\', "/");
    let has_windows_prefix = normalized
        .as_bytes()
        .get(1)
        .is_some_and(|separator| *separator == b':');
    let segments = normalized.split('/').collect::<Vec<_>>();
    let valid = !normalized.is_empty()
        && !normalized.starts_with('/')
        && !has_windows_prefix
        && segments.iter().enumerate().all(|(index, segment)| {
            let trailing_separator = index + 1 == segments.len() && segment.is_empty();
            trailing_separator || (!segment.is_empty() && *segment != "." && *segment != "..")
        })
        && segments
            .iter()
            .any(|segment| !segment.is_empty() && *segment != "." && *segment != "..");
    if valid {
        Ok(())
    } else {
        Err(
            format!("WriteSandbox: 沙箱目录必须是项目根下的规范相对目录，收到 '{directory}'")
                .into(),
        )
    }
}

#[async_trait::async_trait]
impl BaseTool for WriteSandboxTool {
    fn name(&self) -> &str {
        "SandboxWrite"
    }

    fn aliases(&self) -> &[&str] {
        &["WriteSandbox"]
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The file path relative to the project root (within your sandbox).\
                     Do NOT use absolute paths or '..'. Overwriting is allowed."
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file. Either 'content' or 'from_draft' must be provided."
                },
                "from_draft": {
                    "type": "string",
                    "description": "A draft id returned in a previous SandboxWrite error message. Recover the failed write without resending content. Mutually exclusive with 'content'; reuse the original file_path."
                }
            },
            "required": ["file_path"]
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let path = input["file_path"]
            .as_str()
            .ok_or("WriteSandbox: 'file_path' 参数必填")?;

        // 安全校验(仍在校验链最前——from_draft 恢复必须走完整校验链,
        // 含绝对路径/穿越/symlink 逃逸/沙箱前缀;校验失败时草稿未被消费,可用正确路径重试)
        let target = self.validate_path(path)?;
        let target_str = target.to_string_lossy().to_string();

        // 宽容解析（与 Write 工具同构的容错）：LLM 常同时携带互斥参数，或用
        // "" / "__omit__" 占位符表达「省略」——content 优先，from_draft 仅在
        // content 未提供时使用，两者皆无才报缺参数（原「互斥报错」会让文件无法
        // 落盘、模型被迫重输出一遍内容）。
        let content = input["content"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && *s != "__omit__");
        let from_draft = input["from_draft"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && *s != "__omit__");
        let (content, _append) = if let Some(c) = content {
            (c.to_string(), false)
        } else if let Some(id) = from_draft {
            self.restore_draft(id, &target_str)? // 恢复路径,SandboxWrite 无 append
        } else {
            return Err("WriteSandbox: 必须提供 'content' 或 'from_draft' 参数之一".into());
        };

        let line_count = content.lines().count();

        // 原子写入：先写临时文件再 rename（复用 WriteFileTool 逻辑）
        let tmp_ext = format!("tmp.{}", uuid::Uuid::now_v7());
        let tmp_path = target.with_extension(tmp_ext);

        let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
            if let Err(e) = std::fs::write(&tmp_path, &content) {
                let hint = self
                    .save_draft(&target_str, &content, false)
                    .map(|id| draft_hint_zh(&id, &content))
                    .unwrap_or_default();
                return Err(format!("WriteSandbox: 写入失败: {}{}", e, hint).into());
            }

            // 如果目标已存在，保留 Unix 权限位
            if let Ok(metadata) = std::fs::metadata(&target) {
                #[cfg(unix)]
                {
                    let _ = std::fs::set_permissions(&tmp_path, metadata.permissions());
                }
                #[cfg(not(unix))]
                let _ = &metadata;
            }

            match std::fs::rename(&tmp_path, &target) {
                Ok(_) => {
                    let rel = target
                        .strip_prefix(&self.path_base)
                        .unwrap_or(&target)
                        .display()
                        .to_string();
                    let lines_label = if line_count == 1 { "line" } else { "lines" };
                    self.remove_draft(&target_str);
                    Ok(format!("Wrote {} {} {}", line_count, lines_label, rel))
                }
                Err(e) => {
                    // 读 tmp 实际文本 → 存草稿 → 删 tmp(顺序固定,先读后删)
                    let draft_content =
                        std::fs::read_to_string(&tmp_path).unwrap_or_else(|_| content.to_string());
                    let hint = self
                        .save_draft(&target_str, &draft_content, false)
                        .map(|id| draft_hint_zh(&id, &draft_content))
                        .unwrap_or_default();
                    let _ = std::fs::remove_file(&tmp_path);
                    Err(format!("WriteSandbox: rename 临时文件失败: {}{}", e, hint).into())
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                // 草稿 = content 原文;新 id 自洽(恢复写入超时存的是恢复出的内容)
                let hint = self
                    .save_draft(&target_str, &content, false)
                    .map(|id| draft_hint_zh(&id, &content))
                    .unwrap_or_default();
                Err(format!("WriteSandbox: 操作超时（超过 2 分钟）{}", hint).into())
            }
        }
    }
}

#[cfg(test)]
#[path = "write_sandbox_test.rs"]
mod tests;
