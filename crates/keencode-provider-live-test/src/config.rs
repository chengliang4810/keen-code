use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use keencode_model::ProviderProtocol;
use keencode_provider::{ApiKey, ProviderConfig, WireResponseMode};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// 真实兼容性运行可独立选择的能力探测。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ProbeKind {
    /// 普通文本生成与结束原因。
    Text,
    /// 指定工具选择、调用名称和 JSON 参数。
    ToolCalling,
    /// 同一响应中请求两个互不依赖的工具调用。
    ParallelToolCalling,
    /// 把首轮工具调用结果回传并验证第二轮最终响应。
    ToolResultRoundTrip,
    /// 把首轮工具调用关联到含图片结果的第二轮请求并验证协议边界。
    ToolResultImageRoundTrip,
    /// 把首轮助手响应带入下一轮并验证上下文连续性。
    MultiTurn,
    /// 推理请求配置与可观测推理证据。
    Reasoning,
    /// Provider 返回的输入、输出和可选扩展 Token 用量。
    Usage,
    /// 相同长前缀的第二次请求是否报告真实缓存读取用量。
    PromptCaching,
    /// Provider 原生 JSON Schema 约束。
    StructuredOutput,
    /// 小输出预算是否产生统一的长度结束原因。
    OutputLimit,
    /// 明确越界的采样参数是否被远端拒绝并归一化。
    InvalidParameter,
    /// 超大纯合成输入是否被归一化为上下文长度错误。
    ContextOverflow,
    /// 本地主动截断的 2xx SSE 是否被归一化为可重试流中断。
    StreamInterruption,
    /// 丢弃在途 Future 或事件流的本地取消边界。
    Cancellation,
}

impl ProbeKind {
    /// 返回全部能力的固定执行顺序。
    pub(crate) const fn all() -> [Self; 15] {
        [
            Self::Text,
            Self::ToolCalling,
            Self::ParallelToolCalling,
            Self::ToolResultRoundTrip,
            Self::ToolResultImageRoundTrip,
            Self::MultiTurn,
            Self::Reasoning,
            Self::Usage,
            Self::PromptCaching,
            Self::StructuredOutput,
            Self::OutputLimit,
            Self::InvalidParameter,
            Self::ContextOverflow,
            Self::StreamInterruption,
            Self::Cancellation,
        ]
    }

    /// 返回适合命令行和报告的稳定名称。
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::ToolCalling => "tool_calling",
            Self::ParallelToolCalling => "parallel_tool_calling",
            Self::ToolResultRoundTrip => "tool_result_round_trip",
            Self::ToolResultImageRoundTrip => "tool_result_image_round_trip",
            Self::MultiTurn => "multi_turn",
            Self::Reasoning => "reasoning",
            Self::Usage => "usage",
            Self::PromptCaching => "prompt_caching",
            Self::StructuredOutput => "structured_output",
            Self::OutputLimit => "output_limit",
            Self::InvalidParameter => "invalid_parameter",
            Self::ContextOverflow => "context_overflow",
            Self::StreamInterruption => "stream_interruption",
            Self::Cancellation => "cancellation",
        }
    }

    /// 从单个命令行名称解析能力。
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "text" => Ok(Self::Text),
            "tool_calling" => Ok(Self::ToolCalling),
            "parallel_tool_calling" => Ok(Self::ParallelToolCalling),
            "tool_result_round_trip" => Ok(Self::ToolResultRoundTrip),
            "tool_result_image_round_trip" => Ok(Self::ToolResultImageRoundTrip),
            "multi_turn" => Ok(Self::MultiTurn),
            "reasoning" => Ok(Self::Reasoning),
            "usage" => Ok(Self::Usage),
            "prompt_caching" => Ok(Self::PromptCaching),
            "structured_output" => Ok(Self::StructuredOutput),
            "output_limit" => Ok(Self::OutputLimit),
            "invalid_parameter" => Ok(Self::InvalidParameter),
            "context_overflow" => Ok(Self::ContextOverflow),
            "stream_interruption" => Ok(Self::StreamInterruption),
            "cancellation" => Ok(Self::Cancellation),
            _ => Err(format!(
                "未知能力 {value}；可选值：text、tool_calling、parallel_tool_calling、tool_result_round_trip、tool_result_image_round_trip、multi_turn、reasoning、usage、prompt_caching、structured_output、output_limit、invalid_parameter、context_overflow、stream_interruption、cancellation"
            )),
        }
    }
}

/// 命令行解析后的真实测试运行参数。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeOptions {
    /// 当前用户稳定的 KeenCode 数据目录，用于跨输出根的进程级互斥。
    pub(crate) user_data_directory: PathBuf,
    /// 只在内存中读取的 KeenCode Provider 配置文件。
    pub(crate) config_path: PathBuf,
    /// 只读核验指定的已完成运行目录，不发起请求或写入任何运行产物。
    pub(crate) verify_run_dir: Option<PathBuf>,
    /// 每次运行产物的父目录。
    pub(crate) output_root: PathBuf,
    /// 非空时严格恢复该既有运行目录，不创建新的运行标识。
    pub(crate) resume_dir: Option<PathBuf>,
    /// 非空时从原可执行文件已遗失的运行创建隔离恢复副本。
    pub(crate) recovery: Option<RecoveryOptions>,
    /// 非空时从一份已完成运行创建严格筛选的精确补测运行。
    pub(crate) retry: Option<RetryOptions>,
    /// 非空时仅离线合并一份基础运行与其精确补测运行。
    pub(crate) consolidation: Option<ConsolidationOptions>,
    /// 非空时只运行这些稳定 Provider 标识。
    pub(crate) provider_filters: BTreeSet<String>,
    /// 非空时只运行这些精确模型标识。
    pub(crate) model_filters: BTreeSet<String>,
    /// 单个确定性用例允许的最大尝试次数。
    pub(crate) max_attempts: usize,
    /// 单次 HTTP 模型请求的总超时秒数。
    pub(crate) request_timeout_secs: u64,
    /// 是否只获取实时目录而不发起生成请求。
    pub(crate) catalog_only: bool,
    /// 是否只运行 Provider 级认证与缺失模型负向诊断。
    pub(crate) diagnostics_only: bool,
    /// 按固定顺序执行的能力集合。
    pub(crate) capabilities: BTreeSet<ProbeKind>,
    /// 是否由 `--full` 请求完整能力矩阵。
    pub(crate) full_matrix: bool,
    /// 用户是否显式提供了会改变补测恢复边界的范围或专用模式参数。
    pub(crate) retry_scope_explicit: bool,
    /// 是否显式接受缺少事实认证的上一版来源；仅供隔离升级、一次性补测与合并使用。
    pub(crate) allow_unauthenticated_legacy_base: bool,
}

/// 只能由用户显式选择的遗失可执行文件恢复参数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryOptions {
    /// 只读打开且绝不改写的原始运行目录。
    pub(crate) source_dir: PathBuf,
    /// 用户从原始 `resume.json` 核对并显式提供的可执行文件摘要。
    pub(crate) expected_source_executable_sha256: String,
}

/// 从已完成运行按固定失败策略创建精确补测选择的参数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RetryOptions {
    /// 只读打开且绝不改写的已完成来源运行目录。
    pub(crate) source_dir: PathBuf,
    /// 只允许补测的单一 Provider 稳定标识。
    pub(crate) provider_id: String,
    /// 只选择该提交日志序号及之前已经落盘的失败事实。
    pub(crate) through_sequence: u64,
    /// 用户从来源 `resume.json` 核对并显式提供的可执行文件摘要。
    pub(crate) expected_source_executable_sha256: String,
}

/// 对基础运行和精确补测运行执行纯离线合并的参数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConsolidationOptions {
    /// 提供原始完整矩阵事实的只读基础运行目录。
    pub(crate) base_dir: PathBuf,
    /// 只包含精确补测事实且绑定基础运行摘要的只读运行目录。
    pub(crate) retry_dir: PathBuf,
}

impl RuntimeOptions {
    /// 从进程参数解析运行配置；凭据不允许通过命令行传入。
    pub(crate) fn parse() -> Result<Self, String> {
        Self::parse_from(env::args_os().skip(1))
    }

    /// 从可测试的参数迭代器解析运行配置。
    fn parse_from(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let user_data_directory = default_user_data_directory()?;
        let mut options = Self {
            config_path: user_data_directory.join("providers.json"),
            verify_run_dir: None,
            user_data_directory,
            output_root: env::current_dir()
                .map_err(|error| format!("无法确定当前目录：{error}"))?
                .join("target")
                .join("provider-live-test"),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts: 3,
            request_timeout_secs: 300,
            catalog_only: false,
            diagnostics_only: false,
            capabilities: BTreeSet::new(),
            full_matrix: false,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        };

        let mut capability_explicit = false;
        let mut config_explicit = false;
        let mut output_root_explicit = false;
        let mut max_attempts_explicit = false;
        let mut request_timeout_explicit = false;
        let mut recovery_source_dir = None;
        let mut retry_source_dir = None;
        let mut retry_provider_id = None;
        let mut retry_through_sequence = None;
        let mut consolidation_base_dir = None;
        let mut consolidation_retry_dir = None;
        let mut expected_source_executable_sha256 = None;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--config") => {
                    if config_explicit {
                        return Err("--config 只能指定一次".to_owned());
                    }
                    config_explicit = true;
                    options.config_path = required_path(&mut arguments, "--config")?;
                }
                Some("--verify-run") => {
                    if options.verify_run_dir.is_some() {
                        return Err("--verify-run 只能指定一次".to_owned());
                    }
                    options.verify_run_dir =
                        Some(required_verify_run_path(&mut arguments, "--verify-run")?);
                }
                Some("--output-root") => {
                    output_root_explicit = true;
                    options.output_root = required_path(&mut arguments, "--output-root")?;
                }
                Some("--resume") => {
                    if options.resume_dir.is_some() {
                        return Err("--resume 只能指定一次".to_owned());
                    }
                    options.resume_dir = Some(required_path(&mut arguments, "--resume")?);
                }
                Some("--recover-from") => {
                    if recovery_source_dir.is_some() {
                        return Err("--recover-from 只能指定一次".to_owned());
                    }
                    recovery_source_dir = Some(required_path(&mut arguments, "--recover-from")?);
                }
                Some("--retry-from") => {
                    if retry_source_dir.is_some() {
                        return Err("--retry-from 只能指定一次".to_owned());
                    }
                    retry_source_dir = Some(required_path(&mut arguments, "--retry-from")?);
                }
                Some("--retry-provider") => {
                    if retry_provider_id.is_some() {
                        return Err("--retry-provider 只能指定一次".to_owned());
                    }
                    retry_provider_id = Some(required_text(&mut arguments, "--retry-provider")?);
                }
                Some("--retry-through-sequence") => {
                    if retry_through_sequence.is_some() {
                        return Err("--retry-through-sequence 只能指定一次".to_owned());
                    }
                    let sequence = required_u64(&mut arguments, "--retry-through-sequence")?;
                    if sequence == 0 {
                        return Err("--retry-through-sequence 必须大于零".to_owned());
                    }
                    retry_through_sequence = Some(sequence);
                }
                Some("--consolidate-base") => {
                    if consolidation_base_dir.is_some() {
                        return Err("--consolidate-base 只能指定一次".to_owned());
                    }
                    consolidation_base_dir =
                        Some(required_path(&mut arguments, "--consolidate-base")?);
                }
                Some("--consolidate-retry") => {
                    if consolidation_retry_dir.is_some() {
                        return Err("--consolidate-retry 只能指定一次".to_owned());
                    }
                    consolidation_retry_dir =
                        Some(required_path(&mut arguments, "--consolidate-retry")?);
                }
                Some("--expected-source-executable-sha256") => {
                    if expected_source_executable_sha256.is_some() {
                        return Err("--expected-source-executable-sha256 只能指定一次".to_owned());
                    }
                    expected_source_executable_sha256 = Some(required_text(
                        &mut arguments,
                        "--expected-source-executable-sha256",
                    )?);
                }
                Some("--allow-unauthenticated-legacy-base") => {
                    if options.allow_unauthenticated_legacy_base {
                        return Err("--allow-unauthenticated-legacy-base 只能指定一次".to_owned());
                    }
                    options.allow_unauthenticated_legacy_base = true;
                }
                Some("--provider") => {
                    options.retry_scope_explicit = true;
                    options
                        .provider_filters
                        .insert(required_text(&mut arguments, "--provider")?);
                }
                Some("--model") => {
                    options.retry_scope_explicit = true;
                    options
                        .model_filters
                        .insert(required_text(&mut arguments, "--model")?);
                }
                Some("--max-attempts") => {
                    max_attempts_explicit = true;
                    options.max_attempts = required_usize(&mut arguments, "--max-attempts")?;
                    if !(1..=3).contains(&options.max_attempts) {
                        return Err("--max-attempts 必须在 1 到 3 之间".to_owned());
                    }
                }
                Some("--request-timeout-secs") => {
                    request_timeout_explicit = true;
                    options.request_timeout_secs =
                        required_u64(&mut arguments, "--request-timeout-secs")?;
                    if options.request_timeout_secs == 0 {
                        return Err("--request-timeout-secs 必须大于零".to_owned());
                    }
                }
                Some("--catalog-only") => {
                    options.retry_scope_explicit = true;
                    options.catalog_only = true;
                }
                Some("--diagnostics-only") => {
                    options.retry_scope_explicit = true;
                    options.diagnostics_only = true;
                }
                Some("--capability") => {
                    capability_explicit = true;
                    options.retry_scope_explicit = true;
                    let value = required_text(&mut arguments, "--capability")?;
                    for capability in value.split(',') {
                        let capability = capability.trim();
                        if capability.is_empty() {
                            return Err("--capability 不能包含空能力名称".to_owned());
                        }
                        options.capabilities.insert(ProbeKind::parse(capability)?);
                    }
                }
                Some("--full") => {
                    options.retry_scope_explicit = true;
                    options.full_matrix = true;
                }
                Some("--help" | "-h") => return Err(help_text().to_owned()),
                Some(other) => return Err(format!("不认识的参数：{other}\n{}", help_text())),
                None => return Err("命令行参数必须是有效 Unicode".to_owned()),
            }
        }

        if options.verify_run_dir.is_some() {
            let conflicting_flags = [
                ("--output-root", output_root_explicit),
                ("--resume", options.resume_dir.is_some()),
                ("--recover-from", recovery_source_dir.is_some()),
                ("--retry-from", retry_source_dir.is_some()),
                ("--retry-provider", retry_provider_id.is_some()),
                ("--retry-through-sequence", retry_through_sequence.is_some()),
                ("--consolidate-base", consolidation_base_dir.is_some()),
                ("--consolidate-retry", consolidation_retry_dir.is_some()),
                (
                    "--expected-source-executable-sha256",
                    expected_source_executable_sha256.is_some(),
                ),
                (
                    "--allow-unauthenticated-legacy-base",
                    options.allow_unauthenticated_legacy_base,
                ),
                ("--provider", !options.provider_filters.is_empty()),
                ("--model", !options.model_filters.is_empty()),
                ("--capability", capability_explicit),
                ("--full", options.full_matrix),
                ("--catalog-only", options.catalog_only),
                ("--diagnostics-only", options.diagnostics_only),
                ("--max-attempts", max_attempts_explicit),
                ("--request-timeout-secs", request_timeout_explicit),
            ];
            if let Some((flag, _)) = conflicting_flags.into_iter().find(|(_, present)| *present) {
                return Err(format!("--verify-run 不能与 {flag} 同时使用"));
            }
        }

        if options.full_matrix && capability_explicit {
            return Err("--full 不能与 --capability 同时使用".to_owned());
        }
        if options.resume_dir.is_some() && output_root_explicit {
            return Err("--resume 已指定完整运行目录，不能与 --output-root 同时使用".to_owned());
        }
        let specialized_mode_count = usize::from(recovery_source_dir.is_some())
            + usize::from(retry_source_dir.is_some())
            + usize::from(consolidation_base_dir.is_some() || consolidation_retry_dir.is_some());
        if specialized_mode_count > 1 {
            return Err(
                "--recover-from、--retry-from 与 --consolidate-* 只能选择一种运行模式".to_owned(),
            );
        }
        if options.resume_dir.is_some() && specialized_mode_count > 0 {
            return Err(
                "--resume 不能与 --recover-from、--retry-from 或 --consolidate-* 同时使用"
                    .to_owned(),
            );
        }
        options.recovery = match (
            recovery_source_dir,
            expected_source_executable_sha256.as_deref(),
        ) {
            (Some(source_dir), Some(expected_source_executable_sha256)) => {
                if !is_sha256_digest(expected_source_executable_sha256) {
                    return Err(
                        "--expected-source-executable-sha256 必须是 sha256: 加 64 位小写十六进制"
                            .to_owned(),
                    );
                }
                Some(RecoveryOptions {
                    source_dir,
                    expected_source_executable_sha256: expected_source_executable_sha256.to_owned(),
                })
            }
            (Some(_), None) => {
                return Err(
                    "--recover-from 必须同时提供 --expected-source-executable-sha256".to_owned(),
                );
            }
            (None, _) => None,
        };
        options.retry = match (
            retry_source_dir,
            retry_provider_id,
            retry_through_sequence,
            expected_source_executable_sha256,
        ) {
            (
                Some(source_dir),
                Some(provider_id),
                Some(through_sequence),
                Some(expected_digest),
            ) => {
                validate_inline_value("补测 Provider 标识", &provider_id)?;
                if !is_sha256_digest(&expected_digest) {
                    return Err(
                        "--expected-source-executable-sha256 必须是 sha256: 加 64 位小写十六进制"
                            .to_owned(),
                    );
                }
                Some(RetryOptions {
                    source_dir,
                    provider_id,
                    through_sequence,
                    expected_source_executable_sha256: expected_digest,
                })
            }
            (Some(_), None, _, _) => {
                return Err("--retry-from 必须同时提供 --retry-provider".to_owned());
            }
            (Some(_), _, None, _) => {
                return Err("--retry-from 必须同时提供 --retry-through-sequence".to_owned());
            }
            (Some(_), _, _, None) => {
                return Err(
                    "--retry-from 必须同时提供 --expected-source-executable-sha256".to_owned(),
                );
            }
            (None, Some(_), _, _) | (None, _, Some(_), _) => {
                return Err(
                    "--retry-provider 与 --retry-through-sequence 只能和 --retry-from 一起使用"
                        .to_owned(),
                );
            }
            (None, None, None, Some(_)) if options.recovery.is_none() => {
                return Err(
                    "--expected-source-executable-sha256 只能与 --recover-from 或 --retry-from 同时使用"
                        .to_owned(),
                );
            }
            (None, None, None, _) => None,
        };
        options.consolidation = match (consolidation_base_dir, consolidation_retry_dir) {
            (Some(base_dir), Some(retry_dir)) => Some(ConsolidationOptions {
                base_dir,
                retry_dir,
            }),
            (Some(_), None) => {
                return Err("--consolidate-base 必须同时提供 --consolidate-retry".to_owned());
            }
            (None, Some(_)) => {
                return Err("--consolidate-retry 必须同时提供 --consolidate-base".to_owned());
            }
            (None, None) => None,
        };
        if options.allow_unauthenticated_legacy_base
            && options.recovery.is_none()
            && options.retry.is_none()
            && options.consolidation.is_none()
        {
            return Err(
                "--allow-unauthenticated-legacy-base 只能与 --recover-from、--retry-from 或 --consolidate-base 一起使用"
                    .to_owned(),
            );
        }
        if (options.retry.is_some() || options.consolidation.is_some())
            && (options.full_matrix
                || capability_explicit
                || options.catalog_only
                || options.diagnostics_only
                || !options.provider_filters.is_empty()
                || !options.model_filters.is_empty())
        {
            return Err(
                "精确补测与离线合并模式不能和 --provider、--model、--capability、--full、--catalog-only 或 --diagnostics-only 混用"
                    .to_owned(),
            );
        }
        if options.full_matrix && !options.model_filters.is_empty() {
            return Err("--full 必须覆盖全部候选模型，不能与 --model 同时使用".to_owned());
        }
        if options.catalog_only && (options.full_matrix || capability_explicit) {
            return Err("--catalog-only 不能与 --full 或 --capability 同时使用".to_owned());
        }
        if options.diagnostics_only
            && (options.catalog_only
                || options.full_matrix
                || capability_explicit
                || !options.model_filters.is_empty())
        {
            return Err(
                "--diagnostics-only 不能与 --catalog-only、--full、--capability 或 --model 同时使用"
                    .to_owned(),
            );
        }
        if options.full_matrix {
            options.capabilities.extend(ProbeKind::all());
        } else if options.retry.is_none()
            && options.consolidation.is_none()
            && options.verify_run_dir.is_none()
            && !options.catalog_only
            && !options.diagnostics_only
            && options.capabilities.is_empty()
        {
            options.capabilities.insert(ProbeKind::Text);
        }
        Ok(options)
    }

    /// 用已签入恢复清单的选择重建补测运行身份，不接受调用方扩大范围。
    pub(crate) fn apply_retry_runtime_shape(
        &mut self,
        provider_id: String,
        capabilities: BTreeSet<String>,
    ) -> Result<(), String> {
        self.provider_filters = BTreeSet::from([provider_id]);
        self.model_filters.clear();
        self.capabilities = capabilities
            .iter()
            .map(|capability| ProbeKind::parse(capability))
            .collect::<Result<BTreeSet<_>, String>>()?;
        self.catalog_only = false;
        self.diagnostics_only = false;
        self.full_matrix = false;
        Ok(())
    }

    /// 补测恢复只能使用清单冻结的范围，拒绝静默覆盖任何显式 CLI 范围或专用模式。
    pub(crate) fn reject_explicit_retry_resume_scope(&self) -> Result<(), String> {
        if self.retry_scope_explicit {
            return Err(
                "精确补测恢复不能和显式 --provider、--model、--capability、--full、--catalog-only 或 --diagnostics-only 混用"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

/// `providers.json` 中测试所需的最小根对象。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvidersFile {
    /// 当前用户配置的全部 Provider。
    pub(crate) providers: Vec<ProviderEntry>,
}

impl ProvidersFile {
    /// 从磁盘读取配置并校验 Provider 与模型标识。
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path)
            .map_err(|error| format!("无法读取 Provider 配置文件（路径不会写入报告）：{error}"))?;
        let file: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Provider 配置不是有效 JSON：{error}"))?;
        if file.providers.is_empty() {
            return Err("Provider 配置中没有任何 Provider".to_owned());
        }

        let mut provider_ids = BTreeSet::new();
        for provider in &file.providers {
            provider.validate()?;
            if !provider_ids.insert(provider.id.as_str()) {
                return Err(format!("Provider 标识重复：{}", provider.id));
            }
        }
        Ok(file)
    }

    /// 按命令行过滤条件返回保持配置顺序的 Provider。
    pub(crate) fn selected<'a>(
        &'a self,
        filters: &BTreeSet<String>,
    ) -> Result<Vec<&'a ProviderEntry>, String> {
        let selected = self
            .providers
            .iter()
            .filter(|provider| filters.is_empty() || filters.contains(&provider.id))
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err("Provider 过滤条件没有匹配任何配置".to_owned());
        }
        Ok(selected)
    }
}

/// 一个真实服务的最小可运行配置；API Key 永不参与序列化或调试输出。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderEntry {
    /// 用户配置中的稳定 Provider 标识。
    pub(crate) id: String,
    /// 仅用于终端进度展示的 Provider 名称。
    pub(crate) name: String,
    /// 保留反向代理路径前缀的基础地址。
    pub(crate) base_url: String,
    /// 配置中必须测试的精确模型标识。
    pub(crate) models: Vec<String>,
    /// 配置当前选择的协议名称，仅用于目录认证顺序。
    pub(crate) api_backend: String,
    /// 只在内存中使用且永不写盘的认证凭据。
    api_key: String,
}

impl ProviderEntry {
    /// 校验测试前即可发现的配置错误。
    fn validate(&self) -> Result<(), String> {
        validate_inline_value("Provider 标识", &self.id)?;
        validate_inline_value("Provider 名称", &self.name)?;
        if self.api_key.len() < 16 {
            return Err(format!(
                "Provider {} 的凭据少于 16 字节；真实测试恢复证明会形成离线校验器，请改用高熵测试凭据",
                self.id
            ));
        }
        let key = ApiKey::new(self.api_key.clone()).map_err(|error| error.to_string())?;
        ProviderConfig::new(
            self.id.clone(),
            self.configured_protocol()?,
            &self.base_url,
            key,
        )
        .map_err(|error| error.to_string())?;

        let mut model_ids = BTreeSet::new();
        for model in &self.models {
            validate_inline_value("配置模型标识", model)
                .map_err(|error| format!("Provider {} 的{error}", self.id))?;
            if !model_ids.insert(model.as_str()) {
                return Err(format!("Provider {} 的模型标识重复：{model}", self.id));
            }
        }
        Ok(())
    }

    /// 把配置选择转换为 Provider 中立协议枚举。
    pub(crate) fn configured_protocol(&self) -> Result<ProviderProtocol, String> {
        match self.api_backend.as_str() {
            "messages" | "anthropic" | "anthropic_messages" => Ok(ProviderProtocol::Messages),
            "chat_completions" | "openai" => Ok(ProviderProtocol::ChatCompletions),
            "responses" | "openai_responses" => Ok(ProviderProtocol::Responses),
            value => Err(format!(
                "Provider {} 使用了未知 apiBackend：{value}",
                self.id
            )),
        }
    }

    /// 为一个协议和响应模式创建完全独立的 HTTP Adapter。
    pub(crate) fn provider_config(
        &self,
        protocol: ProviderProtocol,
        response_mode: WireResponseMode,
        request_timeout_secs: u64,
    ) -> Result<ProviderConfig, String> {
        self.provider_config_with_credential(
            protocol,
            response_mode,
            request_timeout_secs,
            self.api_key.clone(),
        )
    }

    /// 使用仅存在于内存的替代凭据创建探测配置。
    pub(crate) fn provider_config_with_credential(
        &self,
        protocol: ProviderProtocol,
        response_mode: WireResponseMode,
        request_timeout_secs: u64,
        credential: String,
    ) -> Result<ProviderConfig, String> {
        let key = ApiKey::new(credential).map_err(|error| error.to_string())?;
        let mut config = ProviderConfig::new(self.id.clone(), protocol, &self.base_url, key)
            .map_err(|error| error.to_string())?;
        config.response_mode = response_mode;
        config.request_timeout = Duration::from_secs(request_timeout_secs);
        Ok(config)
    }

    /// 返回只暴露 Origin 与路径段数的基础端点证据。
    pub(crate) fn redacted_base_endpoint(&self) -> Result<String, String> {
        let config =
            self.provider_config(self.configured_protocol()?, WireResponseMode::Buffered, 1)?;
        let url = config.base_url();
        let host = url
            .host_str()
            .ok_or_else(|| "Provider 基础地址缺少主机".to_owned())?;
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let authority = url
            .port()
            .map_or(host.clone(), |port| format!("{host}:{port}"));
        let path_segments = url
            .path_segments()
            .map(|segments| segments.filter(|segment| !segment.is_empty()).count())
            .unwrap_or(0);
        Ok(format!(
            "{}://{authority}/[PATH_REDACTED;segments={path_segments}]",
            url.scheme()
        ))
    }

    /// 只返回请求路径段数，不保存不透明值或可被低熵字典枚举的裸摘要。
    pub(crate) fn redacted_endpoint_path(&self, path: &str) -> String {
        let segment_count = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count();
        format!("[PATH_REDACTED;segments={segment_count}]")
    }

    /// 使用 Provider 凭据对去凭据配置计算域分离 HMAC，避免路径裸摘要被离线枚举。
    pub(crate) fn fingerprint(&self) -> Result<String, String> {
        let mut models = self.models.clone();
        models.sort();
        let input = serde_json::json!({
            "id": self.id,
            "baseUrl": self.base_url,
            "apiBackend": self.api_backend,
            "models": models,
        });
        let bytes = serde_json::to_vec(&input)
            .map_err(|error| format!("无法构造 Provider 配置摘要：{error}"))?;
        let mut message = b"keencode-provider-config-fingerprint-v2".to_vec();
        let byte_len =
            u64::try_from(bytes.len()).map_err(|_| "Provider 配置摘要长度溢出".to_owned())?;
        message.extend_from_slice(&byte_len.to_be_bytes());
        message.extend_from_slice(&bytes);
        Ok(format!(
            "hmac-sha256:{}",
            hex_encode(&hmac_sha256(self.api_key.as_bytes(), &message))
        ))
    }

    /// 使用当前运行随机盐生成不可跨运行关联且不暴露凭据的恢复证明。
    pub(crate) fn credential_resume_proof(&self, run_salt: &str) -> String {
        self.credential_resume_proof_with_domain(
            b"keencode-provider-resume-credential-v2-fact-authenticated",
            run_salt,
        )
    }

    /// 仅为显式接受的上一版无事实认证基础运行重建旧凭据证明。
    pub(crate) fn legacy_credential_resume_proof(&self, run_salt: &str) -> String {
        self.credential_resume_proof_with_domain(
            b"keencode-provider-resume-credential-v1",
            run_salt,
        )
    }

    /// 使用指定版本域对运行盐与 Provider 标识生成长度前缀凭据证明。
    fn credential_resume_proof_with_domain(&self, domain: &[u8], run_salt: &str) -> String {
        let mut message = domain.to_vec();
        for part in [run_salt.as_bytes(), self.id.as_bytes()] {
            let part_len = u64::try_from(part.len()).expect("恢复凭据证明字段长度必须能表示为 u64");
            message.extend_from_slice(&part_len.to_be_bytes());
            message.extend_from_slice(part);
        }
        format!(
            "hmac-sha256:{}",
            hex_encode(&hmac_sha256(self.api_key.as_bytes(), &message))
        )
    }

    /// 使用独立 HMAC 域把补测选择摘要绑定到凭据证明，阻止公开摘要被整体重算替换。
    pub(crate) fn credential_retry_resume_proof(
        &self,
        run_salt: &str,
        selection_sha256: &str,
    ) -> String {
        let mut message = b"keencode-provider-retry-resume-credential-v1".to_vec();
        for part in [
            run_salt.as_bytes(),
            self.id.as_bytes(),
            selection_sha256.as_bytes(),
        ] {
            let part_len =
                u64::try_from(part.len()).expect("补测恢复凭据证明字段长度必须能表示为 u64");
            message.extend_from_slice(&part_len.to_be_bytes());
            message.extend_from_slice(part);
        }
        format!(
            "hmac-sha256:{}",
            hex_encode(&hmac_sha256(self.api_key.as_bytes(), &message))
        )
    }

    /// 对一条规范探测记录计算包含前序 MAC 的链式提交证明。
    pub(crate) fn journal_record_proof(
        &self,
        run_salt: &str,
        selection_domain: &str,
        sequence: u64,
        previous_mac: &str,
        canonical_record: &[u8],
    ) -> String {
        self.keyed_artifact_proof(
            b"keencode-provider-journal-record-v1",
            &[
                run_salt.as_bytes(),
                selection_domain.as_bytes(),
                &sequence.to_be_bytes(),
                previous_mac.as_bytes(),
                canonical_record,
            ],
        )
    }

    /// 对去除状态证明字段后的规范恢复清单核心计算 Provider 独立状态证明。
    pub(crate) fn resume_state_proof(
        &self,
        run_salt: &str,
        canonical_manifest_core: &[u8],
    ) -> String {
        self.keyed_artifact_proof(
            b"keencode-provider-resume-state-v1",
            &[
                self.id.as_bytes(),
                run_salt.as_bytes(),
                canonical_manifest_core,
            ],
        )
    }

    /// 使用统一长度前缀编码生成域分离 HMAC，避免字段拼接碰撞和跨用途复用。
    fn keyed_artifact_proof(&self, domain: &[u8], parts: &[&[u8]]) -> String {
        let mut message = domain.to_vec();
        for part in parts {
            let part_len =
                u64::try_from(part.len()).expect("Provider 状态证明字段长度必须能表示为 u64");
            message.extend_from_slice(&part_len.to_be_bytes());
            message.extend_from_slice(part);
        }
        format!(
            "hmac-sha256:{}",
            hex_encode(&hmac_sha256(self.api_key.as_bytes(), &message))
        )
    }

    /// 为单次运行内的实际模型文本生成不可跨记录移用的 HMAC 证据。
    pub(crate) fn response_text_proof(&self, stable_key: &str, text: &str) -> String {
        let mut message = b"keencode-provider-response-text-v1".to_vec();
        for part in [self.id.as_bytes(), stable_key.as_bytes(), text.as_bytes()] {
            let part_len = u64::try_from(part.len()).expect("响应文本证据字段长度必须能表示为 u64");
            message.extend_from_slice(&part_len.to_be_bytes());
            message.extend_from_slice(part);
        }
        format!(
            "hmac-sha256:{}",
            hex_encode(&hmac_sha256(self.api_key.as_bytes(), &message))
        )
    }

    /// 统计候选输出中当前完整凭据的实际出现次数。
    pub(crate) fn output_credential_match_count(&self, output: &str) -> usize {
        if self.api_key.is_empty() {
            0
        } else {
            let raw = output.matches(&self.api_key).count();
            let escaped = serde_json::to_string(&self.api_key).ok().and_then(|value| {
                value
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .map(str::to_owned)
            });
            raw + escaped
                .filter(|value| value != &self.api_key)
                .map_or(0, |value| output.matches(&value).count())
        }
    }

    /// 只清理完整凭据、认证头与常见 API Key，保留控制字符供错误证据统一计数。
    pub(crate) fn redact_credentials(&self, text: &str) -> String {
        let mut redacted = if self.api_key.is_empty() {
            text.to_owned()
        } else {
            text.replace(&self.api_key, "[REDACTED]")
        };
        redacted = redact_header_values(&redacted);
        redacted = redact_prefixed_tokens(&redacted, "sk-");
        redact_labeled_credentials(&redacted)
    }

    /// 清理凭据并把不可信显示控制字符转换成可见 ASCII 转义。
    pub(crate) fn redact_text(&self, text: &str) -> String {
        escape_untrusted_inline_text(&self.redact_credentials(text))
    }
}

/// 判断字符是否会改变终端、Markdown 或双向文本的可信显示边界。
///
/// CR、LF 与 TAB 由调用方按结构化文本语境决定是否允许；其余 C0/C1、ANSI/OSC
/// 控制字节以及 Unicode 双向、换行和零宽格式控制均视为危险。
pub(crate) fn is_dangerous_display_character(character: char) -> bool {
    (character.is_control() && !matches!(character, '\r' | '\n' | '\t'))
        || matches!(
            character,
            '\u{00ad}'
                | '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
        )
}

/// 判断不可信单行字段是否含 CR、LF、TAB 或其他危险显示字符。
pub(crate) fn contains_unsafe_inline_character(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(character, '\r' | '\n' | '\t') || is_dangerous_display_character(character)
    })
}

/// 把不可信单行文本中的危险字符编码成可见 ASCII 转义，避免终端和产物注入。
pub(crate) fn escape_untrusted_inline_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\r' | '\n' | '\t') || is_dangerous_display_character(character) {
            write!(&mut escaped, "\\u{{{:04x}}}", u32::from(character))
                .expect("写入 String 不会失败");
        } else {
            escaped.push(character);
        }
    }
    escaped
}

/// 校验来自配置、命令行或恢复清单的单行标识不会注入控制序列。
pub(crate) fn validate_inline_value(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field}不能为空"));
    }
    if value != value.trim() {
        return Err(format!("{field}不能包含首尾空白"));
    }
    if contains_unsafe_inline_character(value) {
        return Err(format!("{field}包含控制字符或 Unicode 方向格式字符"));
    }
    Ok(())
}

/// 返回按固定顺序执行的全部三种协议。
pub(crate) const fn all_protocols() -> [ProviderProtocol; 3] {
    [
        ProviderProtocol::Messages,
        ProviderProtocol::ChatCompletions,
        ProviderProtocol::Responses,
    ]
}

/// 返回稳定且适合结果文件的协议名称。
pub(crate) const fn protocol_name(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::Messages => "anthropic_messages",
        ProviderProtocol::ChatCompletions => "openai_chat_completions",
        ProviderProtocol::Responses => "openai_responses",
    }
}

/// 返回稳定且适合结果文件的线上响应模式名称。
pub(crate) const fn response_mode_name(mode: WireResponseMode) -> &'static str {
    match mode {
        WireResponseMode::Streaming => "streaming",
        WireResponseMode::Buffered => "buffered",
    }
}

/// 计算小写十六进制 SHA-256，避免把配置正文写入报告。
pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_encode(&digest)
}

/// 把任意字节编码为稳定小写十六进制且不执行额外 Hash。
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        write!(&mut output, "{byte:02x}").expect("写入 String 不会失败");
        output
    })
}

/// 使用 SHA-256 实现 RFC 2104 HMAC，避免把凭据或裸摘要写入恢复清单。
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    /// SHA-256 的固定压缩块字节数。
    const BLOCK_SIZE: usize = 64;
    let mut normalized_key = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        normalized_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= normalized_key[index];
        outer_pad[index] ^= normalized_key[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

/// 返回不受临时目录环境变量影响的用户级 KeenCode 数据目录。
fn default_user_data_directory() -> Result<PathBuf, String> {
    let profile = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .ok_or_else(|| "无法确定用户数据目录，不能建立全局真实测试锁".to_owned())?;
    Ok(PathBuf::from(profile).join(".keencode"))
}

/// 读取下一个非空 Unicode 文本参数。
fn required_text(
    arguments: &mut impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<String, String> {
    let value = arguments
        .next()
        .ok_or_else(|| format!("{flag} 缺少参数值"))?;
    let value = value
        .into_string()
        .map_err(|_| format!("{flag} 的参数值必须是有效 Unicode"))?;
    if value.trim().is_empty() {
        return Err(format!("{flag} 的参数值不能为空"));
    }
    validate_inline_value(&format!("{flag} 的参数值"), &value)?;
    Ok(value)
}

/// 读取下一个可表示本机路径的参数。
fn required_path(
    arguments: &mut impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} 缺少路径参数"))
}

/// 读取只读核验目录，并拒绝把紧随其后的命令行 flag 当作运行目录。
fn required_verify_run_path(
    arguments: &mut impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<PathBuf, String> {
    let value = arguments
        .next()
        .ok_or_else(|| format!("{flag} 缺少路径参数"))?;
    if value.to_str().is_some_and(|value| value.starts_with('-')) {
        return Err(format!("{flag} 缺少路径参数"));
    }
    Ok(PathBuf::from(value))
}

/// 读取下一个 `usize` 参数。
fn required_usize(
    arguments: &mut impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<usize, String> {
    required_text(arguments, flag)?
        .parse::<usize>()
        .map_err(|error| format!("{flag} 不是有效整数：{error}"))
}

/// 读取下一个 `u64` 参数。
fn required_u64(arguments: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<u64, String> {
    required_text(arguments, flag)?
        .parse::<u64>()
        .map_err(|error| format!("{flag} 不是有效整数：{error}"))
}

/// 验证命令行中的构建摘要使用唯一规范 SHA-256 文本格式。
fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

/// 返回不包含任何凭据参数的命令行帮助。
fn help_text() -> &'static str {
    "用法：keencode-provider-live-test [--config PATH] [--verify-run RUN_DIR | --output-root PATH | --resume RUN_DIR | --recover-from RUN_DIR --expected-source-executable-sha256 SHA256 [--allow-unauthenticated-legacy-base] | --retry-from RUN_DIR --retry-provider ID --retry-through-sequence N --expected-source-executable-sha256 SHA256 [--allow-unauthenticated-legacy-base] | --consolidate-base RUN_DIR --consolidate-retry RUN_DIR [--allow-unauthenticated-legacy-base]] [--provider ID] [--model ID] [--capability NAME[,NAME...]] [--full] [--max-attempts 1..3] [--request-timeout-secs N] [--catalog-only] [--diagnostics-only]\n默认只运行 text；--verify-run 只读核验指定的已完成运行，除 --config 外不能与其他运行、范围或超时参数混用；--resume 只接受身份完全一致的未完成运行，补测恢复不接受任何显式范围或专用模式参数且绝不重建选择 Sidecar；--recover-from 处理遗失原可执行文件的未完成运行，显式接受上一版来源时会建立只读隔离升级并仅重跑无法按当前契约复核的 tuple；--retry-from 只从已完成来源选择截止序号内指定 Provider 的 retryable、rate_limit 或 server_error 失败 tuple，并创建独立可恢复运行；上一版来源缺少事实认证，默认拒绝，只有显式 --allow-unauthenticated-legacy-base 才能用于隔离升级、一次性补测或合并；补测与恢复目标本身始终必须通过当前事实认证；--consolidate-* 只离线验证和合并基础运行与补测运行；--full 对全部候选模型运行全部能力并运行认证与缺失模型诊断。"
}

/// 清理常见 Authorization 与 x-api-key 请求头的值。
fn redact_header_values(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for segment in text.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let lower = line.to_ascii_lowercase();
        let sensitive = ["authorization:", "x-api-key:"]
            .iter()
            .filter_map(|marker| lower.find(marker).map(|index| (index, marker.len())))
            .min_by_key(|(index, _)| *index);
        if let Some((index, marker_len)) = sensitive {
            let value_start = index + marker_len;
            output.push_str(&line[..value_start]);
            output.push_str(" [REDACTED]");
            if line.ends_with('\r') {
                output.push('\r');
            }
        } else {
            output.push_str(line);
        }
        output.push_str(newline);
    }
    output
}

/// 清理以固定 ASCII 前缀开头且长度足以形似凭据的连续 Token。
fn redact_prefixed_tokens(text: &str, prefix: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find(prefix) {
        let start = cursor + relative;
        output.push_str(&text[cursor..start]);
        let mut end = start + prefix.len();
        while text
            .as_bytes()
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.'))
        {
            end += 1;
        }
        if end - start >= prefix.len() + 8 {
            output.push_str("[REDACTED]");
        } else {
            output.push_str(&text[start..end]);
        }
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    output
}

/// 清理 Provider 错误中由常见凭据标签引出的完整值或掩码后缀。
fn redact_labeled_credentials(text: &str) -> String {
    const LABELS: [&str; 4] = ["api key", "api-key", "api_key", "apikey"];
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while cursor < text.len() {
        let next = LABELS
            .iter()
            .filter_map(|label| {
                let mut search_from = cursor;
                while let Some(relative) = lower[search_from..].find(label) {
                    let start = search_from + relative;
                    let has_boundary = start == 0
                        || !bytes[start - 1].is_ascii_alphanumeric() && bytes[start - 1] != b'_';
                    let mut separator = start + label.len();
                    while bytes
                        .get(separator)
                        .is_some_and(|byte| byte.is_ascii_whitespace())
                    {
                        separator += 1;
                    }
                    if bytes
                        .get(separator)
                        .is_some_and(|byte| matches!(*byte, b'\'' | b'"'))
                    {
                        separator += 1;
                        while bytes
                            .get(separator)
                            .is_some_and(|byte| byte.is_ascii_whitespace())
                        {
                            separator += 1;
                        }
                    }
                    if has_boundary
                        && bytes
                            .get(separator)
                            .is_some_and(|byte| matches!(*byte, b':' | b'='))
                    {
                        let mut value_start = separator + 1;
                        while bytes.get(value_start).is_some_and(|byte| {
                            byte.is_ascii_whitespace() || matches!(*byte, b'\'' | b'"')
                        }) {
                            value_start += 1;
                        }
                        return Some((start, value_start));
                    }
                    search_from = start + label.len();
                }
                None
            })
            .min_by_key(|(start, _)| *start);
        let Some((marker_start, value_start)) = next else {
            output.push_str(&text[cursor..]);
            break;
        };
        let mut value_end = value_start;
        while bytes.get(value_end).is_some_and(|byte| {
            !byte.is_ascii_whitespace()
                && !matches!(*byte, b',' | b';' | b'\'' | b'"' | b'\\' | b'}' | b']')
        }) {
            value_end += 1;
        }
        if value_start == value_end {
            let label_end = marker_start + 1;
            output.push_str(&text[cursor..label_end]);
            cursor = label_end;
            continue;
        }
        output.push_str(&text[cursor..value_start]);
        output.push_str("[REDACTED]");
        cursor = value_end;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 使用 Unicode 文本构造可测试的命令行参数。
    fn parse_options(arguments: &[&str]) -> Result<RuntimeOptions, String> {
        RuntimeOptions::parse_from(arguments.iter().map(OsString::from))
    }

    /// 验证三种配置协议名称均映射到独立枚举。
    #[test]
    fn configured_protocol_覆盖三种协议() {
        for (api_backend, expected) in [
            ("messages", ProviderProtocol::Messages),
            ("chat_completions", ProviderProtocol::ChatCompletions),
            ("responses", ProviderProtocol::Responses),
        ] {
            let provider = ProviderEntry {
                id: "provider".to_owned(),
                name: "测试".to_owned(),
                base_url: "https://example.com/v1".to_owned(),
                models: vec!["model".to_owned()],
                api_backend: api_backend.to_owned(),
                api_key: "secret".to_owned(),
            };
            assert_eq!(provider.configured_protocol(), Ok(expected));
        }
    }

    /// 验证配置摘要由凭据加钥且不暴露凭据或凭据裸摘要。
    #[test]
    fn fingerprint_使用凭据hmac且不包含裸秘密() {
        let provider = |api_key: &str| ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: api_key.to_owned(),
        };
        let first_secret = "first-secret-value";
        let first = provider(first_secret)
            .fingerprint()
            .expect("配置 HMAC 应成功");
        assert_eq!(
            first,
            provider(first_secret)
                .fingerprint()
                .expect("相同配置 HMAC 应稳定")
        );
        assert_ne!(
            first,
            provider("second-secret-value")
                .fingerprint()
                .expect("换 Key 配置 HMAC 应成功")
        );
        assert_eq!(first.len(), "hmac-sha256:".len() + 64);
        assert!(first.strip_prefix("hmac-sha256:").is_some_and(|digest| {
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }));
        assert!(!first.contains(first_secret));
        assert!(!first.contains(&hex_digest(first_secret.as_bytes())));
    }

    /// 验证报告端点不暴露网关不透明路径，但配置摘要仍能发现路径变化。
    #[test]
    fn redacted_endpoint_隐藏路径且fingerprint绑定原地址() {
        let provider = |path: &str| ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: format!("https://example.com/{path}/v1"),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: "strong-synthetic-secret".to_owned(),
        };
        let first = provider("tenant-secret");
        let endpoint = first
            .redacted_base_endpoint()
            .expect("应能生成脱敏基础端点");
        assert!(endpoint.starts_with("https://example.com/[PATH_REDACTED;"));
        assert!(!endpoint.contains("tenant-secret"));
        assert!(!endpoint.contains("sha256"));
        assert_ne!(
            first.fingerprint().expect("首个配置摘要应成功"),
            provider("other-tenant")
                .fingerprint()
                .expect("第二个配置摘要应成功")
        );
        let path = first.redacted_endpoint_path("/tenant-secret/v1/responses");
        assert!(!path.contains("tenant-secret"));
        assert!(!path.contains("sha256"));
        assert!(path.contains("segments=3"));
    }

    /// 验证当前凭据证明使用独立于 legacy v1 的域，且换 Key、换盐时变化并不暴露裸摘要。
    #[test]
    fn credential_resume_proof_独立于legacy域且换key换盐不暴露裸摘要() {
        let provider = |api_key: &str| ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: api_key.to_owned(),
        };
        let first_secret = "first-secret";
        let first = provider(first_secret).credential_resume_proof("run-salt");
        assert_ne!(
            first,
            provider("second-secret").credential_resume_proof("run-salt")
        );
        assert_ne!(
            first,
            provider("first-secret").credential_resume_proof("other-salt")
        );
        assert!(first.starts_with("hmac-sha256:"));
        assert!(!first.contains(first_secret));
        assert!(!first.contains(&hex_digest(first_secret.as_bytes())));

        let legacy = provider(first_secret).legacy_credential_resume_proof("run-salt");
        let mut legacy_message = b"keencode-provider-resume-credential-v1".to_vec();
        for part in [b"run-salt".as_slice(), b"provider".as_slice()] {
            legacy_message.extend_from_slice(&(part.len() as u64).to_be_bytes());
            legacy_message.extend_from_slice(part);
        }
        assert_eq!(
            legacy,
            format!(
                "hmac-sha256:{}",
                hex_encode(&hmac_sha256(first_secret.as_bytes(), &legacy_message))
            ),
            "legacy v1 凭据证明必须保持原算法字节"
        );
        assert_ne!(first, legacy, "当前事实认证证明必须与 legacy v1 域分离");
    }

    /// 验证补测凭据证明使用独立域并不可分离地绑定选择摘要。
    #[test]
    fn credential_retry_resume_proof_绑定选择摘要且不改变普通证明() {
        let provider = ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: "synthetic-secret".to_owned(),
        };
        let first_selection = format!("sha256:{}", "a".repeat(64));
        let second_selection = format!("sha256:{}", "b".repeat(64));
        let ordinary = provider.credential_resume_proof("run-salt");
        let retry = provider.credential_retry_resume_proof("run-salt", &first_selection);

        assert_ne!(retry, ordinary, "补测证明必须使用独立 HMAC 域");
        assert_ne!(
            retry,
            provider.credential_retry_resume_proof("run-salt", &second_selection),
            "补测选择摘要变化必须改变凭据证明"
        );
        assert_eq!(
            ordinary,
            provider.credential_resume_proof("run-salt"),
            "计算补测证明不得改变普通证明"
        );
    }

    /// 验证真实测试拒绝会被恢复 HMAC 低成本枚举的明显弱凭据。
    #[test]
    fn provider_validate_拒绝短凭据离线校验器() {
        let provider = ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: "x".to_owned(),
        };
        assert!(
            provider
                .validate()
                .expect_err("短凭据必须在真实请求前失败")
                .contains("少于 16 字节")
        );
    }

    /// 验证配置中的 Provider 标识、名称和模型标识均拒绝终端与双向文本注入。
    #[test]
    fn provider_validate_拒绝危险显示字符() {
        let provider = || ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: "strong-synthetic-secret".to_owned(),
        };
        for (field, mutate) in [
            ("Provider 标识", 0_u8),
            ("Provider 名称", 1_u8),
            ("配置模型标识", 2_u8),
        ] {
            for attack in [
                "safe\r\nforged",
                "safe\u{001b}]0;owned\u{0007}",
                "safe\u{009b}31mowned",
                "safe\u{202e}txt.exe",
                "safe\u{2066}owned\u{2069}",
                "safe\u{200b}hidden",
            ] {
                let mut candidate = provider();
                match mutate {
                    0 => candidate.id = attack.to_owned(),
                    1 => candidate.name = attack.to_owned(),
                    2 => candidate.models = vec![attack.to_owned()],
                    _ => unreachable!("测试字段索引必须固定"),
                }
                let error = candidate
                    .validate()
                    .expect_err("危险显示字符必须在任何网络请求前被拒绝");
                assert!(error.contains(field), "实际错误：{error}");
                assert!(!error.contains(attack), "错误不得回显不可信原值");
            }
        }
    }

    /// 验证精确模型标识拒绝首尾空白，避免报告值与实际请求值被静默归一化。
    #[test]
    fn provider_validate_拒绝模型标识首尾空白() {
        for model in [" model", "model ", "\u{3000}model"] {
            let provider = ProviderEntry {
                id: "provider".to_owned(),
                name: "测试".to_owned(),
                base_url: "https://example.com/v1".to_owned(),
                models: vec![model.to_owned()],
                api_backend: "responses".to_owned(),
                api_key: "strong-synthetic-secret".to_owned(),
            };
            let error = provider
                .validate()
                .expect_err("带首尾空白的模型标识必须在真实请求前拒绝");
            assert!(error.contains("配置模型标识不能包含首尾空白"));
            assert!(!error.contains(model), "错误不得回显不可信模型标识");
        }
    }

    /// 验证危险字符分类允许结构化文本换行，但单行字段会额外拒绝 CR、LF 与 TAB。
    #[test]
    fn dangerous_display_character_覆盖控制与方向字符() {
        assert!(is_dangerous_display_character('\u{001b}'));
        assert!(is_dangerous_display_character('\u{009b}'));
        assert!(is_dangerous_display_character('\u{202e}'));
        assert!(is_dangerous_display_character('\u{2067}'));
        assert!(is_dangerous_display_character('\u{200b}'));
        assert!(!is_dangerous_display_character('\n'));
        assert!(!is_dangerous_display_character('\r'));
        assert!(!is_dangerous_display_character('\t'));
        assert!(!is_dangerous_display_character('中'));
        assert!(contains_unsafe_inline_character("safe\nforged"));
        assert!(contains_unsafe_inline_character("safe\tforged"));
    }

    /// 验证不可信文本会把控制序列转为可见 ASCII，且结果自身不再含危险字符。
    #[test]
    fn escape_untrusted_inline_text_清理终端和双向注入() {
        let escaped = escape_untrusted_inline_text(
            "safe\r\n\u{001b}]0;owned\u{0007}\u{009b}31m\u{202e}\u{2066}\u{200b}",
        );
        assert!(!contains_unsafe_inline_character(&escaped));
        for evidence in [
            "\\u{000d}",
            "\\u{000a}",
            "\\u{001b}",
            "\\u{0007}",
            "\\u{009b}",
            "\\u{202e}",
            "\\u{2066}",
            "\\u{200b}",
        ] {
            assert!(escaped.contains(evidence), "缺少转义证据：{evidence}");
        }
    }

    /// 验证响应文本证据只在 Key、稳定键和原始文本均相同时保持一致。
    #[test]
    fn response_text_proof_换key换稳定键或正文都变化() {
        let provider = |api_key: &str| ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: api_key.to_owned(),
        };
        let first = provider("first-secret").response_text_proof("stable-a", "original");
        assert_eq!(
            first,
            provider("first-secret").response_text_proof("stable-a", "original")
        );
        assert_ne!(
            first,
            provider("second-secret").response_text_proof("stable-a", "original")
        );
        assert_ne!(
            first,
            provider("first-secret").response_text_proof("stable-b", "original")
        );
        assert_ne!(
            first,
            provider("first-secret").response_text_proof("stable-a", "originaL")
        );
        assert!(first.starts_with("hmac-sha256:"));
        assert_eq!(first.len(), "hmac-sha256:".len() + 64);
        assert!(
            first["hmac-sha256:".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        );
    }

    /// 验证自研 HMAC-SHA256 与 RFC 4231 固定向量一致。
    #[test]
    fn hmac_sha256_符合rfc4231向量() {
        let digest = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex_encode(&digest),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    /// 验证未指定能力时只运行成本最低的文本探测。
    #[test]
    fn parse_options_默认只选择文本能力() {
        let options = parse_options(&[]).expect("默认参数应有效");
        assert_eq!(options.capabilities, BTreeSet::from([ProbeKind::Text]));
        assert!(!options.full_matrix);
        assert!(!options.allow_unauthenticated_legacy_base);
        assert!(options.verify_run_dir.is_none());
        assert!(options.resume_dir.is_none());
        assert!(options.recovery.is_none());
        assert!(options.retry.is_none());
        assert!(options.consolidation.is_none());
    }

    /// 验证上一版未认证来源只能由隔离升级、精确补测或离线合并显式接受。
    #[test]
    fn parse_options_legacy来源显式接受只允许隔离升级补测与合并() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let recovery = parse_options(&[
            "--recover-from",
            "source-run",
            "--expected-source-executable-sha256",
            &digest,
            "--allow-unauthenticated-legacy-base",
        ])
        .expect("隔离升级应允许显式接受上一版未认证来源");
        assert!(recovery.recovery.is_some());
        assert!(recovery.allow_unauthenticated_legacy_base);

        let retry = parse_options(&[
            "--retry-from",
            "source-run",
            "--retry-provider",
            "provider",
            "--retry-through-sequence",
            "3857",
            "--expected-source-executable-sha256",
            &digest,
            "--allow-unauthenticated-legacy-base",
        ])
        .expect("精确补测应允许显式接受上一版未认证基础来源");
        assert!(retry.retry.is_some());
        assert!(retry.allow_unauthenticated_legacy_base);

        let consolidation = parse_options(&[
            "--consolidate-base",
            "base-run",
            "--consolidate-retry",
            "retry-run",
            "--allow-unauthenticated-legacy-base",
        ])
        .expect("离线合并应允许显式接受上一版未认证基础来源");
        assert!(consolidation.consolidation.is_some());
        assert!(consolidation.allow_unauthenticated_legacy_base);

        for arguments in [
            vec!["--allow-unauthenticated-legacy-base"],
            vec![
                "--resume",
                "existing-run",
                "--allow-unauthenticated-legacy-base",
            ],
        ] {
            assert!(
                parse_options(&arguments)
                    .expect_err("普通或原目录恢复必须拒绝上一版未认证来源开关")
                    .contains("只能与 --recover-from、--retry-from 或 --consolidate-base 一起使用")
            );
        }

        assert!(
            parse_options(&[
                "--allow-unauthenticated-legacy-base",
                "--allow-unauthenticated-legacy-base",
            ])
            .expect_err("上一版未认证基础来源开关不能重复指定")
            .contains("只能指定一次")
        );
    }

    /// 验证恢复参数只接受一个完整运行目录且不会与新运行输出根目录混用。
    #[test]
    fn parse_options_resume_拒绝歧义输出目录() {
        let options = parse_options(&["--resume", "existing-run"]).expect("单独指定恢复目录应有效");
        assert_eq!(options.resume_dir, Some(PathBuf::from("existing-run")));
        assert!(!options.retry_scope_explicit);
        assert!(options.reject_explicit_retry_resume_scope().is_ok());
        assert!(
            parse_options(&["--resume", "existing-run", "--output-root", "new-root",])
                .expect_err("恢复目录与新运行输出根目录不能并存")
                .contains("不能与 --output-root")
        );
        assert!(
            parse_options(&["--resume", "one", "--resume", "two"])
                .expect_err("恢复目录不能重复指定")
                .contains("只能指定一次")
        );
    }

    /// 验证只读完成运行核验可以显式指定配置，并拒绝重复或其他运行参数。
    #[test]
    fn parse_options_verify_run_成功重复且拒绝全部冲突参数() {
        let options = parse_options(&[
            "--verify-run",
            "completed-run",
            "--config",
            "providers.json",
        ])
        .expect("只读完成运行核验参数应有效");
        assert_eq!(options.verify_run_dir, Some(PathBuf::from("completed-run")));
        assert_eq!(options.config_path, PathBuf::from("providers.json"));
        assert!(options.capabilities.is_empty());
        assert!(!options.retry_scope_explicit);
        let path_with_spaces_and_unicode =
            parse_options(&["--verify-run", r"C:\核验目录\completed run"])
                .expect("只读核验目录应保留空格、Unicode 和 Windows 路径语义");
        assert_eq!(
            path_with_spaces_and_unicode.verify_run_dir,
            Some(PathBuf::from(r"C:\核验目录\completed run"))
        );
        assert!(
            parse_options(&["--verify-run"])
                .expect_err("只读核验目录不能缺少路径")
                .contains("--verify-run 缺少路径参数")
        );
        assert!(
            parse_options(&["--verify-run", "--config", "x"])
                .expect_err("只读核验不能把下一个 flag 当作运行目录")
                .contains("--verify-run 缺少路径参数")
        );
        assert!(
            parse_options(&["--verify-run", "run", "--config"])
                .expect_err("--config 不能缺少配置路径")
                .contains("--config 缺少路径参数")
        );
        assert!(
            parse_options(&["--config", "one", "--config", "two"])
                .expect_err("--config 不能静默覆盖前一个配置路径")
                .contains("--config 只能指定一次")
        );
        assert!(
            parse_options(&["--verify-run", "one", "--verify-run", "two"])
                .expect_err("只读完成运行核验目录不能重复指定")
                .contains("只能指定一次")
        );

        for arguments in [
            vec!["--verify-run", "run", "--output-root", "output"],
            vec!["--verify-run", "run", "--resume", "other"],
            vec!["--verify-run", "run", "--recover-from", "other"],
            vec!["--verify-run", "run", "--retry-from", "other"],
            vec!["--verify-run", "run", "--retry-provider", "provider"],
            vec!["--verify-run", "run", "--retry-through-sequence", "1"],
            vec!["--verify-run", "run", "--consolidate-base", "base"],
            vec!["--verify-run", "run", "--consolidate-retry", "retry"],
            vec![
                "--verify-run",
                "run",
                "--expected-source-executable-sha256",
                "not-a-digest",
            ],
            vec!["--verify-run", "run", "--allow-unauthenticated-legacy-base"],
            vec!["--verify-run", "run", "--provider", "provider"],
            vec!["--verify-run", "run", "--model", "model"],
            vec!["--verify-run", "run", "--capability", "text"],
            vec!["--verify-run", "run", "--full"],
            vec!["--verify-run", "run", "--catalog-only"],
            vec!["--verify-run", "run", "--diagnostics-only"],
            vec!["--verify-run", "run", "--max-attempts", "1"],
            vec!["--verify-run", "run", "--request-timeout-secs", "1"],
        ] {
            let error = parse_options(&arguments)
                .expect_err("只读完成运行核验不能与其他运行或范围参数混用");
            assert!(
                error.contains("--verify-run"),
                "实际冲突错误未标识核验模式：{error}"
            );
        }
    }

    /// 验证补测恢复拒绝全部显式范围与专用模式，同时不把默认文本能力误判为显式。
    #[test]
    fn parse_options_retry_resume_拒绝显式范围且允许默认能力() {
        for arguments in [
            vec!["--resume", "retry-run", "--provider", "provider"],
            vec!["--resume", "retry-run", "--model", "model"],
            vec!["--resume", "retry-run", "--capability", "text"],
            vec!["--resume", "retry-run", "--full"],
            vec!["--resume", "retry-run", "--catalog-only"],
            vec!["--resume", "retry-run", "--diagnostics-only"],
        ] {
            let options = parse_options(&arguments).expect("单个显式范围参数应先完成语法解析");
            assert!(options.retry_scope_explicit);
            assert!(
                options
                    .reject_explicit_retry_resume_scope()
                    .expect_err("补测恢复必须拒绝显式范围")
                    .contains("精确补测恢复")
            );
        }

        let options =
            parse_options(&["--resume", "retry-run"]).expect("未显式选择能力的恢复参数应有效");
        assert_eq!(options.capabilities, BTreeSet::from([ProbeKind::Text]));
        assert!(!options.retry_scope_explicit);
        assert!(options.reject_explicit_retry_resume_scope().is_ok());
    }

    /// 验证遗失可执行文件恢复必须显式提供来源目录与完整摘要，且输出到新目录。
    #[test]
    fn parse_options_recovery_要求成对参数并拒绝常规resume() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let options = parse_options(&[
            "--recover-from",
            "source-run",
            "--expected-source-executable-sha256",
            &digest,
            "--output-root",
            "recovered-runs",
        ])
        .expect("完整隔离恢复参数应有效");
        assert_eq!(
            options.recovery,
            Some(RecoveryOptions {
                source_dir: PathBuf::from("source-run"),
                expected_source_executable_sha256: digest.clone(),
            })
        );
        assert_eq!(options.output_root, PathBuf::from("recovered-runs"));
        assert!(
            parse_options(&["--recover-from", "source-run"])
                .expect_err("恢复来源不能缺少用户确认摘要")
                .contains("必须同时提供")
        );
        assert!(
            parse_options(&["--expected-source-executable-sha256", &digest,])
                .expect_err("确认摘要不能脱离恢复来源单独使用")
                .contains("只能与 --recover-from")
        );
        assert!(
            parse_options(&[
                "--resume",
                "source-run",
                "--recover-from",
                "source-run",
                "--expected-source-executable-sha256",
                &digest,
            ])
            .expect_err("常规恢复与隔离恢复不能同时启用")
            .contains("不能与 --recover-from")
        );
        assert!(
            parse_options(&[
                "--recover-from",
                "source-run",
                "--expected-source-executable-sha256",
                "A910",
            ])
            .expect_err("来源摘要必须使用完整规范格式")
            .contains("64 位小写十六进制")
        );
    }

    /// 验证精确补测必须完整绑定来源、Provider、截止序号和用户确认的构建摘要。
    #[test]
    fn parse_options_retry_要求完整来源身份并拒绝扩大范围() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let options = parse_options(&[
            "--retry-from",
            "source-run",
            "--retry-provider",
            "provider",
            "--retry-through-sequence",
            "3857",
            "--expected-source-executable-sha256",
            &digest,
            "--output-root",
            "retry-runs",
        ])
        .expect("完整精确补测参数应有效");
        assert_eq!(
            options.retry,
            Some(RetryOptions {
                source_dir: PathBuf::from("source-run"),
                provider_id: "provider".to_owned(),
                through_sequence: 3857,
                expected_source_executable_sha256: digest.clone(),
            })
        );
        assert!(options.capabilities.is_empty());
        assert!(options.provider_filters.is_empty());
        assert!(
            parse_options(&[
                "--retry-from",
                "source-run",
                "--retry-provider",
                "provider",
                "--retry-through-sequence",
                "3857",
            ])
            .expect_err("补测来源不能缺少用户确认构建摘要")
            .contains("expected-source-executable-sha256")
        );
        assert!(
            parse_options(&[
                "--retry-from",
                "source-run",
                "--retry-provider",
                "provider",
                "--retry-through-sequence",
                "3857",
                "--expected-source-executable-sha256",
                &digest,
                "--model",
                "extra-model",
            ])
            .expect_err("精确补测不能由调用方扩大模型范围")
            .contains("不能和 --provider、--model")
        );
        assert!(
            parse_options(&[
                "--retry-from",
                "source-run",
                "--retry-provider",
                "provider",
                "--retry-through-sequence",
                "0",
                "--expected-source-executable-sha256",
                &digest,
            ])
            .expect_err("补测截止序号必须大于零")
            .contains("必须大于零")
        );
    }

    /// 验证离线合并必须同时指定两份来源，且不能携带任何在线探测范围参数。
    #[test]
    fn parse_options_consolidation_要求成对来源且禁止在线参数() {
        let options = parse_options(&[
            "--consolidate-base",
            "base-run",
            "--consolidate-retry",
            "retry-run",
            "--output-root",
            "merged-runs",
        ])
        .expect("完整离线合并参数应有效");
        assert_eq!(
            options.consolidation,
            Some(ConsolidationOptions {
                base_dir: PathBuf::from("base-run"),
                retry_dir: PathBuf::from("retry-run"),
            })
        );
        assert!(options.capabilities.is_empty());
        assert!(
            parse_options(&["--consolidate-base", "base-run"])
                .expect_err("离线合并不能缺少补测来源")
                .contains("必须同时提供")
        );
        assert!(
            parse_options(&[
                "--consolidate-base",
                "base-run",
                "--consolidate-retry",
                "retry-run",
                "--full",
            ])
            .expect_err("离线合并不能触发在线完整矩阵")
            .contains("不能和 --provider、--model")
        );
    }

    /// 验证能力参数可重复且支持逗号分隔，并保持固定枚举顺序。
    #[test]
    fn parse_options_合并重复和逗号能力() {
        let options = parse_options(&[
            "--capability",
            "reasoning,structured_output",
            "--capability",
            "tool_calling",
        ])
        .expect("能力组合应有效");
        assert_eq!(
            options.capabilities,
            BTreeSet::from([
                ProbeKind::ToolCalling,
                ProbeKind::Reasoning,
                ProbeKind::StructuredOutput,
            ])
        );
    }

    /// 验证图片工具结果往返能力可以独立通过 CLI 名称选择并保持稳定枚举名称。
    #[test]
    fn parse_options_支持图片工具结果往返能力() {
        assert_eq!(
            ProbeKind::parse("tool_result_image_round_trip")
                .expect("图片工具结果往返能力名称应可解析"),
            ProbeKind::ToolResultImageRoundTrip
        );
        assert_eq!(
            ProbeKind::ToolResultImageRoundTrip.as_str(),
            "tool_result_image_round_trip"
        );
        let options = parse_options(&["--capability", "tool_result_image_round_trip"])
            .expect("图片工具结果往返能力应可独立选择");
        assert_eq!(
            options.capabilities,
            BTreeSet::from([ProbeKind::ToolResultImageRoundTrip])
        );
    }

    /// 验证完整矩阵包含全部能力且禁止模型过滤。
    #[test]
    fn parse_options_full_覆盖全部能力且禁止模型过滤() {
        let options = parse_options(&["--full"]).expect("完整矩阵参数应有效");
        assert_eq!(options.capabilities, BTreeSet::from(ProbeKind::all()));
        assert_eq!(options.capabilities.len(), 15);
        assert!(options.full_matrix);
        assert!(
            parse_options(&["--full", "--model", "one"])
                .expect_err("完整矩阵不能过滤模型")
                .contains("不能与 --model")
        );
    }

    /// 验证仅目录模式不会接受任何生成能力选择。
    #[test]
    fn parse_options_catalog_only_拒绝能力参数() {
        assert!(
            parse_options(&["--catalog-only", "--capability", "text"])
                .expect_err("仅目录模式不能生成")
                .contains("--catalog-only")
        );
        assert!(
            parse_options(&["--catalog-only", "--full"])
                .expect_err("仅目录模式不能运行完整矩阵")
                .contains("--catalog-only")
        );
    }

    /// 验证 Provider 负向诊断可独立运行且不会隐式加入模型文本探测。
    #[test]
    fn parse_options_diagnostics_only_不运行模型矩阵() {
        let options = parse_options(&["--diagnostics-only"]).expect("独立诊断参数应有效");
        assert!(options.diagnostics_only);
        assert!(options.capabilities.is_empty());
        assert!(
            parse_options(&["--diagnostics-only", "--model", "one"])
                .expect_err("独立诊断不能过滤模型")
                .contains("--diagnostics-only")
        );
    }

    /// 验证模型和 Provider 命令行过滤值不能向恢复清单注入控制字符。
    #[test]
    fn parse_options_拒绝危险过滤值() {
        for arguments in [
            ["--provider", "provider\nforged"],
            ["--model", "model\u{202e}txt.exe"],
            ["--model", "model\u{001b}]0;owned\u{0007}"],
        ] {
            let error = parse_options(&arguments).expect_err("危险过滤值必须被拒绝");
            assert!(error.contains("控制字符或 Unicode 方向格式字符"));
        }
    }

    /// 验证 CLI 模型与 Provider 标识保持精确字节，不静默去除首尾空白。
    #[test]
    fn parse_options_拒绝过滤标识首尾空白并保留内部空格() {
        for arguments in [
            ["--model", " model"],
            ["--provider", "provider "],
            ["--retry-provider", " provider"],
        ] {
            let error = parse_options(&arguments).expect_err("首尾空白过滤标识必须被拒绝");
            assert!(error.contains("不能包含首尾空白"), "实际错误：{error}");
        }

        let options = parse_options(&["--model", "model internal space"])
            .expect("模型标识中的内部空格不应被改写");
        assert_eq!(
            options.model_filters,
            BTreeSet::from(["model internal space".to_owned()])
        );
    }

    /// 验证完整凭据、认证头和常见 sk Token 均会被清理。
    #[test]
    fn redact_text_清理常见凭据形态() {
        let provider = ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: "exact-secret-value".to_owned(),
        };
        let synthetic_prefixed_secret = ["sk", "1234567890abcdef"].join("-");
        let redacted = provider.redact_text(&format!(
            "exact-secret-value\nAuthorization: Bearer another-value\nx-api-key: third-value\n{synthetic_prefixed_secret}\nAuthentication failed, api key: ****b930 is invalid\n{{\"api_key\":\"masked-suffix\"}}"
        ));
        assert!(!redacted.contains("exact-secret-value"));
        assert!(!redacted.contains("another-value"));
        assert!(!redacted.contains("third-value"));
        assert!(!redacted.contains(&synthetic_prefixed_secret));
        assert!(!redacted.contains("****b930"));
        assert!(!redacted.contains("masked-suffix"));
        assert!(redacted.contains("[REDACTED]"));
    }

    /// 验证报告字段共用的脱敏入口不会保留任意终端或方向控制字符。
    #[test]
    fn redact_text_同时清理危险显示字符() {
        let provider = ProviderEntry {
            id: "provider".to_owned(),
            name: "测试".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: "strong-synthetic-secret".to_owned(),
        };
        let redacted = provider.redact_text("model\n\u{001b}]0;owned\u{0007}\u{202e}\u{200b}");
        assert!(!contains_unsafe_inline_character(&redacted));
        assert!(redacted.contains("\\u{001b}"));
        assert!(redacted.contains("\\u{202e}"));
    }
}
