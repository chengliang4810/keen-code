use rcode_agent::ToolEffect;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    #[default]
    Ask,
    Edit,
    FullAccess,
}

impl PermissionMode {
    pub fn auto_approves(self, tool_name: &str, effect: ToolEffect) -> bool {
        effect == ToolEffect::ReadOnly
            || self == Self::FullAccess
            || (self == Self::Edit
                && matches!(
                    tool_name,
                    "Write" | "Edit" | "MultiEdit" | "create_directory"
                ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_unknown_values_never_grant_additional_permissions() {
        assert_eq!(PermissionMode::default(), PermissionMode::Ask);
        assert!(serde_json::from_str::<PermissionMode>("\"yolo\"").is_err());
        assert!(serde_json::from_str::<PermissionMode>("null").is_err());
    }

    #[test]
    fn edit_only_auto_approves_registered_file_mutations() {
        for name in ["Write", "Edit", "MultiEdit", "create_directory"] {
            assert!(PermissionMode::Edit.auto_approves(name, ToolEffect::ChangesState));
            assert!(!PermissionMode::Ask.auto_approves(name, ToolEffect::ChangesState));
        }
        for name in [
            "Bash",
            "PowerShell",
            "bash_background",
            "bash_kill",
            "spawn_coding_agent",
            "send_to_agent",
            "mcp__server__Write",
            "unknown",
        ] {
            assert!(!PermissionMode::Edit.auto_approves(name, ToolEffect::ChangesState));
            assert!(!PermissionMode::Ask.auto_approves(name, ToolEffect::ChangesState));
            assert!(PermissionMode::FullAccess.auto_approves(name, ToolEffect::ChangesState));
        }
        for mode in [
            PermissionMode::Ask,
            PermissionMode::Edit,
            PermissionMode::FullAccess,
        ] {
            assert!(mode.auto_approves("Read", ToolEffect::ReadOnly));
        }
    }
}
