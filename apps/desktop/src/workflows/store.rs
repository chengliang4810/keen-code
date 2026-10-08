//! 已保存工作流定义的真实文件存储。
//!
//! 这里只有定义文件；run、event、artifact 均由 Workflow Journal 处理，避免把 JSON
//! 文件误当成运行时事实源。

use super::types::{
    InvalidWorkflowDefinition, MAX_WORKFLOW_DEFINITION_BYTES, WORKFLOW_VERSION, WorkflowDefinition,
    WorkflowDefinitionError, WorkflowDefinitionMeta, WorkflowGetResult, WorkflowListResult,
    WorkflowScope, canonical_hash, validate_definition, validate_workflow_name,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const WORKFLOW_DIRECTORY: &str = "workflows";

#[derive(Debug)]
pub enum WorkflowStoreError {
    InvalidTarget(String),
    InvalidDefinition(WorkflowDefinitionError),
    Io(io::Error),
    Json(serde_json::Error),
    NotFound(String),
    TargetExists(String),
}

impl std::fmt::Display for WorkflowStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTarget(value) => {
                write!(formatter, "invalid workflow storage target: {value}")
            }
            Self::InvalidDefinition(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
            Self::NotFound(name) => write!(formatter, "workflow not found: {name}"),
            Self::TargetExists(path) => write!(formatter, "workflow target already exists: {path}"),
        }
    }
}

impl std::error::Error for WorkflowStoreError {}

impl From<io::Error> for WorkflowStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for WorkflowStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<WorkflowDefinitionError> for WorkflowStoreError {
    fn from(error: WorkflowDefinitionError) -> Self {
        Self::InvalidDefinition(error)
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowStore {
    data_root: PathBuf,
    /// 由桌面装配层校验后注入的项目存储根；RPC 不从请求参数接收路径。
    project_storage: Option<PathBuf>,
}

impl WorkflowStore {
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            project_storage: None,
        }
    }

    /// 创建带授权项目存储根的 Store。项目路径必须来自宿主当前工作区解析结果，不能来自
    /// 前端 JSON；保留 `new` 方便没有项目上下文的全局定义测试和 CLI 装配。
    pub fn with_project_storage(
        data_root: impl Into<PathBuf>,
        project_storage: impl Into<PathBuf>,
    ) -> Self {
        Self {
            data_root: data_root.into(),
            project_storage: Some(project_storage.into()),
        }
    }

    pub fn authorized_project_storage(&self) -> Option<&Path> {
        self.project_storage.as_deref()
    }

    pub fn global_dir(&self) -> PathBuf {
        self.data_root.join(WORKFLOW_DIRECTORY)
    }

    pub fn project_dir(&self, project_storage: &Path) -> Result<PathBuf, WorkflowStoreError> {
        if project_storage.as_os_str().is_empty() {
            return Err(WorkflowStoreError::InvalidTarget(
                "project storage path is empty".to_owned(),
            ));
        }
        Ok(project_storage.join(WORKFLOW_DIRECTORY))
    }

    pub fn directory(
        &self,
        scope: WorkflowScope,
        project_storage: Option<&Path>,
    ) -> Result<PathBuf, WorkflowStoreError> {
        match scope {
            WorkflowScope::Global => Ok(self.global_dir()),
            WorkflowScope::Project => self.project_dir(
                project_storage
                    .or(self.authorized_project_storage())
                    .ok_or_else(|| {
                        WorkflowStoreError::InvalidTarget(
                            "project workflow requires project storage".to_owned(),
                        )
                    })?,
            ),
        }
    }

    pub fn path(
        &self,
        scope: WorkflowScope,
        project_storage: Option<&Path>,
        name: &str,
    ) -> Result<PathBuf, WorkflowStoreError> {
        validate_workflow_name(name)?;
        Ok(self
            .directory(scope, project_storage)?
            .join(format!("{name}.json")))
    }

    pub fn save(
        &self,
        scope: WorkflowScope,
        project_storage: Option<&Path>,
        definition: &WorkflowDefinition,
    ) -> Result<WorkflowDefinitionMeta, WorkflowStoreError> {
        validate_definition(definition)?;
        let path = self.path(scope, project_storage, &definition.meta.name)?;
        let bytes = serde_json::to_vec_pretty(definition)?;
        if bytes.len() > MAX_WORKFLOW_DEFINITION_BYTES {
            return Err(WorkflowStoreError::InvalidTarget(
                "workflow definition exceeds the size limit".to_owned(),
            ));
        }
        fs::create_dir_all(path.parent().expect("workflow path always has a parent"))?;
        atomic_write(&path, &bytes)?;
        self.meta(scope, path, definition)
    }

    pub fn list(
        &self,
        scope: WorkflowScope,
        project_storage: Option<&Path>,
    ) -> Result<WorkflowListResult, WorkflowStoreError> {
        let dir = self.directory(scope, project_storage)?;
        let mut workflows = Vec::new();
        let mut invalid = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(WorkflowListResult {
                    workflows,
                    invalid,
                    dir,
                });
            }
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            match read_definition(&path) {
                Ok(definition) => {
                    if let Err(error) = validate_definition(&definition) {
                        invalid.push(InvalidWorkflowDefinition {
                            path,
                            reason: error.to_string(),
                        });
                    } else {
                        workflows.push(self.meta(scope, path, &definition)?);
                    }
                }
                Err(error) => invalid.push(InvalidWorkflowDefinition {
                    path,
                    reason: error.to_string(),
                }),
            }
        }
        workflows.sort_by(|left, right| left.name.cmp(&right.name));
        invalid.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(WorkflowListResult {
            workflows,
            invalid,
            dir,
        })
    }

    pub fn get(
        &self,
        scope: WorkflowScope,
        project_storage: Option<&Path>,
        name: &str,
    ) -> Result<WorkflowGetResult, WorkflowStoreError> {
        let path = self.path(scope, project_storage, name)?;
        if !path.is_file() {
            return Ok(WorkflowGetResult {
                ok: false,
                name: Some(name.to_owned()),
                path: Some(path),
                scope: Some(scope),
                meta: None,
                definition: None,
                reason: Some("not_found".to_owned()),
                detail: None,
            });
        }
        match read_definition(&path) {
            Ok(definition) => {
                if let Err(error) = validate_definition(&definition) {
                    return Ok(WorkflowGetResult {
                        ok: false,
                        name: Some(name.to_owned()),
                        path: Some(path),
                        scope: Some(scope),
                        meta: None,
                        definition: None,
                        reason: Some("parse_error".to_owned()),
                        detail: Some(error.to_string()),
                    });
                }
                let meta = self.meta(scope, path.clone(), &definition)?;
                Ok(WorkflowGetResult {
                    ok: true,
                    name: Some(definition.meta.name.clone()),
                    path: Some(path),
                    scope: Some(scope),
                    meta: Some(meta),
                    definition: Some(definition),
                    reason: None,
                    detail: None,
                })
            }
            Err(error) => Ok(WorkflowGetResult {
                ok: false,
                name: Some(name.to_owned()),
                path: Some(path),
                scope: Some(scope),
                meta: None,
                definition: None,
                reason: Some("parse_error".to_owned()),
                detail: Some(error.to_string()),
            }),
        }
    }

    pub fn delete(
        &self,
        scope: WorkflowScope,
        project_storage: Option<&Path>,
        name: &str,
    ) -> Result<PathBuf, WorkflowStoreError> {
        let path = self.path(scope, project_storage, name)?;
        if !path.is_file() {
            return Err(WorkflowStoreError::NotFound(name.to_owned()));
        }
        fs::remove_file(&path)?;
        Ok(path)
    }

    /// 在全局与当前授权项目之间移动定义，不覆盖目标作用域中的同名文件。
    ///
    /// 移动保留原 JSON，因此定义的 meta、输入声明和节点结构不会因为切换作用域
    /// 被重新序列化或丢失；调用方仍需在移动前校验当前 revision。
    pub fn move_between_scopes(
        &self,
        from_scope: WorkflowScope,
        from_project_storage: Option<&Path>,
        to_scope: WorkflowScope,
        to_project_storage: Option<&Path>,
        name: &str,
    ) -> Result<(PathBuf, PathBuf), WorkflowStoreError> {
        if from_scope == to_scope {
            return Err(WorkflowStoreError::InvalidTarget(
                "workflow source and target scopes must differ".to_owned(),
            ));
        }
        let from = self.path(from_scope, from_project_storage, name)?;
        let to = self.path(to_scope, to_project_storage, name)?;
        if !from.is_file() {
            return Err(WorkflowStoreError::NotFound(name.to_owned()));
        }
        if to.exists() {
            return Err(WorkflowStoreError::TargetExists(to.display().to_string()));
        }
        fs::create_dir_all(to.parent().expect("workflow path always has a parent"))?;
        fs::rename(&from, &to)?;
        Ok((from, to))
    }

    fn meta(
        &self,
        scope: WorkflowScope,
        path: PathBuf,
        definition: &WorkflowDefinition,
    ) -> Result<WorkflowDefinitionMeta, WorkflowStoreError> {
        Ok(WorkflowDefinitionMeta {
            name: definition.meta.name.clone(),
            description: definition.meta.description.clone(),
            when_to_use: definition.meta.when_to_use.clone(),
            args: definition.inputs.clone(),
            scope,
            path,
            canonical_hash: canonical_hash(definition)?,
        })
    }
}

fn read_definition(path: &Path) -> Result<WorkflowDefinition, WorkflowStoreError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_WORKFLOW_DEFINITION_BYTES as u64 {
        return Err(WorkflowStoreError::InvalidTarget(format!(
            "workflow definition is not a regular file or is too large: {}",
            path.display()
        )));
    }
    let bytes = fs::read(path)?;
    let definition: WorkflowDefinition = serde_json::from_slice(&bytes)?;
    if definition.version != WORKFLOW_VERSION {
        return Err(WorkflowStoreError::InvalidTarget(
            "workflow JSON schema/version mismatch".to_owned(),
        ));
    }
    Ok(definition)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), WorkflowStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| WorkflowStoreError::InvalidTarget(path.display().to_string()))?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        nonce
    ));
    fs::write(&temp, bytes)?;
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_workflow::{AgentNode, EffectClass, Node, ValueExpr, WorkflowMeta};
    use serde_json::Value;
    use std::collections::BTreeMap;

    fn definition(name: &str) -> WorkflowDefinition {
        WorkflowDefinition {
            version: WORKFLOW_VERSION,
            meta: WorkflowMeta {
                id: None,
                name: name.to_owned(),
                description: Some("test".to_owned()),
                when_to_use: None,
                tags: Vec::new(),
            },
            inputs: BTreeMap::new(),
            body: vec![Node::Agent(AgentNode {
                node_id: "ask-1".to_owned(),
                name: "researcher".to_owned(),
                input: ValueExpr::literal(serde_json::json!("inspect")),
                config: Value::Null,
                output_type: None,
                effect: EffectClass::ReadOnly,
            })],
        }
    }

    #[test]
    fn global_and_project_definitions_are_separate() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(root.path());
        store
            .save(WorkflowScope::Global, None, &definition("shared"))
            .unwrap();
        store
            .save(
                WorkflowScope::Project,
                Some(project.path()),
                &definition("local"),
            )
            .unwrap();
        assert_eq!(
            store
                .list(WorkflowScope::Global, None)
                .unwrap()
                .workflows
                .len(),
            1
        );
        assert_eq!(
            store
                .list(WorkflowScope::Project, Some(project.path()))
                .unwrap()
                .workflows
                .len(),
            1
        );
    }

    #[test]
    fn move_does_not_overwrite_project_definition() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(root.path());
        store
            .save(WorkflowScope::Global, None, &definition("shared"))
            .unwrap();
        store
            .save(
                WorkflowScope::Project,
                Some(project.path()),
                &definition("shared"),
            )
            .unwrap();
        assert!(matches!(
            store.move_between_scopes(
                WorkflowScope::Global,
                None,
                WorkflowScope::Project,
                Some(project.path()),
                "shared",
            ),
            Err(WorkflowStoreError::TargetExists(_))
        ));
    }

    #[test]
    fn move_project_to_global_preserves_definition_bytes() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(root.path());
        store
            .save(
                WorkflowScope::Project,
                Some(project.path()),
                &definition("local"),
            )
            .unwrap();
        let source = store
            .path(WorkflowScope::Project, Some(project.path()), "local")
            .unwrap();
        let expected = fs::read(&source).unwrap();
        store
            .move_between_scopes(
                WorkflowScope::Project,
                Some(project.path()),
                WorkflowScope::Global,
                None,
                "local",
            )
            .unwrap();
        let target = store.path(WorkflowScope::Global, None, "local").unwrap();
        assert_eq!(fs::read(target).unwrap(), expected);
        assert!(!source.exists());
    }

    #[test]
    fn move_rejects_same_scope() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(root.path());
        assert!(matches!(
            store.move_between_scopes(
                WorkflowScope::Global,
                None,
                WorkflowScope::Global,
                None,
                "same",
            ),
            Err(WorkflowStoreError::InvalidTarget(_))
        ));
    }
}
