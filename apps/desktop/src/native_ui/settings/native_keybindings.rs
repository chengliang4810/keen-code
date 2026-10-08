//! Native GPUI 快捷键的严格持久化、校验和进程内快照。
//!
//! 快捷键设置是窗口行为的共享事实源：设置页负责写入，窗口控制和 Composer
//! 只读取同一份不可变快照。文件独立于 `AppSettings`，因此修改快捷键不会改变
//! 其他应用配置，也不会在启动时依赖 UI 实体。

use super::contracts::KeyboardSettings;
use crate::{native_paths::NativePaths, storage};
use gpui::Keystroke;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub(crate) const KEYBINDINGS_FILE_SCHEMA: &str = "keencode/native-keybindings";
pub(crate) const KEYBINDINGS_FILE_VERSION: u32 = 1;
const KEYBINDINGS_FILE_NAME: &str = "native-keybindings.json";
const MAX_KEYBINDINGS_FILE_BYTES: u64 = 16 * 1024;

/// 运行时快捷键动作。动作名只在 Rust 内部使用，持久化格式仍是用户可读的
/// GPUI keystroke 字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeKeybindingAction {
    ComposerSubmit,
    NewChat,
    Search,
    Quit,
}

impl NativeKeybindingAction {
    fn label(self) -> &'static str {
        match self {
            Self::ComposerSubmit => "Composer 发送",
            Self::NewChat => "新建会话",
            Self::Search => "搜索",
            Self::Quit => "退出",
        }
    }
}

/// 所有窗口消费者共享的进程内状态。读操作不需要持有设置适配器或 GPUI 实体。
#[derive(Clone)]
pub struct NativeKeybindingsState(Arc<RwLock<NativeKeybindingsSnapshot>>);

#[derive(Clone)]
struct NativeKeybindingsSnapshot {
    settings: KeyboardSettings,
    // 设置替换时一次性解析，keydown 热路径只需比较已解析的修饰键和键名。
    strokes: [Option<Keystroke>; 4],
}

impl NativeKeybindingsSnapshot {
    fn new(settings: KeyboardSettings) -> Self {
        let strokes = [
            Keystroke::parse(&settings.composer_submit).ok(),
            Keystroke::parse(&settings.new_chat).ok(),
            Keystroke::parse(&settings.search).ok(),
            Keystroke::parse(&settings.quit).ok(),
        ];
        Self { settings, strokes }
    }

    fn stroke_for(&self, action: NativeKeybindingAction) -> Option<&Keystroke> {
        let index = match action {
            NativeKeybindingAction::ComposerSubmit => 0,
            NativeKeybindingAction::NewChat => 1,
            NativeKeybindingAction::Search => 2,
            NativeKeybindingAction::Quit => 3,
        };
        self.strokes[index].as_ref()
    }
}

impl NativeKeybindingsState {
    pub fn new(initial: KeyboardSettings) -> Self {
        Self(Arc::new(RwLock::new(NativeKeybindingsSnapshot::new(
            initial,
        ))))
    }

    pub fn snapshot(&self) -> KeyboardSettings {
        self.0
            .read()
            .map(|value| value.settings.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().settings.clone())
    }

    /// 返回面向用户的快捷键文本；读取同一份快照，设置热更新后侧栏提示立即同步。
    pub fn display(&self, action: NativeKeybindingAction) -> String {
        let snapshot = match self.0.read() {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        };
        let value = match action {
            NativeKeybindingAction::ComposerSubmit => &snapshot.settings.composer_submit,
            NativeKeybindingAction::NewChat => &snapshot.settings.new_chat,
            NativeKeybindingAction::Search => &snapshot.settings.search,
            NativeKeybindingAction::Quit => &snapshot.settings.quit,
        };
        format_keystroke_label(value)
    }

    pub fn replace(&self, next: KeyboardSettings) {
        match self.0.write() {
            Ok(mut value) => *value = NativeKeybindingsSnapshot::new(next),
            Err(poisoned) => *poisoned.into_inner() = NativeKeybindingsSnapshot::new(next),
        }
    }

    /// 判断一次真实 GPUI keydown 是否命中当前动作；`key_char` 被有意忽略，
    /// 避免 IME/键盘布局生成的字符元数据让同一快捷键失配。
    pub fn matches(&self, action: NativeKeybindingAction, stroke: &Keystroke) -> bool {
        let snapshot = match self.0.read() {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        };
        snapshot
            .stroke_for(action)
            .map(|expected| same_stroke(expected, stroke))
            .unwrap_or(false)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct KeybindingsFile {
    schema: String,
    version: u32,
    composer_submit: String,
    new_chat: String,
    search: String,
    quit: String,
}

impl From<KeyboardSettings> for KeybindingsFile {
    fn from(value: KeyboardSettings) -> Self {
        Self {
            schema: KEYBINDINGS_FILE_SCHEMA.to_owned(),
            version: KEYBINDINGS_FILE_VERSION,
            composer_submit: value.composer_submit,
            new_chat: value.new_chat,
            search: value.search,
            quit: value.quit,
        }
    }
}

impl TryFrom<KeybindingsFile> for KeyboardSettings {
    type Error = String;

    fn try_from(value: KeybindingsFile) -> Result<Self, Self::Error> {
        if value.schema != KEYBINDINGS_FILE_SCHEMA || value.version != KEYBINDINGS_FILE_VERSION {
            return Err("快捷键设置格式版本不受支持".to_owned());
        }
        let settings = Self {
            composer_submit: value.composer_submit,
            new_chat: value.new_chat,
            search: value.search,
            quit: value.quit,
        };
        validate(&settings)?;
        Ok(settings)
    }
}

pub fn keybindings_file_path(paths: &NativePaths) -> PathBuf {
    paths.data_root.join(KEYBINDINGS_FILE_NAME)
}

/// 从磁盘读取并校验快捷键；首次启动没有文件时使用内置默认值。
pub fn read(paths: &NativePaths) -> Result<KeyboardSettings, String> {
    let path = keybindings_file_path(paths);
    let Some(bytes) =
        storage::read_private_bytes_bounded(&path, MAX_KEYBINDINGS_FILE_BYTES, "Native 快捷键设置")
            .map_err(|error| format!("读取快捷键设置失败：{error}"))?
    else {
        return Ok(KeyboardSettings::default());
    };
    let file: KeybindingsFile =
        serde_json::from_slice(&bytes).map_err(|error| format!("解析快捷键设置失败：{error}"))?;
    file.try_into()
        .map_err(|error| format!("快捷键设置无效：{error}"))
}

/// 校验完成后通过统一私有存储原子替换，保证进程重启不会读到半个 JSON。
pub fn write(paths: &NativePaths, settings: &KeyboardSettings) -> Result<(), String> {
    validate(settings)?;
    let bytes = serde_json::to_vec_pretty(&KeybindingsFile::from(settings.clone()))
        .map_err(|error| format!("编码快捷键设置失败：{error}"))?;
    if bytes.len() as u64 > MAX_KEYBINDINGS_FILE_BYTES {
        return Err("快捷键设置超过大小限制".to_owned());
    }
    storage::atomic_write_private(&keybindings_file_path(paths), &bytes)
        .map_err(|error| format!("写入快捷键设置失败：{error}"))
}

/// 严格限制为单个 GPUI keystroke。全局动作必须带修饰键，Composer 允许裸 Enter，
/// 但保留 Shift+Enter 作为多行换行，且所有动作必须互不冲突。
pub fn validate(settings: &KeyboardSettings) -> Result<(), String> {
    let composer = validate_stroke(
        &settings.composer_submit,
        NativeKeybindingAction::ComposerSubmit,
        false,
    )?;
    if composer.key == "enter" && composer.modifiers.shift {
        return Err("Composer 发送不能占用 Shift+Enter，需保留为换行".to_owned());
    }
    let new_chat = validate_stroke(&settings.new_chat, NativeKeybindingAction::NewChat, true)?;
    let search = validate_stroke(&settings.search, NativeKeybindingAction::Search, true)?;
    let quit = validate_stroke(&settings.quit, NativeKeybindingAction::Quit, true)?;

    // 这些按键由 Ely TextInput 保留；`enter` 和 `secondary-enter` 是 Composer
    // 自身的发送候选，因此允许 Composer 动作占用，但全局动作不能抢占它们。
    let mut reserved = vec![
        "backspace".to_owned(),
        "delete".to_owned(),
        "left".to_owned(),
        "right".to_owned(),
        "up".to_owned(),
        "down".to_owned(),
        "home".to_owned(),
        "end".to_owned(),
        "shift-left".to_owned(),
        "shift-right".to_owned(),
        "shift-up".to_owned(),
        "shift-down".to_owned(),
        "shift-home".to_owned(),
        "shift-end".to_owned(),
        "secondary-a".to_owned(),
        "secondary-c".to_owned(),
        "secondary-x".to_owned(),
        "secondary-z".to_owned(),
        "secondary-shift-z".to_owned(),
        "secondary-v".to_owned(),
        "shift-enter".to_owned(),
    ];
    let word_modifier = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    reserved.extend([
        format!("{word_modifier}-backspace"),
        format!("{word_modifier}-left"),
        format!("{word_modifier}-right"),
        format!("{word_modifier}-shift-left"),
        format!("{word_modifier}-shift-right"),
    ]);
    if cfg!(target_os = "macos") {
        reserved.extend([
            "cmd-left".to_owned(),
            "cmd-right".to_owned(),
            "cmd-shift-left".to_owned(),
            "cmd-shift-right".to_owned(),
            "ctrl-cmd-space".to_owned(),
        ]);
    } else {
        reserved.push("ctrl-y".to_owned());
    }
    let mut global_reserved = reserved.clone();
    global_reserved.push("secondary-enter".to_owned());

    for action in [
        (NativeKeybindingAction::ComposerSubmit, &composer),
        (NativeKeybindingAction::NewChat, &new_chat),
        (NativeKeybindingAction::Search, &search),
        (NativeKeybindingAction::Quit, &quit),
    ] {
        let reserved = if action.0 == NativeKeybindingAction::ComposerSubmit {
            &reserved
        } else {
            &global_reserved
        };
        if reserved.iter().any(|value| {
            Keystroke::parse(value)
                .map(|reserved| same_stroke(&reserved, action.1))
                .unwrap_or(false)
        }) {
            return Err(format!("{} 不能覆盖文本编辑快捷键", action.0.label()));
        }
    }

    let actions = [
        (NativeKeybindingAction::ComposerSubmit, composer),
        (NativeKeybindingAction::NewChat, new_chat),
        (NativeKeybindingAction::Search, search),
        (NativeKeybindingAction::Quit, quit),
    ];
    for (index, (action, stroke)) in actions.iter().enumerate() {
        if actions[index + 1..]
            .iter()
            .any(|(_, other)| same_stroke(stroke, other))
        {
            return Err(format!("{} 与其他快捷键冲突", action.label()));
        }
    }
    Ok(())
}

fn validate_stroke(
    value: &str,
    action: NativeKeybindingAction,
    require_modifier: bool,
) -> Result<Keystroke, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("{}必须是单个快捷键", action.label()));
    }
    let stroke = Keystroke::parse(value)
        .map_err(|_| format!("{}包含无法识别的快捷键：{value}", action.label()))?;
    if stroke.key.is_empty() || stroke.key_char.is_some() {
        return Err(format!("{}的快捷键格式无效", action.label()));
    }
    if require_modifier && !stroke.modifiers.modified() {
        return Err(format!(
            "{}必须包含 Ctrl、Alt、Shift 或 Cmd 修饰键",
            action.label()
        ));
    }
    if stroke.modifiers.function && stroke.key == "fn" {
        return Err(format!("{}不能只绑定 Fn 键", action.label()));
    }
    Ok(stroke)
}

fn same_stroke(left: &Keystroke, right: &Keystroke) -> bool {
    left.modifiers == right.modifiers && left.key.eq_ignore_ascii_case(&right.key)
}

fn format_keystroke_label(value: &str) -> String {
    let Ok(stroke) = Keystroke::parse(value) else {
        return value.to_owned();
    };
    let modifiers = stroke.modifiers;
    let mut parts: Vec<String> = Vec::new();
    if modifiers.control {
        parts.push("Ctrl".to_owned());
    }
    if modifiers.alt {
        parts.push("Alt".to_owned());
    }
    if modifiers.platform {
        parts.push(
            (if cfg!(target_os = "macos") {
                "Cmd"
            } else if cfg!(target_os = "windows") {
                "Win"
            } else {
                "Super"
            })
            .to_owned(),
        );
    }
    if modifiers.shift {
        parts.push("Shift".to_owned());
    }
    if modifiers.function {
        parts.push("Fn".to_owned());
    }
    let key = match stroke.key.as_str() {
        "backspace" => "Backspace".to_owned(),
        "delete" => "Delete".to_owned(),
        "enter" => "Enter".to_owned(),
        "escape" => "Esc".to_owned(),
        "space" => "Space".to_owned(),
        "tab" => "Tab".to_owned(),
        key if key.len() == 1 => key.to_ascii_uppercase(),
        key => key.to_owned(),
    };
    parts.push(key);
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_are_valid_and_use_existing_actions() {
        let settings = KeyboardSettings::default();
        validate(&settings).expect("默认快捷键应通过校验");
        assert_eq!(settings.new_chat, "secondary-n");
        assert_eq!(settings.search, "secondary-k");
        assert_eq!(settings.quit, "secondary-q");
    }

    #[test]
    fn rejects_invalid_and_conflicting_bindings() {
        let mut settings = KeyboardSettings {
            search: "not-a-key".to_owned(),
            ..KeyboardSettings::default()
        };
        assert!(validate(&settings).is_err());
        settings.search = settings.new_chat.clone();
        assert!(validate(&settings).is_err());
        settings.search = "secondary-c".to_owned();
        assert!(validate(&settings).is_err());
        settings.search = "secondary-f".to_owned();
        settings.composer_submit = "secondary-c".to_owned();
        assert!(validate(&settings).is_err());
        settings.search = "secondary-f".to_owned();
        settings.composer_submit = "shift-enter".to_owned();
        assert!(validate(&settings).is_err());
    }

    #[test]
    fn writes_and_cold_loads_a_strict_file() {
        let directory = tempfile::tempdir().expect("临时目录");
        let paths = NativePaths::from_data_root(directory.path().to_owned());
        let settings = KeyboardSettings {
            search: "secondary-f".to_owned(),
            ..KeyboardSettings::default()
        };
        write(&paths, &settings).expect("写入快捷键");
        assert_eq!(read(&paths).expect("重新读取快捷键"), settings);
        let text = fs::read_to_string(keybindings_file_path(&paths)).expect("读取 JSON");
        assert!(text.contains("keencode/native-keybindings"));
        assert!(text.contains("composerSubmit"));
    }

    #[test]
    fn runtime_snapshot_changes_without_rebinding_old_value() {
        let state = NativeKeybindingsState::new(KeyboardSettings::default());
        let old = Keystroke::parse("secondary-n").expect("旧快捷键");
        let next = Keystroke::parse("secondary-t").expect("新快捷键");
        assert!(state.matches(NativeKeybindingAction::NewChat, &old));
        let mut settings = state.snapshot();
        settings.new_chat = "secondary-t".to_owned();
        state.replace(settings);
        assert!(!state.matches(NativeKeybindingAction::NewChat, &old));
        assert!(state.matches(NativeKeybindingAction::NewChat, &next));
    }

    #[test]
    fn display_uses_the_runtime_binding() {
        let state = NativeKeybindingsState::new(KeyboardSettings {
            new_chat: "ctrl-shift-n".to_owned(),
            ..KeyboardSettings::default()
        });
        assert_eq!(
            state.display(NativeKeybindingAction::NewChat),
            "Ctrl+Shift+N"
        );
    }
}
