//! 跨平台路径判断辅助函数。

/// 判断字符串是否符合 Unix、Windows 盘符或 UNC 绝对路径格式。
pub(crate) fn is_absolute_fs_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with(r"\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

#[cfg(test)]
mod tests {
    use super::is_absolute_fs_path;

    /// 验证 Unix 根路径和常见相对路径的判断结果。
    #[test]
    fn 判断_unix_和相对路径() {
        for (path, expected) in [
            ("/tmp/file.txt", true),
            ("/", true),
            ("src/main.rs", false),
            ("./src/main.rs", false),
            ("../src/main.rs", false),
            ("", false),
        ] {
            assert_eq!(is_absolute_fs_path(path), expected, "路径: {path}");
        }
    }

    /// 验证 Windows 盘符路径只接受带分隔符的绝对形式。
    #[test]
    fn 判断_windows_盘符路径() {
        for (path, expected) in [
            (r"C:\Users\dev\main.rs", true),
            ("d:/projects/app", true),
            (r"C:\", true),
            (r"C:relative\file.rs", false),
            (r"1:\not-a-drive", false),
        ] {
            assert_eq!(is_absolute_fs_path(path), expected, "路径: {path}");
        }
    }

    /// 验证 Windows 和斜杠形式 UNC 路径，并拒绝不完整的反斜杠前缀。
    #[test]
    fn 判断_unc路径() {
        for (path, expected) in [
            (r"\\server\share\file.txt", true),
            ("//server/share/file.txt", true),
            (r"\server\share\file.txt", false),
        ] {
            assert_eq!(is_absolute_fs_path(path), expected, "路径: {path}");
        }
    }
}
