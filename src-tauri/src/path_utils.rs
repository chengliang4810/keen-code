use std::path::{Component, Path};

/// 判断路径是否为不含路径前缀、根目录或父目录段的安全相对路径。
///
/// 调用方必须在进入此谓词前自行决定是否 trim 文本，并在通过后继续执行各自
/// 的根目录、符号链接和错误类型校验；本函数只共享词法层面的判断。
pub(crate) fn is_safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
}

/// 将本机路径转换为前端统一使用的斜杠路径，并隐藏 Windows 扩展长度前缀。
pub(crate) fn path_to_frontend(path: &Path) -> String {
    path_text_to_frontend(&path.to_string_lossy())
}

/// 规范化可能已经序列化的本机路径，兼容盘符路径与 UNC 路径。
pub(crate) fn path_text_to_frontend(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let extended_unc_prefix = "//?/UNC/";
    if normalized
        .get(..extended_unc_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(extended_unc_prefix))
    {
        return format!("//{}", &normalized[extended_unc_prefix.len()..]);
    }

    let extended_prefix = "//?/";
    if let Some(local) = normalized
        .strip_prefix(extended_prefix)
        .filter(|local| local.as_bytes().get(1) == Some(&b':'))
    {
        return local.to_owned();
    }

    normalized
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_safe_relative_path, path_text_to_frontend};

    /// 词法相对路径谓词必须允许当前目录段，并拒绝越界及绝对路径组件。
    #[test]
    fn relative_path_predicate_has_one_cross_platform_contract() {
        for (raw, expected) in [
            ("plugin", true),
            ("./plugin", true),
            ("plugins/main", true),
            (".", true),
            ("../outside", false),
            ("plugins/../../outside", false),
            ("/absolute", false),
        ] {
            assert_eq!(
                is_safe_relative_path(Path::new(raw)),
                expected,
                "路径: {raw}"
            );
        }
    }

    /// 两个业务包装必须复用同一个词法谓词，防止路径规则再次各自漂移。
    #[test]
    fn relative_path_callers_delegate_lexical_check() {
        let claude_source = include_str!("claude_plugins.rs");
        let claude_wrapper = claude_source
            .split("fn safe_relative_path")
            .nth(1)
            .and_then(|source| source.split("fn safe_relative_join").next())
            .expect("Claude 相对路径包装应存在");
        let marketplace_source = include_str!("extensions/marketplace_source.rs");
        let marketplace_wrapper = marketplace_source
            .split("pub(super) fn validate_source_relative_path")
            .nth(1)
            .and_then(|source| source.split("/// 通过 reqwest").next())
            .expect("marketplace 相对路径包装应存在");

        assert!(claude_wrapper.contains("is_safe_relative_path(path)"));
        assert!(marketplace_wrapper.contains("is_safe_relative_path(path)"));
        assert!(!claude_wrapper.contains("components().any"));
        assert!(!marketplace_wrapper.contains("components().any"));
    }

    /// Windows 盘符和 UNC 根组件必须与 Unix 绝对路径一样被拒绝。
    #[cfg(windows)]
    #[test]
    fn relative_path_predicate_rejects_windows_roots() {
        for raw in [r"C:\outside", r"\\server\share\outside", r"\outside"] {
            assert!(!is_safe_relative_path(Path::new(raw)), "路径: {raw}");
        }
    }

    /// Windows 扩展盘符路径在界面中只显示普通盘符路径。
    #[test]
    fn removes_windows_extended_drive_prefix() {
        assert_eq!(
            path_text_to_frontend(r"\\?\D:\projects\keen-code"),
            "D:/projects/keen-code"
        );
        assert_eq!(
            path_text_to_frontend("//?/D:/projects/keen-code"),
            "D:/projects/keen-code"
        );
    }

    /// Windows 扩展 UNC 路径在界面中恢复为标准 UNC 路径。
    #[test]
    fn restores_windows_unc_prefix() {
        assert_eq!(
            path_text_to_frontend(r"\\?\UNC\server\share\project"),
            "//server/share/project"
        );
        assert_eq!(
            path_text_to_frontend("//?/unc/server/share/project"),
            "//server/share/project"
        );
    }

    /// 普通 Windows、UNC 与 Unix 路径只统一分隔符，不改变路径身份。
    #[test]
    fn preserves_regular_paths() {
        assert_eq!(
            path_text_to_frontend(r"D:\projects\keen-code"),
            "D:/projects/keen-code"
        );
        assert_eq!(
            path_text_to_frontend(r"\\server\share\project"),
            "//server/share/project"
        );
        assert_eq!(path_text_to_frontend("/opt/keen-code"), "/opt/keen-code");
    }
}
