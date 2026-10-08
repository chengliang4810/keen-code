//! 系统编辑器发现与受控打开。

use serde::Serialize;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use crate::native_paths::NativePaths;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorInfo {
    pub id: String,
    pub name: String,
    pub executable: PathBuf,
    pub is_file_manager: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenEditorResult {
    pub editor_id: String,
    pub path: PathBuf,
}

#[derive(Clone)]
pub struct EditorService {
    paths: Arc<NativePaths>,
}

impl EditorService {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self { paths }
    }

    pub fn discover(&self) -> Vec<EditorInfo> {
        let mut editors = Vec::new();
        #[cfg(windows)]
        discover_windows(&mut editors);
        #[cfg(target_os = "macos")]
        discover_macos(&mut editors);
        #[cfg(all(unix, not(target_os = "macos")))]
        discover_unix(&mut editors);
        editors
    }

    pub fn open(&self, editor_id: &str, path: &Path) -> Result<OpenEditorResult, String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("编辑器目标不可访问：{error}"))?;
        if !path.exists() {
            return Err("编辑器目标不存在".to_owned());
        }
        let editor = self
            .discover()
            .into_iter()
            .find(|editor| editor.id == editor_id)
            .ok_or_else(|| format!("未知或未安装的编辑器：{editor_id}"))?;
        let args = if editor.is_file_manager {
            file_manager_args(&path)
        } else {
            vec![path.as_os_str().to_owned()]
        };
        let mut command = Command::new(&editor.executable);
        command.args(&args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command
            .spawn()
            .map_err(|error| format!("无法启动编辑器 {}：{error}", editor.name))?;
        Ok(OpenEditorResult {
            editor_id: editor.id,
            path,
        })
    }

    pub fn paths(&self) -> &Arc<NativePaths> {
        &self.paths
    }
}

fn push_editor(
    editors: &mut Vec<EditorInfo>,
    id: &'static str,
    name: &'static str,
    executable: PathBuf,
    is_file_manager: bool,
) {
    if (executable.is_file() || (cfg!(target_os = "macos") && executable.is_dir()))
        && editors.iter().all(|editor| editor.id != id)
    {
        editors.push(EditorInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            executable,
            is_file_manager,
        });
    }
}

fn path_command(command: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|directory| {
            let candidate = directory.join(command);
            candidate.is_file().then_some(candidate)
        })
    })
}

#[cfg(windows)]
fn windows_env_path(key: &str, suffix: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .map(|root| root.join(suffix))
}

#[cfg(windows)]
fn discover_windows(editors: &mut Vec<EditorInfo>) {
    push_editor(
        editors,
        "explorer",
        "资源管理器",
        windows_env_path("WINDIR", "explorer.exe")
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\explorer.exe")),
        true,
    );
    for (id, name, candidates) in [
        (
            "vscode",
            "VS Code",
            vec![
                windows_env_path("LOCALAPPDATA", r"Programs\Microsoft VS Code\Code.exe"),
                windows_env_path("ProgramFiles", r"Microsoft VS Code\Code.exe"),
                path_command("code.exe"),
            ],
        ),
        (
            "cursor",
            "Cursor",
            vec![
                windows_env_path("LOCALAPPDATA", r"Programs\cursor\Cursor.exe"),
                path_command("cursor.exe"),
            ],
        ),
        (
            "zed",
            "Zed",
            vec![
                windows_env_path("LOCALAPPDATA", r"Programs\Zed\zed.exe"),
                path_command("zed.exe"),
            ],
        ),
    ] {
        if let Some(path) = candidates.into_iter().flatten().find(|path| path.is_file()) {
            push_editor(editors, id, name, path, false);
        }
    }
}

#[cfg(target_os = "macos")]
fn discover_macos(editors: &mut Vec<EditorInfo>) {
    push_editor(
        editors,
        "finder",
        "Finder",
        PathBuf::from("/usr/bin/open"),
        true,
    );
    for (id, name, app) in [
        ("vscode", "VS Code", "Visual Studio Code.app"),
        ("cursor", "Cursor", "Cursor.app"),
        ("zed", "Zed", "Zed.app"),
    ] {
        let candidates = [
            PathBuf::from("/Applications").join(app),
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join("Applications").join(app)),
        ];
        if let Some(path) = candidates.into_iter().flatten().find(|path| path.exists()) {
            push_editor(editors, id, name, path, false);
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn discover_unix(editors: &mut Vec<EditorInfo>) {
    for (id, name, command) in [
        ("vscode", "VS Code", "code"),
        ("cursor", "Cursor", "cursor"),
        ("zed", "Zed", "zed"),
    ] {
        if let Some(path) = path_command(command) {
            push_editor(editors, id, name, path, false);
        }
    }
}

#[cfg(windows)]
fn file_manager_args(path: &Path) -> Vec<OsString> {
    if path.is_dir() {
        vec![path.as_os_str().to_owned()]
    } else {
        vec![OsString::from(format!("/select,{}", path.display()))]
    }
}

#[cfg(target_os = "macos")]
fn file_manager_args(path: &Path) -> Vec<OsString> {
    if path.is_dir() {
        vec![path.as_os_str().to_owned()]
    } else {
        vec![OsString::from("-R"), path.as_os_str().to_owned()]
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn file_manager_args(path: &Path) -> Vec<OsString> {
    vec![path.as_os_str().to_owned()]
}
