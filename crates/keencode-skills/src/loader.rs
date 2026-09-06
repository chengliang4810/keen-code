//! Skills 根目录的安全发现、冲突归约与正文懒加载。

use crate::parser::normalized_name;
use crate::{
    InjectableSkill, ParsedSkillDocument, SkillCatalogEntry, SkillConfigError, SkillDiagnostic,
    SkillDiagnosticCode, SkillDiagnosticSeverity, SkillDiscoveryConfig, SkillLoadError,
    SkillSource, parse_skill_document,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};

/// 已完成冲突归约且可按名称安全加载正文的 Skills 目录。
#[derive(Debug)]
pub struct SkillCatalog {
    entries: Vec<SkillCatalogEntry>,
    diagnostics: Vec<SkillDiagnostic>,
    records: BTreeMap<String, SkillRecord>,
    max_skill_bytes: u64,
    limits: crate::SkillLimits,
}

impl SkillCatalog {
    /// 返回按跨平台名称键稳定排序的 Provider 中立目录条目。
    pub fn entries(&self) -> &[SkillCatalogEntry] {
        &self.entries
    }

    /// 返回发现阶段产生且不包含 Skill 正文的诊断。
    pub fn diagnostics(&self) -> &[SkillDiagnostic] {
        &self.diagnostics
    }

    /// 按名称重新校验路径并读取一个 Skill 的 Markdown 正文。
    ///
    /// 查找对 ASCII 大小写不敏感。正文不会在 `discover_skills` 中缓存，
    /// 因此文件在元数据保持不变时更新正文，会由本方法读取到最新内容。
    pub fn load(&self, name: &str) -> Result<InjectableSkill, SkillLoadError> {
        let lookup = normalized_name(name);
        let record = self
            .records
            .get(&lookup)
            .ok_or_else(|| SkillLoadError::NotFound {
                name: name.to_string(),
            })?;
        if !record.entry.enabled {
            return Err(SkillLoadError::Disabled {
                name: record.entry.name.clone(),
            });
        }

        let current_root_metadata =
            fs::symlink_metadata(&record.root).map_err(|_| SkillLoadError::RootChanged {
                name: record.entry.name.clone(),
            })?;
        if current_root_metadata.file_type().is_symlink() || !current_root_metadata.is_dir() {
            return Err(SkillLoadError::RootChanged {
                name: record.entry.name.clone(),
            });
        }
        let current_root =
            fs::canonicalize(&record.root).map_err(|_| SkillLoadError::RootChanged {
                name: record.entry.name.clone(),
            })?;
        if current_root != record.root {
            return Err(SkillLoadError::RootChanged {
                name: record.entry.name.clone(),
            });
        }
        if !is_safe_relative_manifest(&record.manifest_relative) {
            return Err(SkillLoadError::UnsafePath {
                name: record.entry.name.clone(),
            });
        }

        let mut current = record.root.clone();
        let component_count = record.manifest_relative.components().count();
        for (index, component) in record.manifest_relative.components().enumerate() {
            let Component::Normal(segment) = component else {
                return Err(SkillLoadError::UnsafePath {
                    name: record.entry.name.clone(),
                });
            };
            current.push(segment);
            let metadata =
                fs::symlink_metadata(&current).map_err(|_| SkillLoadError::Unavailable {
                    name: record.entry.name.clone(),
                })?;
            if metadata.file_type().is_symlink() {
                return Err(SkillLoadError::UnsafePath {
                    name: record.entry.name.clone(),
                });
            }
            let is_final = index + 1 == component_count;
            if (!is_final && !metadata.is_dir()) || (is_final && !metadata.is_file()) {
                return Err(SkillLoadError::Unavailable {
                    name: record.entry.name.clone(),
                });
            }
        }

        let canonical_manifest =
            fs::canonicalize(&current).map_err(|_| SkillLoadError::Unavailable {
                name: record.entry.name.clone(),
            })?;
        if !canonical_manifest.starts_with(&record.root) {
            return Err(SkillLoadError::UnsafePath {
                name: record.entry.name.clone(),
            });
        }
        let bytes = read_limited(&canonical_manifest, self.max_skill_bytes).map_err(|error| {
            map_load_read_error(&record.entry.name, self.max_skill_bytes, error)
        })?;
        let content = String::from_utf8(bytes).map_err(|_| SkillLoadError::InvalidDocument {
            name: record.entry.name.clone(),
            message: "文件不是有效 UTF-8".to_string(),
        })?;
        let document = parse_skill_document(&content, &self.limits).map_err(|error| {
            SkillLoadError::InvalidDocument {
                name: record.entry.name.clone(),
                message: error.to_string(),
            }
        })?;
        if document.name != record.entry.name || document.description != record.entry.description {
            return Err(SkillLoadError::CatalogStale {
                name: record.entry.name.clone(),
            });
        }
        Ok(InjectableSkill {
            name: document.name,
            description: document.description,
            markdown: document.markdown,
            source: record.entry.source,
        })
    }
}

/// 从配置数据目录和项目目录发现 Skills，并建立不含正文的目录。
///
/// 同名时项目来源优先于数据目录；相同来源内按规范化相对路径字典序选择。
/// 所有冲突、无效文件和安全拒绝均通过目录诊断返回，不阻断其他有效 Skills。
pub fn discover_skills(config: &SkillDiscoveryConfig) -> Result<SkillCatalog, SkillConfigError> {
    config.limits.validate()?;
    if config.additional_roots.len() > config.limits.max_manifests
        || config
            .additional_roots
            .iter()
            .any(|root| root.path.as_os_str().is_empty() || !root.path.is_absolute())
    {
        return Err(SkillConfigError::InvalidAdditionalRoots);
    }
    let disabled: BTreeSet<String> = config
        .disabled_names
        .iter()
        .map(|name| normalized_name(name))
        .collect();
    let mut root_specs = vec![
        RootSpec {
            base: &config.directories.project_directory,
            relative: Path::new(".agents").join("skills"),
            source: SkillSource::Project,
            recursive: true,
        },
        RootSpec {
            base: &config.directories.data_directory,
            relative: PathBuf::from("skills"),
            source: SkillSource::Data,
            recursive: true,
        },
    ];
    root_specs.extend(config.additional_roots.iter().map(|root| RootSpec {
        base: &root.path,
        relative: PathBuf::new(),
        source: root.source,
        recursive: root.recursive,
    }));
    let mut diagnostics = Vec::new();
    let mut candidates = Vec::new();
    let mut state = ScanState::default();
    for spec in root_specs {
        let Some(root) = prepare_root(&spec, &mut diagnostics) else {
            continue;
        };
        scan_directory(
            &root,
            &root.canonical,
            Path::new(""),
            0,
            config,
            &mut state,
            &mut candidates,
            &mut diagnostics,
        );
        if state.exhausted {
            break;
        }
    }

    candidates.sort_by(|left, right| {
        left.source
            .priority()
            .cmp(&right.source.priority())
            .then_with(|| left.stable_path.cmp(&right.stable_path))
    });
    let mut winners: BTreeMap<String, SkillRecord> = BTreeMap::new();
    for candidate in candidates {
        let key = normalized_name(&candidate.document.name);
        if let Some(winner) = winners.get(&key) {
            diagnostics.push(SkillDiagnostic {
                severity: SkillDiagnosticSeverity::Warning,
                code: SkillDiagnosticCode::NameConflict,
                source: candidate.source,
                relative_path: Some(candidate.manifest_relative),
                message: format!(
                    "同名 Skill {} 已由{}来源的稳定优先项提供",
                    candidate.document.name,
                    source_label(winner.entry.source)
                ),
            });
            continue;
        }
        let enabled = !disabled.contains(&key);
        let entry = SkillCatalogEntry {
            name: candidate.document.name,
            description: candidate.document.description,
            source: candidate.source,
            enabled,
        };
        if !enabled {
            diagnostics.push(SkillDiagnostic {
                severity: SkillDiagnosticSeverity::Info,
                code: SkillDiagnosticCode::Disabled,
                source: entry.source,
                relative_path: Some(candidate.manifest_relative.clone()),
                message: format!("Skill {} 已按配置禁用", entry.name),
            });
        }
        winners.insert(
            key,
            SkillRecord {
                entry,
                root: candidate.root,
                manifest_relative: candidate.manifest_relative,
            },
        );
    }
    let entries = winners
        .values()
        .map(|record| record.entry.clone())
        .collect();
    Ok(SkillCatalog {
        entries,
        diagnostics,
        records: winners,
        max_skill_bytes: config.limits.max_skill_bytes,
        limits: config.limits,
    })
}

/// 一个来源对应的声明路径和固定 Skills 相对目录。
struct RootSpec<'a> {
    /// 调用方配置的数据目录或项目目录。
    base: &'a Path,
    /// Skills 根相对基础目录的固定路径。
    relative: PathBuf,
    /// 当前根的优先级来源。
    source: SkillSource,
    /// 是否允许递归进入当前根的子目录。
    recursive: bool,
}

/// 已验证位于基础目录内的规范 Skills 根。
struct PreparedRoot {
    /// 当前根的优先级来源。
    source: SkillSource,
    /// 不包含符号链接的规范绝对路径。
    canonical: PathBuf,
    /// 是否允许递归进入当前根的子目录。
    recursive: bool,
    /// 用于跨额外根稳定归约同名项的规范路径键。
    stable_key: String,
}

/// 一个解析成功但尚未执行同名归约的候选 Skill。
struct CandidateSkill {
    /// 当前 Skill 的 Provider 中立文档内容。
    document: ParsedSkillDocument,
    /// 当前候选的配置来源。
    source: SkillSource,
    /// 当前候选所在的规范根目录。
    root: PathBuf,
    /// `SKILL.md` 相对规范根目录的安全路径。
    manifest_relative: PathBuf,
    /// 使用 `/` 分隔且 ASCII 小写的稳定排序键。
    stable_path: String,
}

/// 目录中实际生效 Skill 的私有加载记录。
#[derive(Debug)]
struct SkillRecord {
    /// 对 Provider 中立目录公开的元数据。
    entry: SkillCatalogEntry,
    /// 发现时重新验证过的规范根目录。
    root: PathBuf,
    /// 根内 `SKILL.md` 的相对路径。
    manifest_relative: PathBuf,
}

/// 两个来源共享的遍历计数和熔断状态。
#[derive(Default)]
struct ScanState {
    /// 已检查的目录项数量。
    entries_seen: usize,
    /// 已检查的 `SKILL.md` 数量。
    manifests_seen: usize,
    /// 任一全局数量上限达到后停止后续来源扫描。
    exhausted: bool,
}

/// 验证基础目录和固定 Skills 根的规范边界。
fn prepare_root(
    spec: &RootSpec<'_>,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<PreparedRoot> {
    let base_metadata = match fs::metadata(spec.base) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            push_root_diagnostic(
                diagnostics,
                spec.source,
                SkillDiagnosticSeverity::Info,
                SkillDiagnosticCode::BaseDirectoryMissing,
                "配置的基础目录不存在",
            );
            return None;
        }
        Err(_) => {
            push_root_diagnostic(
                diagnostics,
                spec.source,
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::FileSystemFailure,
                "无法读取配置的基础目录",
            );
            return None;
        }
    };
    if !base_metadata.is_dir() {
        push_root_diagnostic(
            diagnostics,
            spec.source,
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::NotDirectory,
            "配置的基础路径不是目录",
        );
        return None;
    }
    let canonical_base = match fs::canonicalize(spec.base) {
        Ok(path) => path,
        Err(_) => {
            push_root_diagnostic(
                diagnostics,
                spec.source,
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::FileSystemFailure,
                "无法规范化配置的基础目录",
            );
            return None;
        }
    };
    let declared_root = spec.base.join(&spec.relative);
    let root_metadata = match fs::symlink_metadata(&declared_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            push_root_diagnostic(
                diagnostics,
                spec.source,
                SkillDiagnosticSeverity::Info,
                SkillDiagnosticCode::RootDirectoryMissing,
                "Skills 根目录不存在",
            );
            return None;
        }
        Err(_) => {
            push_root_diagnostic(
                diagnostics,
                spec.source,
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::FileSystemFailure,
                "无法读取 Skills 根目录",
            );
            return None;
        }
    };
    if root_metadata.file_type().is_symlink() {
        push_root_diagnostic(
            diagnostics,
            spec.source,
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::RootSymlinkRejected,
            "Skills 根目录不能是符号链接",
        );
        return None;
    }
    if !root_metadata.is_dir() {
        push_root_diagnostic(
            diagnostics,
            spec.source,
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::NotDirectory,
            "Skills 根路径不是目录",
        );
        return None;
    }
    let canonical_root = match fs::canonicalize(&declared_root) {
        Ok(path) => path,
        Err(_) => {
            push_root_diagnostic(
                diagnostics,
                spec.source,
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::FileSystemFailure,
                "无法规范化 Skills 根目录",
            );
            return None;
        }
    };
    if !canonical_root.starts_with(&canonical_base) {
        push_root_diagnostic(
            diagnostics,
            spec.source,
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::RootOutsideBoundary,
            "Skills 根目录离开了配置的基础目录",
        );
        return None;
    }
    Some(PreparedRoot {
        source: spec.source,
        stable_key: canonical_root
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase(),
        canonical: canonical_root,
        recursive: spec.recursive,
    })
}

/// 按文件名稳定排序递归扫描单个安全根目录。
#[allow(clippy::too_many_arguments)]
fn scan_directory(
    root: &PreparedRoot,
    directory: &Path,
    relative_directory: &Path,
    depth: usize,
    config: &SkillDiscoveryConfig,
    state: &mut ScanState,
    candidates: &mut Vec<CandidateSkill>,
    diagnostics: &mut Vec<SkillDiagnostic>,
) {
    if state.exhausted {
        return;
    }
    let read_directory = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => {
            push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::FileSystemFailure,
                relative_directory,
                "无法读取 Skill 目录",
            );
            return;
        }
    };
    let remaining_budget = config.limits.max_entries.saturating_sub(state.entries_seen);
    let mut entries = Vec::with_capacity(remaining_budget.min(256));
    let mut directory_overflow = false;
    for entry in read_directory {
        match entry {
            Ok(entry) if entries.len() < remaining_budget => entries.push(entry),
            Ok(_) => {
                directory_overflow = true;
                break;
            }
            Err(_) => push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::FileSystemFailure,
                relative_directory,
                "无法读取 Skill 目录项",
            ),
        }
    }
    if directory_overflow {
        push_path_diagnostic(
            diagnostics,
            root.source,
            SkillDiagnosticSeverity::Warning,
            SkillDiagnosticCode::EntryLimitReached,
            relative_directory,
            "Skills 目录项数量达到上限，当前目录未扫描",
        );
        state.exhausted = true;
        return;
    }
    state.entries_seen += entries.len();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let relative = relative_directory.join(entry.file_name());
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                push_path_diagnostic(
                    diagnostics,
                    root.source,
                    SkillDiagnosticSeverity::Warning,
                    SkillDiagnosticCode::FileSystemFailure,
                    &relative,
                    "无法读取目录项类型",
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::SymlinkSkipped,
                &relative,
                "符号链接目录项已跳过",
            );
            continue;
        }
        if file_type.is_dir() {
            if !root.recursive || depth >= config.limits.max_depth {
                push_path_diagnostic(
                    diagnostics,
                    root.source,
                    SkillDiagnosticSeverity::Warning,
                    SkillDiagnosticCode::DepthLimitReached,
                    &relative,
                    "Skill 目录达到递归深度上限",
                );
                continue;
            }
            let canonical = match fs::canonicalize(entry.path()) {
                Ok(path) => path,
                Err(_) => {
                    push_path_diagnostic(
                        diagnostics,
                        root.source,
                        SkillDiagnosticSeverity::Warning,
                        SkillDiagnosticCode::FileSystemFailure,
                        &relative,
                        "无法规范化 Skill 子目录",
                    );
                    continue;
                }
            };
            if !canonical.starts_with(&root.canonical) {
                push_path_diagnostic(
                    diagnostics,
                    root.source,
                    SkillDiagnosticSeverity::Error,
                    SkillDiagnosticCode::RootOutsideBoundary,
                    &relative,
                    "Skill 子目录离开了安全根目录",
                );
                continue;
            }
            scan_directory(
                root,
                &canonical,
                &relative,
                depth + 1,
                config,
                state,
                candidates,
                diagnostics,
            );
            if state.exhausted {
                return;
            }
            continue;
        }
        if entry.file_name() != OsStr::new("SKILL.md") {
            continue;
        }
        if state.manifests_seen >= config.limits.max_manifests {
            push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::ManifestLimitReached,
                &relative,
                "SKILL.md 候选数量达到上限，后续内容未扫描",
            );
            state.exhausted = true;
            return;
        }
        state.manifests_seen += 1;
        if !file_type.is_file() {
            push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::ManifestNotFile,
                &relative,
                "SKILL.md 不是普通文件",
            );
            continue;
        }
        let canonical = match fs::canonicalize(entry.path()) {
            Ok(path) => path,
            Err(_) => {
                push_path_diagnostic(
                    diagnostics,
                    root.source,
                    SkillDiagnosticSeverity::Warning,
                    SkillDiagnosticCode::FileSystemFailure,
                    &relative,
                    "无法规范化 SKILL.md",
                );
                continue;
            }
        };
        if !canonical.starts_with(&root.canonical) {
            push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::RootOutsideBoundary,
                &relative,
                "SKILL.md 离开了安全根目录",
            );
            continue;
        }
        match read_and_parse_candidate(&canonical, config) {
            Ok(document) => candidates.push(CandidateSkill {
                document,
                source: root.source,
                root: root.canonical.clone(),
                manifest_relative: relative.clone(),
                stable_path: format!("{}//{}", root.stable_key, stable_relative_key(&relative)),
            }),
            Err(CandidateError::TooLarge) => push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::ManifestTooLarge,
                &relative,
                "SKILL.md 超过配置的单文件大小上限",
            ),
            Err(CandidateError::Read) => push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::FileSystemFailure,
                &relative,
                "无法读取 SKILL.md",
            ),
            Err(CandidateError::Invalid(message)) => push_path_diagnostic(
                diagnostics,
                root.source,
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::InvalidDocument,
                &relative,
                &message,
            ),
        }
    }
}

/// 读取并解析一个候选文档，且不把 Markdown 正文存入目录记录。
fn read_and_parse_candidate(
    path: &Path,
    config: &SkillDiscoveryConfig,
) -> Result<ParsedSkillDocument, CandidateError> {
    let metadata = fs::metadata(path).map_err(|_| CandidateError::Read)?;
    if metadata.len() > config.limits.max_skill_bytes {
        return Err(CandidateError::TooLarge);
    }
    let bytes = read_front_matter(path, config.limits.max_front_matter_bytes)?;
    let content = String::from_utf8(bytes)
        .map_err(|_| CandidateError::Invalid("Skill front matter 不是有效 UTF-8".to_string()))?;
    parse_skill_document(&content, &config.limits)
        .map_err(|error| CandidateError::Invalid(error.to_string()))
}

/// 只读取 front matter 到闭合分隔符，发现阶段不读取 Markdown 正文。
fn read_front_matter(path: &Path, limit: usize) -> Result<Vec<u8>, CandidateError> {
    let file = File::open(path).map_err(|_| CandidateError::Read)?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::with_capacity(limit.min(4 * 1024));
    let mut line_index = 0;
    loop {
        let mut line = Vec::new();
        let count = reader
            .read_until(b'\n', &mut line)
            .map_err(|_| CandidateError::Read)?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(line.len()) > limit {
            return Err(CandidateError::Invalid(
                "front matter 未在配置上限内闭合".to_string(),
            ));
        }
        let delimiter = line_without_ending(&line);
        let is_opening = if line_index == 0 {
            delimiter.strip_prefix(&[0xef, 0xbb, 0xbf]) == Some(b"---") || delimiter == b"---"
        } else {
            false
        };
        let is_closing = line_index > 0 && delimiter == b"---";
        bytes.extend_from_slice(&line);
        line_index += 1;
        if !is_opening && line_index == 1 {
            break;
        }
        if is_closing {
            break;
        }
    }
    Ok(bytes)
}

/// 去除一行末尾的 LF 与可选 CR，供分隔符字节比较使用。
fn line_without_ending(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line))
}

/// 候选文档读取和解析的内部失败类别。
enum CandidateError {
    /// 文件超过单文件上限。
    TooLarge,
    /// 文件系统读取失败。
    Read,
    /// UTF-8 或文档结构无效。
    Invalid(String),
}

/// 有界读取的内部失败类别。
enum BoundedReadError {
    /// 元数据或实际读取结果超过上限。
    TooLarge,
    /// 打开、读取或元数据调用失败。
    Io(io::Error),
}

/// 先检查元数据，再使用加一哨兵避免竞态导致无界读取。
fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, BoundedReadError> {
    let metadata = fs::metadata(path).map_err(BoundedReadError::Io)?;
    if metadata.len() > limit {
        return Err(BoundedReadError::TooLarge);
    }
    let file = File::open(path).map_err(BoundedReadError::Io)?;
    let mut bytes = Vec::with_capacity(metadata.len().min(limit).min(64 * 1024) as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(BoundedReadError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(BoundedReadError::TooLarge);
    }
    Ok(bytes)
}

/// 把有界读取错误映射为不泄露绝对路径的公开加载错误。
fn map_load_read_error(name: &str, limit: u64, error: BoundedReadError) -> SkillLoadError {
    match error {
        BoundedReadError::TooLarge => SkillLoadError::TooLarge {
            name: name.to_string(),
            limit,
        },
        BoundedReadError::Io(error) => SkillLoadError::ReadFailed {
            name: name.to_string(),
            message: error.kind().to_string(),
        },
    }
}

/// 检查私有记录只能包含根内普通组件并以固定文件名结束。
fn is_safe_relative_manifest(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && path.file_name() == Some(OsStr::new("SKILL.md"))
}

/// 生成不依赖平台分隔符和 ASCII 大小写的路径排序键。
fn stable_relative_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// 返回诊断消息使用的中文来源短语。
const fn source_label(source: SkillSource) -> &'static str {
    match source {
        SkillSource::Project => "项目",
        SkillSource::Data => "数据目录",
        SkillSource::Plugin => "插件",
    }
}

/// 添加不关联具体条目的根级诊断。
fn push_root_diagnostic(
    diagnostics: &mut Vec<SkillDiagnostic>,
    source: SkillSource,
    severity: SkillDiagnosticSeverity,
    code: SkillDiagnosticCode,
    message: &str,
) {
    diagnostics.push(SkillDiagnostic {
        severity,
        code,
        source,
        relative_path: None,
        message: message.to_string(),
    });
}

/// 添加只暴露根内相对路径的条目诊断。
fn push_path_diagnostic(
    diagnostics: &mut Vec<SkillDiagnostic>,
    source: SkillSource,
    severity: SkillDiagnosticSeverity,
    code: SkillDiagnosticCode,
    relative_path: &Path,
    message: &str,
) {
    diagnostics.push(SkillDiagnostic {
        severity,
        code,
        source,
        relative_path: Some(relative_path.to_path_buf()),
        message: message.to_string(),
    });
}
