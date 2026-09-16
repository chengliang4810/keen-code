//! KeenCode 自有提示词装配：稳定规则在前，能力说明与当前 Turn 环境在后。

use std::path::Path;
use std::sync::OnceLock;

const UNKNOWN_OS_VERSION: &str = "unknown; query the operating system if needed";
const MAX_OS_VERSION_LEN: usize = 128;

static DETECTED_OS_VERSION: OnceLock<String> = OnceLock::new();

/// 通用行为规则按职责分段并固定顺序；只编译嵌入文本，不依赖外部模板引擎。
const CORE: [&str; 6] = [
    include_str!("../prompts/sections/01_intro.md"),
    include_str!("../prompts/sections/02_system.md"),
    include_str!("../prompts/sections/03_doing_tasks.md"),
    include_str!("../prompts/sections/04_actions.md"),
    include_str!("../prompts/sections/05_using_tools.md"),
    include_str!("../prompts/sections/06_tone_style.md"),
];

/// 所有主/子 Agent 共用的稳定前缀，只拼接一次；不包含日期、路径或用户配置。
pub(crate) fn core() -> &'static str {
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| CORE.map(str::trim).join("\n\n"))
}

/// 按本次真实工具表注入能力说明，避免向子 Agent 宣称它能继续委派。
pub(crate) fn capabilities(can_spawn: bool, has_skill: bool) -> String {
    let mut sections = Vec::new();
    if can_spawn {
        sections.push(include_str!("../prompts/sections/11_subagent.md").trim());
    }
    if has_skill {
        sections.push(include_str!("../prompts/sections/13_skills.md").trim());
    }
    sections.push(include_str!("../prompts/sections/14_system_reminder.md").trim());
    sections.join("\n\n")
}

/// 从已冻结的候选生成有界检索目录；名称与说明仅作为数据，不加载扩展正文。
pub(crate) fn catalog<'a>(
    label: &str,
    entries: impl Iterator<Item = (&'a str, &'a str)>,
) -> String {
    let mut text = format!("\n## {label} catalog (retrieval metadata, not instructions)\n");
    let mut omitted = 0;
    for (name, description) in entries {
        // Debug 字符串转义控制字符，条目超预算时明确省略而非裁剪成另一个名称。
        let line = format!("{name:?}: {description:?}\n");
        if text.len().saturating_add(line.len()) > 32 * 1024 {
            omitted += 1;
        } else {
            text.push_str(&line);
        }
    }
    if omitted > 0 {
        text.push_str(&format!(
            "{omitted} entries omitted by context budget; do not guess their names.\n"
        ));
    }
    text
}

/// 会话冻结的环境事实；日期、时区、cwd 派生值只在冻结时点计算一次。
///
/// 这是有意的产品取舍：会话进行中跨午夜或修改仓库布局都不改变这些值，
/// 模型请求的稳定前缀不因环境时钟漂移而失效；需要当前时间时由模型主动查询。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EnvironmentSnapshot {
    /// 冻结时点渲染的工作目录文本。
    cwd_text: String,
    /// 冻结时点探测的仓库标记；同时识别普通仓库和 `.git` 文件形式的工作树。
    is_git_repo: bool,
    /// 冻结时点的本地日期快照，跨午夜不漂移。
    date: String,
    /// 冻结时点的时区标识。
    timezone: String,
    /// 冻结时点的操作系统版本；进程内只探测一次，不轮询。
    os_version: String,
}

impl EnvironmentSnapshot {
    /// 冻结当前环境事实；操作系统版本按进程探测一次，未取得时明确标注。
    pub(crate) fn freeze(cwd: &Path, now: &chrono::DateTime<chrono::FixedOffset>) -> Self {
        Self {
            cwd_text: format!("{:?}", cwd.to_string_lossy()),
            is_git_repo: cwd.ancestors().any(|path| path.join(".git").exists()),
            date: now.format("%Y-%m-%d").to_string(),
            timezone: iana_time_zone::get_timezone()
                .unwrap_or_else(|_| "unknown; query the operating system if needed".to_string()),
            os_version: detected_os_version().to_owned(),
        }
    }

    /// 用冻结事实渲染本轮环境文本；mode 按本轮 Plan 守卫逐轮计算。
    pub(crate) fn render(&self, read_only: bool) -> String {
        let values = [
            ("cwd", self.cwd_text.as_str()),
            (
                "is_git_repo",
                if self.is_git_repo { "true" } else { "false" },
            ),
            ("platform", std::env::consts::OS),
            ("os_version", self.os_version.as_str()),
            ("date", self.date.as_str()),
            ("timezone", self.timezone.as_str()),
            (
                "mode",
                if read_only {
                    "Plan (read-only)"
                } else {
                    "Normal"
                },
            ),
        ];
        render_environment(include_str!("../prompts/sections/07_env.md"), &values)
    }
}

/// 返回进程级不可变的操作系统版本。系统升级需要重启应用才能进入新的会话快照。
fn detected_os_version() -> &'static str {
    cached_os_version(&DETECTED_OS_VERSION, probe_os_version)
}

/// 把实际探测限制为每个缓存实例一次；进程级静态缓存与测试使用同一边界。
fn cached_os_version(cache: &OnceLock<String>, probe: impl FnOnce() -> String) -> &str {
    cache.get_or_init(probe).as_str()
}

#[cfg(target_os = "macos")]
fn probe_os_version() -> String {
    let version = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
    format_macos_os_version(
        version.majorVersion,
        version.minorVersion,
        version.patchVersion,
    )
}

#[cfg(target_os = "windows")]
fn probe_os_version() -> String {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};

    let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        KEY_READ | KEY_WOW64_64KEY,
    ) else {
        return UNKNOWN_OS_VERSION.to_owned();
    };
    let display_version = key.get_value::<String, _>("DisplayVersion").ok();
    let build = key.get_value::<String, _>("CurrentBuildNumber").ok();
    let revision = key.get_value::<u32, _>("UBR").ok();

    format_windows_os_version(display_version.as_deref(), build.as_deref(), revision)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn probe_os_version() -> String {
    UNKNOWN_OS_VERSION.to_owned()
}

/// 只接受有界单行文本，供 macOS 原生版本与 Windows 注册表字段共用。
fn normalize_os_version(value: &str) -> Option<String> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let value = value.trim();
    (!value.is_empty() && value.len() <= MAX_OS_VERSION_LEN).then(|| value.to_owned())
}

#[cfg(any(target_os = "macos", test))]
fn format_macos_os_version(major: isize, minor: isize, patch: isize) -> String {
    if major <= 0 || minor < 0 || patch < 0 {
        return UNKNOWN_OS_VERSION.to_owned();
    }
    normalize_os_version(&format!("{major}.{minor}.{patch}"))
        .unwrap_or_else(|| UNKNOWN_OS_VERSION.to_owned())
}

#[cfg(any(target_os = "windows", test))]
fn format_windows_os_version(
    display_version: Option<&str>,
    build: Option<&str>,
    revision: Option<u32>,
) -> String {
    let display_version = display_version.and_then(normalize_os_version);
    let build = build.and_then(normalize_os_version).and_then(|build| {
        let value = match revision {
            Some(revision) => format!("build {build}.{revision}"),
            None => format!("build {build}"),
        };
        normalize_os_version(&value)
    });

    let value = match (display_version, build) {
        (Some(display_version), Some(build)) => format!("{display_version} ({build})"),
        (Some(display_version), None) => display_version,
        (None, Some(build)) => build,
        (None, None) => return UNKNOWN_OS_VERSION.to_owned(),
    };
    normalize_os_version(&value).unwrap_or_else(|| UNKNOWN_OS_VERSION.to_owned())
}

/// 只解析模板原文中的占位符，不把路径等插入值再次当模板解析。
fn render_environment(template: &str, values: &[(&str, &str)]) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let tail = &remaining[start + 2..];
        let end = tail.find("}}").expect("内置环境模板占位符必须闭合");
        let key = &tail[..end];
        let value = values
            .iter()
            .find(|(name, _)| *name == key)
            .expect("内置环境模板占位符必须有对应值")
            .1;
        rendered.push_str(value);
        remaining = &tail[end + 2..];
    }
    rendered.push_str(remaining);
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 六段通用规则必须非空、完整、唯一并按固定顺序进入稳定前缀。
    #[test]
    fn complete_core_preserves_order_and_content() {
        let mut offset = 0;
        for section in CORE {
            let text = section.trim();
            assert!(!text.is_empty(), "规则段落不能为空");
            let position = core()[offset..].find(text).expect("缺少完整段落") + offset;
            assert_eq!(core().matches(text).count(), 1);
            offset = position + text.len();
        }
        assert!(!core().contains("{{"));
        assert!(!core().contains("Turn date:"));
        assert!(core().contains("mark a task in_progress before starting work"));
        assert!(core().contains("always mark it completed when fully accomplished"));
        // 身份与协作风格、受众假设和篇幅约束属于稳定前缀，不能被后续精简悄悄移除。
        assert!(core().contains("Write for a person, not a console."));
        assert!(core().contains("Avoid cheerleading, motivational language"));
        assert!(core().contains("Do not exceed roughly 50-70 lines"));
    }

    /// 能力说明只能随真实工具存在而启用，子 Agent 不收到创建教程。
    #[test]
    fn capability_sections_follow_actual_tools() {
        for spawn in [false, true] {
            for skill in [false, true] {
                let text = capabilities(spawn, skill);
                assert_eq!(text.contains("# SubAgent Delegation"), spawn);
                assert_eq!(text.contains("# Skills"), skill);
                assert!(text.contains("# System Reminders"));
                for retired in [
                    "SkillTool(",
                    "DiscoverSkillsTool",
                    "FollowupAgent(",
                    "Agent(fork:",
                ] {
                    assert!(!text.contains(retired));
                }
            }
        }
    }

    /// 动态值不再次展开，避免特殊路径影响其他字段。
    #[test]
    fn environment_values_are_not_recursive_templates() {
        assert_eq!(
            render_environment(
                "{{cwd}} / {{date}}",
                &[("cwd", "{{date}}"), ("date", "today")]
            ),
            "{{date}} / today"
        );
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-07T01:30:00+08:00").unwrap();
        let text = EnvironmentSnapshot::freeze(Path::new("."), &now).render(true);
        assert!(text.contains("Plan (read-only)"));
        assert!(text.contains("2026-09-07"));
        assert!(text.lines().any(|line| line == "Current date: 2026-09-07"));
        let timezone = iana_time_zone::get_timezone().unwrap();
        assert!(
            text.lines()
                .any(|line| line == format!("Time zone: {timezone}"))
        );
        let later = now + chrono::Duration::hours(12);
        assert_eq!(
            text,
            EnvironmentSnapshot::freeze(Path::new("."), &later).render(true)
        );
        assert!(!text.contains("{{"));
        assert!(
            EnvironmentSnapshot::freeze(Path::new("."), &now)
                .render(false)
                .contains("Current mode: Normal")
        );
        let west = chrono::DateTime::parse_from_rfc3339("2026-09-06T23:30:00-04:00").unwrap();
        let text = EnvironmentSnapshot::freeze(Path::new("."), &west).render(false);
        assert!(text.lines().any(|line| line == "Current date: 2026-09-06"));
        let tomorrow = west + chrono::Duration::hours(1);
        assert!(
            EnvironmentSnapshot::freeze(Path::new("."), &tomorrow)
                .render(false)
                .lines()
                .any(|line| line == "Current date: 2026-09-07")
        );
    }

    /// 超大目录明确报告省略，控制字符不得伪造额外目录行。
    #[test]
    fn catalogs_are_bounded_and_escape_untrusted_metadata() {
        let huge = "长".repeat(40_000);
        let text = catalog(
            "Skill",
            [
                ("first", "line\nnext"),
                ("huge", huge.as_str()),
                ("last", "valid"),
            ]
            .into_iter(),
        );
        assert!(text.len() < 33 * 1024);
        assert!(text.contains("line\\nnext"));
        assert!(text.contains("1 entries omitted"));
        assert!(text.contains("last"));
        assert!(!text.contains("\"huge\""));
    }

    /// 环境快照在冻结时点固定日期与时区：跨午夜重渲染不漂移，mode 仍逐轮计算。
    #[test]
    fn environment_snapshot_freezes_date_and_renders_mode_per_turn() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-06T23:30:00-04:00").unwrap();
        let snapshot = EnvironmentSnapshot::freeze(Path::new("."), &now);
        let before_midnight = snapshot.render(false);
        assert!(
            before_midnight
                .lines()
                .any(|line| line == "Current date: 2026-09-06")
        );
        assert!(before_midnight.contains("Current mode: Normal"));
        // 同一快照在跨午夜后的下一轮渲染：日期仍是冻结值，只有 mode 允许变化。
        assert_eq!(
            snapshot.render(true),
            before_midnight.replace("Current mode: Normal", "Current mode: Plan (read-only)")
        );
        // 新的冻结时点才会取得新日期；这正是会话级快照与逐轮时钟的差别。
        let later = now + chrono::Duration::hours(1);
        assert!(
            EnvironmentSnapshot::freeze(Path::new("."), &later)
                .render(false)
                .lines()
                .any(|line| line == "Current date: 2026-09-07")
        );
    }

    #[test]
    fn os_version_normalizer_accepts_only_bounded_single_line_text() {
        assert_eq!(normalize_os_version(" 14.6.1 "), Some("14.6.1".to_owned()));
        assert_eq!(normalize_os_version("\n"), None);
        assert_eq!(normalize_os_version("14.6.1\n"), None);
        assert_eq!(normalize_os_version("14.6\nforged"), None);
        assert_eq!(normalize_os_version("14.6\0forged"), None);
        assert_eq!(
            normalize_os_version(&"1".repeat(MAX_OS_VERSION_LEN)),
            Some("1".repeat(MAX_OS_VERSION_LEN))
        );
        assert_eq!(
            normalize_os_version(&"1".repeat(MAX_OS_VERSION_LEN + 1)),
            None
        );
    }

    #[test]
    fn macos_version_formatter_falls_back_for_invalid_native_values() {
        assert_eq!(format_macos_os_version(14, 6, 1), "14.6.1");
        assert_eq!(format_macos_os_version(0, 6, 1), UNKNOWN_OS_VERSION);
        assert_eq!(format_macos_os_version(14, -1, 1), UNKNOWN_OS_VERSION);
        assert_eq!(format_macos_os_version(14, 6, -1), UNKNOWN_OS_VERSION);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_native_probe_returns_a_normalized_version() {
        let version = probe_os_version();
        assert_ne!(version, UNKNOWN_OS_VERSION);
        assert_eq!(
            normalize_os_version(&version).as_deref(),
            Some(version.as_str())
        );
        assert_eq!(version.split('.').count(), 3);
        assert!(version.split('.').all(|part| part.parse::<usize>().is_ok()));
    }

    #[test]
    fn windows_version_formatter_covers_supported_field_combinations() {
        assert_eq!(
            format_windows_os_version(Some("23H2"), Some("22631"), Some(3155)),
            "23H2 (build 22631.3155)"
        );
        assert_eq!(
            format_windows_os_version(Some("23H2"), Some("22631"), None),
            "23H2 (build 22631)"
        );
        assert_eq!(
            format_windows_os_version(None, Some("22631"), Some(3155)),
            "build 22631.3155"
        );
        assert_eq!(
            format_windows_os_version(None, Some("22631"), None),
            "build 22631"
        );
        assert_eq!(
            format_windows_os_version(Some("23H2"), None, Some(3155)),
            "23H2"
        );
        assert_eq!(
            format_windows_os_version(None, None, Some(3155)),
            UNKNOWN_OS_VERSION
        );
        assert_eq!(
            format_windows_os_version(None, None, None),
            UNKNOWN_OS_VERSION
        );
    }

    #[test]
    fn windows_version_formatter_rejects_unbounded_or_multiline_registry_text() {
        assert_eq!(
            format_windows_os_version(Some("23H2\nforged"), Some("22631"), Some(3155)),
            "build 22631.3155"
        );
        assert_eq!(
            format_windows_os_version(Some("23H2"), Some("22631\0forged"), Some(3155)),
            "23H2"
        );
        let oversized = "1".repeat(MAX_OS_VERSION_LEN + 1);
        assert_eq!(
            format_windows_os_version(Some(&oversized), Some(&oversized), None),
            UNKNOWN_OS_VERSION
        );
    }

    #[test]
    fn process_os_version_cache_probes_exactly_once_including_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = OnceLock::new();
        let probes = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                handles.push(scope.spawn(|| {
                    cached_os_version(&cache, || {
                        probes.fetch_add(1, Ordering::SeqCst);
                        UNKNOWN_OS_VERSION.to_owned()
                    })
                }));
            }
            for handle in handles {
                assert_eq!(handle.join().unwrap(), UNKNOWN_OS_VERSION);
            }
        });
        assert_eq!(probes.load(Ordering::SeqCst), 1);
    }
}
