//! 校验原输入框的技能选择；保留原消息正文，由已有 Skill 工具加载技能。
use crate::extensions::{ExtensionsState, SkillDto};
use serde::Deserialize;
use tauri::{AppHandle, State};

#[derive(Deserialize)]
pub struct SkillReference {
    name: String,
    path: String,
}

/// 只接受本次后端目录中可由用户调用的技能；路径是身份校验，不是任意文件读取入口。
fn selected_prompt(
    text: &str,
    selected: &[SkillReference],
    catalog: &[SkillDto],
) -> Result<String, String> {
    if selected.is_empty() {
        return Ok(text.to_owned());
    }
    if selected.len() > 32 {
        return Err("一次最多选择 32 个技能".into());
    }
    for requested in selected {
        let skill = catalog
            .iter()
            .find(|skill| {
                skill.name == requested.name
                    && skill.path.replace('\\', "/") == requested.path.replace('\\', "/")
            })
            .ok_or("所选技能已失效或不属于当前目录，请重新选择")?;
        if !skill.enabled {
            return Err("所选技能已禁用，请重新选择".into());
        }
        if !skill.user_invocable {
            return Err("此技能不允许用户显式调用".into());
        }
        // 原页面把技能 chip 序列化为 /name 或 $name；没有正文引用的旁路选择不接受。
        let slash = format!("/{}", skill.name);
        let dollar = format!("${}", skill.name);
        if !text
            .split_whitespace()
            .any(|token| token == slash || token == dollar)
        {
            return Err("所选技能缺少原消息中的明确引用".into());
        }
    }
    // 不把宿主控制说明写进用户正文，原消息与技能 chip 可以按原页面恢复。
    Ok(text.to_owned())
}

// 技能发现涉及本机目录与扩展锁，在 Tauri 异步执行器运行，避免占用窗口消息线程。
#[tauri::command(async)]
pub fn ui_skill_prompt(
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    cwd: String,
    text: String,
    skills: Vec<SkillReference>,
) -> Result<String, String> {
    if text.len() > 4 * 1024 * 1024 {
        return Err("技能消息正文超过限制".into());
    }
    // 重新查询权威目录，避免页面的旧缓存或工作目录切换复用其他项目的选择。
    let catalog = crate::extensions::skills_list(Some(cwd), app, state)?;
    selected_prompt(&text, &skills, &catalog.skills)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_current_invocable_skill_can_enter_prompt_with_exact_identity() {
        let catalog = vec![SkillDto {
            name: "ui-proof".into(),
            description: "fixture".into(),
            source: "project".into(),
            path: "D:/fixture/.agents/skills/ui-proof/SKILL.md".into(),
            user_invocable: true,
            enabled: true,
        }];
        let selected = vec![SkillReference {
            name: "ui-proof".into(),
            path: "D:\\fixture\\.agents\\skills\\ui-proof\\SKILL.md".into(),
        }];
        let prompt = selected_prompt("/ui-proof 原请求", &selected, &catalog).unwrap();
        assert_eq!(prompt, "/ui-proof 原请求");
        assert!(!prompt.contains("D:/fixture"));
        assert!(selected_prompt("原请求", &selected, &catalog).is_err());
        assert!(selected_prompt("/ui-proof-other 原请求", &selected, &catalog).is_err());
        assert_eq!(
            selected_prompt("$ui-proof 原请求", &selected, &catalog).unwrap(),
            "$ui-proof 原请求"
        );
        let unknown = vec![SkillReference {
            name: "ui-proof".into(),
            path: "D:/other/SKILL.md".into(),
        }];
        assert!(selected_prompt("原请求", &unknown, &catalog).is_err());
        let mut disabled = catalog.clone();
        disabled[0].enabled = false;
        assert!(
            selected_prompt("/ui-proof 原请求", &selected, &disabled)
                .unwrap_err()
                .contains("已禁用")
        );
        let mut hidden = catalog;
        hidden[0].user_invocable = false;
        assert!(selected_prompt("原请求", &selected, &hidden).is_err());
        assert_eq!(selected_prompt("原请求", &[], &hidden).unwrap(), "原请求");
    }
}
