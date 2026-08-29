use std::path::Path;

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
    if normalized.starts_with(extended_prefix) {
        let local = &normalized[extended_prefix.len()..];
        if local.as_bytes().get(1) == Some(&b':') {
            return local.to_owned();
        }
    }

    normalized
}

#[cfg(test)]
mod tests {
    use super::path_text_to_frontend;

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
