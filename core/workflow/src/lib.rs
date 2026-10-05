//! KeenCode 的纯 Rust JSON 动态工作流领域层。
//!
//! 定义在这里是声明式 JSON：节点只能使用固定的控制结构和 JSON Pointer，不能执行
//! 任意表达式。宿主通过 [`WorkflowDriver`] 实现 Agent、工具和产物能力，通过
//! [`WorkflowJournal`] 接入父会话的权威 Journal。该 crate 不依赖 Node、JavaScript
//! 引擎或桌面实现，因此可以被 Tauri、CLI 和 headless Host 复用。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio::time;
use tokio_util::sync::CancellationToken;

/// 工作流定义的唯一支持版本。
pub const WORKFLOW_DEFINITION_VERSION: u32 = 1;
/// 默认最大并发节点数。调用方只能继续收紧这个值。
pub const DEFAULT_MAX_CONCURRENCY: u16 = 4;
/// 默认静态和动态节点预算。
pub const DEFAULT_MAX_NODES: u32 = 10_000;
/// 默认控制结构嵌套深度。
pub const DEFAULT_MAX_DEPTH: u16 = 64;
/// 默认单次运行最长时间，避免错误定义长期占用执行器。
pub const DEFAULT_MAX_DURATION_MS: u64 = 5 * 60 * 1_000;
/// 默认所有节点输出累计字节上限。
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
/// 默认单个循环允许的最大迭代次数。
pub const DEFAULT_MAX_ITERATIONS: u32 = 10_000;

/// 领域异步结果的无额外依赖 Future 类型。
pub type WorkflowFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// JSON 输入的受限类型声明。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InputType {
    /// 接受所有 JSON 值。
    #[default]
    Any,
    /// 接受任意 JSON 值；这是 source workflow 协议的显式 `json` 类型。
    ///
    /// `Any` 保留为 Rust 侧默认类型，`Json` 用于保持定义 JSON 与 source
    /// 参数表的往返语义一致；两者都不把值限制为对象或数组。
    Json,
    /// JSON 字符串。
    String,
    /// JSON 数字，包括整数和小数。
    Number,
    /// JSON 整数。
    Integer,
    /// JSON 布尔值。
    Boolean,
    /// JSON 对象。
    Object,
    /// JSON 数组。
    Array,
    /// JSON null。
    Null,
}

impl InputType {
    fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Any | Self::Json => true,
            Self::String => value.is_string(),
            Self::Number => value.is_number(),
            Self::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
            Self::Boolean => value.is_boolean(),
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
            Self::Null => value.is_null(),
        }
    }
}

/// 工作流元数据。元数据不会被当作可执行代码。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowMeta {
    /// 稳定的业务标识；为空时由调用方用定义摘要标识。
    #[serde(default)]
    pub id: Option<String>,
    /// 用户可见名称。
    pub name: String,
    /// 用户可见说明。
    #[serde(default)]
    pub description: Option<String>,
    /// 面向用户的使用场景说明，用于目录检索和提示；不参与执行控制。
    ///
    /// 该字段属于定义本体，因此会随 canonical definition hash 冻结；仅使用
    /// `whenToUse` 这一 JSON 名称，旧的 snake_case 名称不再被兼容。
    #[serde(default, rename = "whenToUse")]
    pub when_to_use: Option<String>,
    /// 用户可见标签。
    #[serde(default)]
    pub tags: Vec<String>,
}

/// 一个工作流输入的声明。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputSpec {
    /// 运行时类型约束。
    #[serde(rename = "type", default)]
    pub value_type: InputType,
    /// 用户在启动参数表中看到的输入说明；不参与运行时求值。
    #[serde(default)]
    pub description: Option<String>,
    /// 是否必须由调用方显式提供或由默认值补全。
    #[serde(default)]
    pub required: bool,
    /// 缺失输入的固定 JSON 默认值。
    #[serde(default)]
    pub default: Option<Value>,
}

/// JSON Pointer 或固定 JSON 字面量。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValueExpr {
    /// 不读取运行时状态的固定值。
    Literal {
        /// 固定 JSON 值。
        value: Value,
    },
    /// 从受限作用域读取值。
    Ref {
        /// 只允许 `/inputs`、`/nodes` 和 `/iteration` 根作用域。
        pointer: String,
    },
}

impl ValueExpr {
    /// 创建一个固定 JSON 值表达式。
    pub fn literal(value: Value) -> Self {
        Self::Literal { value }
    }

    /// 创建一个 JSON Pointer 表达式。
    pub fn reference(pointer: impl Into<String>) -> Self {
        Self::Ref {
            pointer: pointer.into(),
        }
    }
}

fn default_effect_unknown() -> EffectClass {
    EffectClass::Unknown
}

fn default_effect_write() -> EffectClass {
    EffectClass::Write
}

/// 节点执行可能造成的副作用等级。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    /// 可以安全读取或重新计算。
    #[default]
    ReadOnly,
    /// 会写入工作区或其他持久资源，恢复时禁止自动重放。
    Write,
    /// 宿主无法证明副作用，恢复时禁止自动重放且并发上保守串行。
    Unknown,
}

impl EffectClass {
    fn requires_serial_gate(self) -> bool {
        matches!(self, Self::Write | Self::Unknown)
    }

    fn is_mutating(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    fn join(self, other: Self) -> Self {
        self.max(other)
    }
}

/// Agent 节点声明。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentNode {
    /// 稳定节点 ID。
    pub node_id: String,
    /// 宿主选择 Agent 的稳定名称。
    pub name: String,
    /// 传给 Agent 的 JSON 输入。
    pub input: ValueExpr,
    /// 宿主专用的静态配置；不会被本 crate 求值。
    #[serde(default)]
    pub config: Value,
    /// 结果类型标签。
    #[serde(default)]
    pub output_type: Option<String>,
    /// 副作用声明。
    #[serde(default = "default_effect_unknown")]
    pub effect: EffectClass,
}

/// Tool 节点声明。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolNode {
    /// 稳定节点 ID。
    pub node_id: String,
    /// 宿主工具注册表中的稳定名称。
    pub name: String,
    /// 传给工具的 JSON 输入。
    pub input: ValueExpr,
    /// 宿主专用的静态配置；不会被本 crate 求值。
    #[serde(default)]
    pub config: Value,
    /// 结果类型标签。
    #[serde(default)]
    pub output_type: Option<String>,
    /// 副作用声明。
    #[serde(default = "default_effect_unknown")]
    pub effect: EffectClass,
}

/// Artifact 节点声明。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactNode {
    /// 稳定节点 ID。
    pub node_id: String,
    /// 宿主 Artifact 操作的稳定名称。
    pub name: String,
    /// 传给 Artifact 操作的 JSON 输入。
    pub input: ValueExpr,
    /// 宿主专用的静态配置；不会被本 crate 求值。
    #[serde(default)]
    pub config: Value,
    /// 结果媒体或数据类型标签。
    #[serde(default)]
    pub output_type: Option<String>,
    /// 产物通常会写入持久存储，因此默认禁止恢复重放。
    #[serde(default = "default_effect_write")]
    pub effect: EffectClass,
}

/// 声明式工作流节点。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    /// 按定义顺序执行子节点。
    Sequence {
        /// 稳定节点 ID。
        node_id: String,
        /// 子节点。
        nodes: Vec<Node>,
    },
    /// 并发执行子节点，任一失败会取消兄弟节点。
    Parallel {
        /// 稳定节点 ID。
        node_id: String,
        /// 子节点。
        nodes: Vec<Node>,
        /// 可选的组内并发上限；全局执行上限仍由 [`WorkflowLimits`] 控制。
        #[serde(default)]
        concurrency: Option<u16>,
    },
    /// 固定条件分支。
    #[serde(rename = "if")]
    If {
        /// 稳定节点 ID。
        node_id: String,
        /// 固定操作符条件。
        condition: Condition,
        /// 条件为真时执行。
        then_body: Vec<Node>,
        /// 条件为假时执行。
        #[serde(default)]
        else_body: Vec<Node>,
    },
    /// 遍历一个 JSON 数组，实际次数受 `max_iterations` 约束。
    Foreach {
        /// 稳定节点 ID。
        node_id: String,
        /// 必须求值为数组的输入。
        items: ValueExpr,
        /// 每个元素执行的节点体。
        body: Vec<Node>,
        /// 强制的循环预算。
        max_iterations: u32,
    },
    /// 重复执行节点体，必须显式提供循环预算。
    Repeat {
        /// 稳定节点 ID。
        node_id: String,
        /// 每次重复执行的节点体。
        body: Vec<Node>,
        /// 强制的循环预算。
        max_iterations: u32,
    },
    /// 交给宿主 Agent 实现。
    Agent(AgentNode),
    /// 交给宿主工具实现。
    Tool(ToolNode),
    /// 交给宿主 Artifact 实现。
    Artifact(ArtifactNode),
}

impl Node {
    /// 返回稳定节点 ID。
    pub fn node_id(&self) -> &str {
        match self {
            Self::Sequence { node_id, .. }
            | Self::Parallel { node_id, .. }
            | Self::If { node_id, .. }
            | Self::Foreach { node_id, .. }
            | Self::Repeat { node_id, .. } => node_id,
            Self::Agent(node) => &node.node_id,
            Self::Tool(node) => &node.node_id,
            Self::Artifact(node) => &node.node_id,
        }
    }

    /// 返回供图和 UI 投影使用的稳定节点种类。
    pub fn kind(&self) -> NodeKind {
        match self {
            Self::Sequence { .. } => NodeKind::Sequence,
            Self::Parallel { .. } => NodeKind::Parallel,
            Self::If { .. } => NodeKind::If,
            Self::Foreach { .. } => NodeKind::Foreach,
            Self::Repeat { .. } => NodeKind::Repeat,
            Self::Agent(_) => NodeKind::Agent,
            Self::Tool(_) => NodeKind::Tool,
            Self::Artifact(_) => NodeKind::Artifact,
        }
    }
}

/// 固定条件操作符；不存在字符串求值或任意脚本。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    /// JSON 深相等。
    Eq {
        /// 左值。
        left: ValueExpr,
        /// 右值。
        right: ValueExpr,
    },
    /// JSON 不相等。
    Ne {
        /// 左值。
        left: ValueExpr,
        /// 右值。
        right: ValueExpr,
    },
    /// 数字小于。
    Lt {
        /// 左值。
        left: ValueExpr,
        /// 右值。
        right: ValueExpr,
    },
    /// 数字小于等于。
    Lte {
        /// 左值。
        left: ValueExpr,
        /// 右值。
        right: ValueExpr,
    },
    /// 数字大于。
    Gt {
        /// 左值。
        left: ValueExpr,
        /// 右值。
        right: ValueExpr,
    },
    /// 数字大于等于。
    Gte {
        /// 左值。
        left: ValueExpr,
        /// 右值。
        right: ValueExpr,
    },
    /// 引用目标存在；目标为 null 仍算存在。
    Exists {
        /// 待读取的值。
        value: ValueExpr,
    },
    /// 所有子条件为真。
    And {
        /// 子条件。
        all: Vec<Condition>,
    },
    /// 任一子条件为真。
    Or {
        /// 子条件。
        any: Vec<Condition>,
    },
    /// 子条件取反。
    Not {
        /// 被取反的子条件。
        condition: Box<Condition>,
    },
}

/// V1 JSON 工作流定义。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionV1 {
    /// 必须是 [`WORKFLOW_DEFINITION_VERSION`]。
    pub version: u32,
    /// 用户可见定义元数据。
    pub meta: WorkflowMeta,
    /// 输入声明，键顺序参与规范化摘要。
    #[serde(default)]
    pub inputs: BTreeMap<String, InputSpec>,
    /// 顶层节点体。
    pub body: Vec<Node>,
}

/// 工作流资源限制。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorkflowLimits {
    /// 全局同时执行的叶节点数，默认 4。
    pub max_concurrency: u16,
    /// 单次运行允许执行的节点调用总数。
    pub max_nodes: u32,
    /// 定义控制结构最大嵌套深度。
    pub max_depth: u16,
    /// 单次运行最长时间（毫秒）。
    pub max_duration_ms: u64,
    /// 节点输出累计 UTF-8 JSON 字节上限。
    pub max_output_bytes: u64,
    /// 每个 foreach/repeat 的最大迭代数。
    pub max_iterations: u32,
}

impl Default for WorkflowLimits {
    fn default() -> Self {
        Self {
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            max_nodes: DEFAULT_MAX_NODES,
            max_depth: DEFAULT_MAX_DEPTH,
            max_duration_ms: DEFAULT_MAX_DURATION_MS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_iterations: DEFAULT_MAX_ITERATIONS,
        }
    }
}

impl WorkflowLimits {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.max_concurrency == 0
            || self.max_nodes == 0
            || self.max_depth == 0
            || self.max_duration_ms == 0
            || self.max_output_bytes == 0
            || self.max_iterations == 0
        {
            return Err(ValidationError::InvalidLimits);
        }
        Ok(())
    }

    /// 判断 `candidate` 是否只收紧了当前限制。
    pub fn allows(&self, candidate: &Self) -> bool {
        candidate.max_concurrency <= self.max_concurrency
            && candidate.max_nodes <= self.max_nodes
            && candidate.max_depth <= self.max_depth
            && candidate.max_duration_ms <= self.max_duration_ms
            && candidate.max_output_bytes <= self.max_output_bytes
            && candidate.max_iterations <= self.max_iterations
    }
}

/// 编译后的静态图节点种类。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// 顺序控制节点。
    Sequence,
    /// 并行控制节点。
    Parallel,
    /// 条件控制节点。
    If,
    /// 遍历控制节点。
    Foreach,
    /// 重复控制节点。
    Repeat,
    /// Agent 叶节点。
    Agent,
    /// Tool 叶节点。
    Tool,
    /// Artifact 叶节点。
    Artifact,
}

/// 图中的已知边关系，便于 UI 映射到阶段和时间线。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    /// 顺序或控制节点包含关系。
    Contains,
    /// 条件真分支。
    Then,
    /// 条件假分支。
    Else,
    /// 循环体。
    Body,
}

/// 编译后稳定图节点。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphNode {
    /// 稳定节点 ID。
    pub node_id: String,
    /// 已知节点种类。
    pub kind: NodeKind,
    /// 从根开始的静态深度。
    pub depth: u16,
}

/// 编译后稳定图边。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphEdge {
    /// 父或控制节点。
    pub from: String,
    /// 子节点。
    pub to: String,
    /// 已知边关系。
    pub relation: EdgeRelation,
    /// 同一父节点下的定义顺序。
    pub ordinal: u32,
}

/// 编译后的图。它只包含稳定领域类型，宿主负责映射到严格 UI schema。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowGraph {
    /// 静态图节点。
    pub nodes: Vec<GraphNode>,
    /// 静态图边。
    pub edges: Vec<GraphEdge>,
    /// 图的最大静态深度。
    pub max_depth: u16,
    /// 不展开循环时的叶节点计数估计。
    pub leaf_count: u32,
}

/// 编译产物，冻结定义摘要和限制。
#[derive(Clone, Debug)]
pub struct CompiledWorkflow {
    definition: WorkflowDefinitionV1,
    definition_hash: String,
    graph: WorkflowGraph,
    limits: WorkflowLimits,
    /// 按真实桌面 Driver 的保存点计算的最坏父 Artifact 槽位数。
    /// Agent 结果保存在 actor Session，不占父 ArtifactStore；Tool 完成结果和
    /// Artifact 叶节点各占一个槽位。工具文件变更的 before/after 证据是动态保存点，
    /// 不能被静态编译结果冒充已预留，实际提交仍由 ArtifactStore fail-closed。
    artifact_slots: u32,
}

impl CompiledWorkflow {
    /// 返回不可变定义。
    pub fn definition(&self) -> &WorkflowDefinitionV1 {
        &self.definition
    }

    /// 返回 `sha256:<hex>` 定义摘要。
    pub fn definition_hash(&self) -> &str {
        &self.definition_hash
    }

    /// 返回稳定图。
    pub fn graph(&self) -> &WorkflowGraph {
        &self.graph
    }

    /// 返回编译时限制。
    pub fn limits(&self) -> WorkflowLimits {
        self.limits
    }

    /// 返回一次全新执行最多需要的父 Artifact 槽位数。
    pub fn artifact_slot_count(&self) -> u32 {
        self.artifact_slots
    }

    /// 扣除恢复时已确认完成且可复用的 Tool/Artifact 节点后所需的新槽位数。
    ///
    /// `completed` 来自权威 RecoverySnapshot；地址包含完整循环 invocation，重复
    /// 地址只计一次。取消或失败节点没有完成事实，因此不会被扣除，已产生的取消
    /// Artifact 仍然由 ArtifactStore 的当前容量快照占用。
    pub fn artifact_slots_after_completed(&self, completed: &[NodeAddress]) -> u32 {
        let artifact_node_ids = self
            .graph
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Tool | NodeKind::Artifact))
            .map(|node| node.node_id.as_str())
            .collect::<HashSet<_>>();
        let reusable = completed
            .iter()
            .filter(|address| artifact_node_ids.contains(address.node_id.as_str()))
            .map(|address| (address.node_id.as_str(), address.invocation.as_slice()))
            .collect::<HashSet<_>>()
            .len();
        self.artifact_slots
            .saturating_sub(u32::try_from(reusable).unwrap_or(u32::MAX))
    }
}

/// 编译定义并使用默认预算。
pub fn compile(definition: WorkflowDefinitionV1) -> Result<CompiledWorkflow, ValidationError> {
    compile_with_limits(definition, WorkflowLimits::default())
}

/// 编译定义并使用给定的静态预算。
pub fn compile_with_limits(
    definition: WorkflowDefinitionV1,
    limits: WorkflowLimits,
) -> Result<CompiledWorkflow, ValidationError> {
    limits.validate()?;
    if definition.version != WORKFLOW_DEFINITION_VERSION {
        return Err(ValidationError::UnsupportedVersion(definition.version));
    }
    if definition.meta.name.trim().is_empty() {
        return Err(ValidationError::EmptyMetadataName);
    }
    for (name, spec) in &definition.inputs {
        validate_identifier(name).map_err(|_| ValidationError::InvalidInputName(name.clone()))?;
        if let Some(default) = &spec.default
            && !spec.value_type.accepts(default)
        {
            return Err(ValidationError::InputDefaultType {
                name: name.clone(),
                expected: spec.value_type.clone(),
            });
        }
        if spec.required && spec.default.is_some() {
            // required + default 是明确的“缺失时补全”语义，不是冲突配置。
        }
    }
    let mut ids = BTreeSet::new();
    let mut all_nodes = BTreeMap::new();
    collect_ids(&definition.body, &mut ids, &mut all_nodes)?;
    validate_references(&definition.body, &definition.inputs, &ids)?;
    let mut graph = WorkflowGraph {
        nodes: Vec::new(),
        edges: Vec::new(),
        max_depth: 0,
        leaf_count: 0,
    };
    collect_graph(
        &definition.body,
        0,
        None,
        EdgeRelation::Contains,
        &mut graph,
    );
    graph.max_depth = graph.nodes.iter().map(|node| node.depth).max().unwrap_or(0);
    let estimated = estimate_nodes(&definition.body, &limits)?;
    graph.leaf_count = estimate_leaves(&definition.body, &limits)?;
    let artifact_slots = estimate_artifact_slots(&definition.body, &limits)?;
    if graph.max_depth > limits.max_depth {
        return Err(ValidationError::DepthExceeded {
            depth: graph.max_depth,
            max: limits.max_depth,
        });
    }
    if estimated > limits.max_nodes {
        return Err(ValidationError::NodeBudgetExceeded {
            count: estimated,
            max: limits.max_nodes,
        });
    }
    let definition_hash = hash_json(&definition).map_err(ValidationError::Serialization)?;
    Ok(CompiledWorkflow {
        definition,
        definition_hash,
        graph,
        limits,
        artifact_slots,
    })
}

fn validate_identifier(value: &str) -> Result<(), ()> {
    if value.is_empty()
        || value.len() > 128
        || value.contains('/')
        || value.contains('~')
        || value.chars().any(char::is_whitespace)
    {
        return Err(());
    }
    Ok(())
}

fn collect_ids(
    nodes: &[Node],
    ids: &mut BTreeSet<String>,
    all_nodes: &mut BTreeMap<String, NodeKind>,
) -> Result<(), ValidationError> {
    for node in nodes {
        let id = node.node_id();
        validate_identifier(id).map_err(|_| ValidationError::InvalidNodeId(id.to_owned()))?;
        if !ids.insert(id.to_owned()) {
            return Err(ValidationError::DuplicateNodeId(id.to_owned()));
        }
        all_nodes.insert(id.to_owned(), node.kind());
        match node {
            Node::Sequence { nodes, .. } | Node::Parallel { nodes, .. } => {
                collect_ids(nodes, ids, all_nodes)?;
            }
            Node::If {
                then_body,
                else_body,
                ..
            } => {
                collect_ids(then_body, ids, all_nodes)?;
                collect_ids(else_body, ids, all_nodes)?;
            }
            Node::Foreach { body, .. } | Node::Repeat { body, .. } => {
                collect_ids(body, ids, all_nodes)?;
            }
            Node::Agent(agent) => validate_leaf(
                agent.node_id.as_str(),
                &agent.name,
                agent.output_type.as_deref(),
            )?,
            Node::Tool(tool) => validate_leaf(
                tool.node_id.as_str(),
                &tool.name,
                tool.output_type.as_deref(),
            )?,
            Node::Artifact(artifact) => validate_leaf(
                artifact.node_id.as_str(),
                &artifact.name,
                artifact.output_type.as_deref(),
            )?,
        }
    }
    Ok(())
}

fn validate_leaf(
    _node_id: &str,
    name: &str,
    output_type: Option<&str>,
) -> Result<(), ValidationError> {
    if name.trim().is_empty() {
        return Err(ValidationError::EmptyOperationName);
    }
    if output_type.is_some_and(|value| value.trim().is_empty()) {
        return Err(ValidationError::EmptyOutputType);
    }
    Ok(())
}

fn collect_expr_refs(expr: &ValueExpr, refs: &mut Vec<String>) {
    if let ValueExpr::Ref { pointer } = expr {
        refs.push(pointer.clone());
    }
}

fn collect_condition_refs(condition: &Condition, refs: &mut Vec<String>) {
    match condition {
        Condition::Eq { left, right }
        | Condition::Ne { left, right }
        | Condition::Lt { left, right }
        | Condition::Lte { left, right }
        | Condition::Gt { left, right }
        | Condition::Gte { left, right } => {
            collect_expr_refs(left, refs);
            collect_expr_refs(right, refs);
        }
        Condition::Exists { value } => collect_expr_refs(value, refs),
        Condition::And { all } => all
            .iter()
            .for_each(|item| collect_condition_refs(item, refs)),
        Condition::Or { any } => any
            .iter()
            .for_each(|item| collect_condition_refs(item, refs)),
        Condition::Not { condition } => collect_condition_refs(condition, refs),
    }
}

fn validate_references(
    nodes: &[Node],
    inputs: &BTreeMap<String, InputSpec>,
    ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>, ValidationError> {
    validate_references_with_scope(nodes, inputs, ids, &BTreeSet::new())
}

/// 按控制结构计算“在当前节点执行前已经确定完成”的节点集合。
///
/// 这个集合只用于编译期安全检查，不改变运行时 `/nodes` 的数据形状。分支体
/// 和循环体的内部叶节点不会泄漏到控制结构外；并行兄弟各自只看到并行段之前
/// 的集合，从而拒绝任何依赖调度顺序的引用。
fn validate_references_with_scope(
    nodes: &[Node],
    inputs: &BTreeMap<String, InputSpec>,
    ids: &BTreeSet<String>,
    available: &BTreeSet<String>,
) -> Result<BTreeSet<String>, ValidationError> {
    let mut completed = available.clone();
    for node in nodes {
        let mut refs = Vec::new();
        let visible_after = match node {
            Node::Sequence { nodes, .. } => {
                validate_references_with_scope(nodes, inputs, ids, &completed)?
            }
            Node::Parallel { nodes, .. } => {
                let mut parallel_completed = completed.clone();
                // 每个并行分支只能读取并行段前的快照，禁止读取兄弟输出。
                for child in nodes {
                    let child_after = validate_references_with_scope(
                        std::slice::from_ref(child),
                        inputs,
                        ids,
                        &completed,
                    )?;
                    parallel_completed.extend(child_after);
                }
                parallel_completed
            }
            Node::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                collect_condition_refs(condition, &mut refs);
                validate_references_with_scope(then_body, inputs, ids, &completed)?;
                validate_references_with_scope(else_body, inputs, ids, &completed)?;
                // 两个分支的叶节点并非都一定执行；只暴露 If 自身的稳定控制输出。
                let mut after = completed.clone();
                after.insert(node.node_id().to_owned());
                after
            }
            Node::Foreach { items, body, .. } => {
                collect_expr_refs(items, &mut refs);
                validate_references_with_scope(body, inputs, ids, &completed)?;
                // foreach 允许空数组，循环体叶节点不能作为外部确定引用。
                let mut after = completed.clone();
                after.insert(node.node_id().to_owned());
                after
            }
            Node::Repeat { body, .. } => {
                validate_references_with_scope(body, inputs, ids, &completed)?;
                let mut after = completed.clone();
                after.insert(node.node_id().to_owned());
                after
            }
            Node::Agent(agent) => {
                collect_expr_refs(&agent.input, &mut refs);
                let mut after = completed.clone();
                after.insert(agent.node_id.clone());
                after
            }
            Node::Tool(tool) => {
                collect_expr_refs(&tool.input, &mut refs);
                let mut after = completed.clone();
                after.insert(tool.node_id.clone());
                after
            }
            Node::Artifact(artifact) => {
                collect_expr_refs(&artifact.input, &mut refs);
                let mut after = completed.clone();
                after.insert(artifact.node_id.clone());
                after
            }
        };
        for pointer in refs {
            validate_pointer(&pointer, inputs, ids, &completed)?;
        }
        match node {
            Node::Foreach { max_iterations, .. } | Node::Repeat { max_iterations, .. }
                if *max_iterations == 0 =>
            {
                return Err(ValidationError::MissingIterationBudget(
                    node.node_id().to_owned(),
                ));
            }
            Node::Parallel { concurrency, .. } if concurrency == &Some(0) => {
                return Err(ValidationError::InvalidParallelConcurrency(
                    node.node_id().to_owned(),
                ));
            }
            Node::If { condition, .. } => validate_condition_shape(condition)?,
            _ => {}
        }
        completed = visible_after;
    }
    Ok(completed)
}

fn validate_condition_shape(condition: &Condition) -> Result<(), ValidationError> {
    match condition {
        Condition::And { all } if all.is_empty() => {
            Err(ValidationError::EmptyCondition("and".into()))
        }
        Condition::Or { any } if any.is_empty() => {
            Err(ValidationError::EmptyCondition("or".into()))
        }
        Condition::And { all } => all.iter().try_for_each(validate_condition_shape),
        Condition::Or { any } => any.iter().try_for_each(validate_condition_shape),
        Condition::Not { condition } => validate_condition_shape(condition),
        _ => Ok(()),
    }
}

fn validate_pointer(
    pointer: &str,
    inputs: &BTreeMap<String, InputSpec>,
    ids: &BTreeSet<String>,
    available: &BTreeSet<String>,
) -> Result<(), ValidationError> {
    let parts = decode_pointer(pointer)?;
    match parts.first().map(String::as_str) {
        Some("inputs") => {
            let Some(name) = parts.get(1) else {
                return Err(ValidationError::InvalidPointer(pointer.to_owned()));
            };
            if !inputs.contains_key(name) {
                return Err(ValidationError::UnknownInputReference(name.clone()));
            }
        }
        Some("nodes") => {
            let Some(node_id) = parts.get(1) else {
                return Err(ValidationError::InvalidPointer(pointer.to_owned()));
            };
            if !ids.contains(node_id) {
                return Err(ValidationError::UnknownNodeReference(node_id.clone()));
            }
            if !available.contains(node_id) {
                return Err(ValidationError::NodeReferenceUnavailable(node_id.clone()));
            }
        }
        Some("iteration") => {}
        _ => return Err(ValidationError::DisallowedPointer(pointer.to_owned())),
    }
    Ok(())
}

fn decode_pointer(pointer: &str) -> Result<Vec<String>, ValidationError> {
    if pointer.is_empty() || !pointer.starts_with('/') {
        return Err(ValidationError::InvalidPointer(pointer.to_owned()));
    }
    pointer
        .split('/')
        .skip(1)
        .map(|part| {
            let mut decoded = String::with_capacity(part.len());
            let mut chars = part.chars();
            while let Some(ch) = chars.next() {
                if ch != '~' {
                    decoded.push(ch);
                    continue;
                }
                match chars.next() {
                    Some('0') => decoded.push('~'),
                    Some('1') => decoded.push('/'),
                    _ => return Err(ValidationError::InvalidPointer(pointer.to_owned())),
                }
            }
            Ok(decoded)
        })
        .collect()
}

fn estimate_nodes(nodes: &[Node], limits: &WorkflowLimits) -> Result<u32, ValidationError> {
    let mut total = 0u64;
    for node in nodes {
        let self_count = 1u64;
        let child_count = match node {
            Node::Sequence { nodes, .. } | Node::Parallel { nodes, .. } => {
                u64::from(estimate_nodes(nodes, limits)?)
            }
            Node::If {
                then_body,
                else_body,
                ..
            } => u64::from(
                estimate_nodes(then_body, limits)?.max(estimate_nodes(else_body, limits)?),
            ),
            Node::Foreach {
                body,
                max_iterations,
                ..
            }
            | Node::Repeat {
                body,
                max_iterations,
                ..
            } => {
                if *max_iterations == 0 || *max_iterations > limits.max_iterations {
                    return Err(ValidationError::IterationBudgetExceeded {
                        node_id: node.node_id().to_owned(),
                        count: *max_iterations,
                        max: limits.max_iterations,
                    });
                }
                u64::from(estimate_nodes(body, limits)?).saturating_mul(u64::from(*max_iterations))
            }
            Node::Agent(_) | Node::Tool(_) | Node::Artifact(_) => 0,
        };
        total = total.saturating_add(self_count).saturating_add(child_count);
        if total > u64::from(limits.max_nodes) {
            return Err(ValidationError::NodeBudgetExceeded {
                count: total.min(u64::from(u32::MAX)) as u32,
                max: limits.max_nodes,
            });
        }
    }
    u32::try_from(total).map_err(|_| ValidationError::NodeBudgetExceeded {
        count: u32::MAX,
        max: limits.max_nodes,
    })
}

fn estimate_leaves(nodes: &[Node], limits: &WorkflowLimits) -> Result<u32, ValidationError> {
    let mut total = 0u64;
    for node in nodes {
        let count = match node {
            Node::Sequence { nodes, .. } | Node::Parallel { nodes, .. } => {
                u64::from(estimate_leaves(nodes, limits)?)
            }
            Node::If {
                then_body,
                else_body,
                ..
            } => u64::from(
                estimate_leaves(then_body, limits)?.max(estimate_leaves(else_body, limits)?),
            ),
            Node::Foreach {
                body,
                max_iterations,
                ..
            }
            | Node::Repeat {
                body,
                max_iterations,
                ..
            } => {
                u64::from(estimate_leaves(body, limits)?).saturating_mul(u64::from(*max_iterations))
            }
            Node::Agent(_) | Node::Tool(_) | Node::Artifact(_) => 1,
        };
        total = total.saturating_add(count);
        if total > u64::from(limits.max_nodes) {
            return Err(ValidationError::NodeBudgetExceeded {
                count: total.min(u64::from(u32::MAX)) as u32,
                max: limits.max_nodes,
            });
        }
    }
    u32::try_from(total).map_err(|_| ValidationError::NodeBudgetExceeded {
        count: u32::MAX,
        max: limits.max_nodes,
    })
}

/// 计算父 ArtifactStore 的静态保存点上界。
///
/// 这是宿主 Driver 当前已经兑现的保存契约：Tool 结算写一份结果、Artifact
/// 叶节点写一份内容，Agent 结果写入独立 actor Session。文件工具的 before/after
/// 证据依赖运行时实际变更数量，因此不在这里伪造固定倍数，仍由实际写入门禁拒绝。
fn estimate_artifact_slots(
    nodes: &[Node],
    limits: &WorkflowLimits,
) -> Result<u32, ValidationError> {
    let mut total = 0u64;
    for node in nodes {
        let count = match node {
            Node::Sequence { nodes, .. } | Node::Parallel { nodes, .. } => {
                u64::from(estimate_artifact_slots(nodes, limits)?)
            }
            Node::If {
                then_body,
                else_body,
                ..
            } => u64::from(
                estimate_artifact_slots(then_body, limits)?
                    .max(estimate_artifact_slots(else_body, limits)?),
            ),
            Node::Foreach {
                body,
                max_iterations,
                ..
            }
            | Node::Repeat {
                body,
                max_iterations,
                ..
            } => u64::from(estimate_artifact_slots(body, limits)?)
                .saturating_mul(u64::from(*max_iterations)),
            Node::Tool(_) | Node::Artifact(_) => 1,
            Node::Agent(_) => 0,
        };
        total = total.saturating_add(count);
        if total > u64::from(limits.max_nodes) {
            return Err(ValidationError::NodeBudgetExceeded {
                count: total.min(u64::from(u32::MAX)) as u32,
                max: limits.max_nodes,
            });
        }
    }
    u32::try_from(total).map_err(|_| ValidationError::NodeBudgetExceeded {
        count: u32::MAX,
        max: limits.max_nodes,
    })
}

fn collect_graph(
    nodes: &[Node],
    depth: u16,
    parent: Option<&str>,
    relation: EdgeRelation,
    graph: &mut WorkflowGraph,
) {
    for (ordinal, node) in nodes.iter().enumerate() {
        let id = node.node_id().to_owned();
        graph.nodes.push(GraphNode {
            node_id: id.clone(),
            kind: node.kind(),
            depth,
        });
        if let Some(parent) = parent {
            graph.edges.push(GraphEdge {
                from: parent.to_owned(),
                to: id.clone(),
                relation,
                ordinal: ordinal as u32,
            });
        }
        let child_depth = depth.saturating_add(1);
        match node {
            Node::Sequence { nodes, .. } | Node::Parallel { nodes, .. } => {
                collect_graph(nodes, child_depth, Some(&id), EdgeRelation::Contains, graph)
            }
            Node::If {
                then_body,
                else_body,
                ..
            } => {
                collect_graph(then_body, child_depth, Some(&id), EdgeRelation::Then, graph);
                collect_graph(else_body, child_depth, Some(&id), EdgeRelation::Else, graph);
            }
            Node::Foreach { body, .. } | Node::Repeat { body, .. } => {
                collect_graph(body, child_depth, Some(&id), EdgeRelation::Body, graph)
            }
            Node::Agent(_) | Node::Tool(_) | Node::Artifact(_) => {}
        }
    }
}

/// 运行时已解析且经过副作用分类的节点输出。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TypedOutput {
    /// JSON 输出。
    pub value: Value,
    /// 宿主可消费的稳定类型标签。
    pub output_type: String,
    /// 输出大小（UTF-8 JSON 字节），由执行器复核。
    pub bytes: u64,
    /// Driver 对实际副作用的分类。
    pub effect: EffectClass,
}

impl TypedOutput {
    /// 创建输出并按紧凑 JSON 计算字节数。
    pub fn new(value: Value, output_type: impl Into<String>, effect: EffectClass) -> Self {
        let bytes = serde_json::to_vec(&value)
            .map(|data| data.len() as u64)
            .unwrap_or(u64::MAX);
        Self {
            value,
            output_type: output_type.into(),
            bytes,
            effect,
        }
    }
}

/// Driver 的通用叶节点请求。
#[derive(Clone, Debug)]
pub struct DriverRequest {
    /// 由编译后的节点种类确定的叶节点类别；宿主不得从 `name` 字符串猜测类别。
    pub leaf_kind: DriverLeafKind,
    /// 稳定节点 ID。
    pub node_id: String,
    /// 宿主注册名称。
    pub name: String,
    /// 已解析输入。
    pub input: Value,
    /// 静态节点配置。
    pub config: Value,
    /// 定义要求的输出类型。
    pub output_type: Option<String>,
    /// 定义声明的副作用。
    pub effect: EffectClass,
}

/// 传给叶节点 Driver 的稳定类别。
///
/// 这是执行图的类型信息，不是用户可编辑的字符串字段；恢复时必须按此类别选择
/// Agent、Tool 或 Artifact 的专用入口，避免把只读 Agent 误当作可安全重放的 Tool。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverLeafKind {
    /// 由宿主 AgentRuntime 执行的模型节点。
    Agent,
    /// 由宿主受控 Tool registry 执行的工具节点。
    Tool,
    /// 由宿主 ArtifactStore 执行的产物节点。
    Artifact,
}

/// Driver 调用时可读的工作流作用域。
#[derive(Clone, Debug)]
pub struct NodeContext {
    /// 本次运行 ID。
    pub run_id: String,
    /// 当前节点稳定 ID。
    pub node_id: String,
    /// 当前节点完整执行地址；嵌套循环索引不可从 `iteration` 推断，Driver 必须使用此身份。
    pub address: NodeAddress,
    /// 已补全并校验的输入对象。
    pub inputs: Arc<Value>,
    /// 当前已完成节点的最新输出。
    pub nodes: Arc<BTreeMap<String, Value>>,
    /// 当前循环作用域；不在循环内时为 null。
    pub iteration: Value,
    /// 取消令牌；Driver 必须把它传给底层模型/工具/进程边界。
    pub cancellation_token: CancellationToken,
}

/// Agent、Tool 和 Artifact 的宿主能力实现。
pub trait WorkflowDriver: Send + Sync {
    /// 执行 Agent 叶节点。
    fn execute_agent<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>>;

    /// 执行 Tool 叶节点。
    fn execute_tool<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>>;

    /// 执行 Artifact 叶节点。
    fn execute_artifact<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>>;

    /// 对已启动但未结算的只读节点执行宿主明确提供的恢复逻辑。
    ///
    /// 默认拒绝恢复，防止“只读”标签被误当作可以隐式重放的许可。
    fn recover_read_only<'a>(
        &'a self,
        _request: DriverRequest,
        _context: NodeContext,
    ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        Box::pin(async { Err(DriverError::recovery_not_supported()) })
    }

    /// 恢复已启动但未结算的只读 Agent；默认兼容旧的统一恢复入口。
    fn recover_agent<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        self.recover_read_only(request, context)
    }

    /// 恢复已启动但未结算的只读 Tool；默认兼容旧的统一恢复入口。
    fn recover_tool<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        self.recover_read_only(request, context)
    }

    /// Artifact 即使声明只读也不能由默认恢复路径写入或重建；宿主必须显式实现安全策略。
    fn recover_artifact<'a>(
        &'a self,
        request: DriverRequest,
        context: NodeContext,
    ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
        self.recover_read_only(request, context)
    }
}

/// Journal 持久化错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalError {
    /// 稳定错误代码。
    pub code: String,
    /// 不含凭据或用户正文的短错误描述。
    pub message: String,
}

impl JournalError {
    /// 创建 Journal 错误。
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl Display for JournalError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for JournalError {}

/// 节点调用的稳定地址。循环迭代通过 invocation 区分，不改变 nodeId。
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct NodeAddress {
    /// 稳定定义节点 ID。
    pub node_id: String,
    /// 从外层到内层循环的迭代索引。
    #[serde(default)]
    pub invocation: Vec<u32>,
}

impl NodeAddress {
    fn execution_id(&self) -> String {
        if self.invocation.is_empty() {
            return self.node_id.clone();
        }
        let suffix = self
            .invocation
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".");
        format!("{}@{}", self.node_id, suffix)
    }
}

/// Journal 记录；执行器保证每个启动前先 append，通知在 append 成功后才发送。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalEvent {
    /// 节点开始执行。
    NodeStarted(NodeStarted),
    /// 节点执行成功或失败并形成结算。
    NodeSettled(NodeSettled),
    /// 重用已持久化完成节点的事实。
    NodeReused(NodeReused),
    /// 工作流运行终态。
    RunFinished(RunFinished),
}

/// 节点开始记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeStarted {
    /// 运行 ID。
    pub run_id: String,
    /// 节点地址。
    pub address: NodeAddress,
    /// 定义声明的副作用。
    pub effect: EffectClass,
}

/// 节点结算记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeSettled {
    /// 运行 ID。
    pub run_id: String,
    /// 节点地址。
    pub address: NodeAddress,
    /// 实际终态。
    pub status: NodeStatus,
    /// 成功输出；失败时为空。
    #[serde(default)]
    pub output: Option<TypedOutput>,
    /// 安全的稳定错误摘要。
    #[serde(default)]
    pub error: Option<PublicError>,
}

/// 节点缓存重用记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeReused {
    /// 运行 ID。
    pub run_id: String,
    /// 节点地址。
    pub address: NodeAddress,
    /// 被重用的输出。
    pub output: TypedOutput,
}

/// 运行终态记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunFinished {
    /// 运行 ID。
    pub run_id: String,
    /// 工作流终态。
    pub status: RunStatus,
    /// 定义摘要。
    pub definition_hash: String,
    /// 输入摘要。
    pub input_hash: String,
    /// 失败或不确定终态的受限错误摘要；成功终态省略该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PublicError>,
}

/// Journal 中可恢复的已完成节点。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompletedNode {
    /// 节点地址。
    pub address: NodeAddress,
    /// 已确认输出。
    pub output: TypedOutput,
}

/// Journal 中开始但没有结算的节点。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StartedNode {
    /// 节点地址。
    pub address: NodeAddress,
    /// 启动时声明的副作用。
    pub effect: EffectClass,
}

/// 恢复所需的 Journal 快照。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RecoverySnapshot {
    /// 运行 ID。
    pub run_id: String,
    /// 冻结的定义摘要。
    pub definition_hash: String,
    /// 冻结的输入摘要。
    pub input_hash: String,
    /// 已完成节点缓存。
    #[serde(default)]
    pub completed: Vec<CompletedNode>,
    /// 已启动但未结算节点。
    #[serde(default)]
    pub started: Vec<StartedNode>,
}

/// 持久化 Journal 和通知的端口。
pub trait WorkflowJournal: Send + Sync {
    /// 原子追加权威记录；失败时执行器不得调用 Driver。
    fn append<'a>(&'a self, event: JournalEvent) -> WorkflowFuture<'a, Result<(), JournalError>>;

    /// 将已持久化记录通知给投影层；调用一定发生在 [`Self::append`] 成功后。
    fn notify<'a>(&'a self, event: JournalEvent) -> WorkflowFuture<'a, Result<(), JournalError>>;

    /// 读取指定运行的恢复快照。
    fn load_recovery<'a>(
        &'a self,
        run_id: &'a str,
    ) -> WorkflowFuture<'a, Result<Option<RecoverySnapshot>, JournalError>>;
}

/// 不持久化也不通知的默认 Journal，适合只读工具或上层自带记录的测试。
pub struct NoopJournal;

impl WorkflowJournal for NoopJournal {
    fn append<'a>(&'a self, _event: JournalEvent) -> WorkflowFuture<'a, Result<(), JournalError>> {
        Box::pin(async { Ok(()) })
    }

    fn notify<'a>(&'a self, _event: JournalEvent) -> WorkflowFuture<'a, Result<(), JournalError>> {
        Box::pin(async { Ok(()) })
    }

    fn load_recovery<'a>(
        &'a self,
        _run_id: &'a str,
    ) -> WorkflowFuture<'a, Result<Option<RecoverySnapshot>, JournalError>> {
        Box::pin(async { Ok(None) })
    }
}

/// Driver 执行错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriverError {
    /// 稳定错误代码。
    pub code: String,
    /// 不含凭据和完整用户数据的短错误描述。
    pub message: String,
}

impl DriverError {
    /// 创建 Driver 错误。
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn recovery_not_supported() -> Self {
        Self::new(
            "read_only_recovery_not_supported",
            "宿主未明确实现只读节点恢复",
        )
    }
}

impl Display for DriverError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DriverError {}

/// 可公开给 UI 的错误摘要。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PublicError {
    /// 稳定错误代码。
    pub code: String,
    /// 有界错误描述。
    pub message: String,
}

impl From<&WorkflowError> for PublicError {
    fn from(error: &WorkflowError) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string().chars().take(512).collect(),
        }
    }
}

/// 节点终态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// 已成功结算。
    Succeeded,
    /// Driver 返回失败。
    Failed,
    /// 被取消且未形成成功输出。
    Cancelled,
    /// Journal 结算失败，结果不确定。
    Indeterminate,
}

/// 工作流运行终态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// 全部节点成功。
    Succeeded,
    /// 至少一个节点失败。
    Failed,
    /// 调用方或预算取消。
    Cancelled,
    /// 存在不可安全重放的未结算副作用。
    Indeterminate,
}

/// 运行中某次节点调用的结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeResult {
    /// 节点地址。
    pub address: NodeAddress,
    /// 节点种类。
    pub kind: NodeKind,
    /// 节点终态。
    pub status: NodeStatus,
    /// 成功输出。
    #[serde(default)]
    pub output: Option<TypedOutput>,
    /// 失败摘要。
    #[serde(default)]
    pub error: Option<PublicError>,
}

/// 宿主可映射到 ZCode v4 的固定 Artifact 视图。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowArtifactView {
    /// 稳定节点地址。
    pub address: NodeAddress,
    /// Artifact 类型。
    pub output_type: String,
    /// Artifact 的 JSON 值或引用。
    pub value: Value,
    /// 字节数。
    pub bytes: u64,
}

/// 宿主可映射到 ZCode v4 的固定阶段视图；不透传本 crate 新增节点种类。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowPhaseView {
    /// 静态图阶段 ID。
    pub phase_id: String,
    /// 用户可见标题。
    pub title: String,
    /// 阶段节点 ID。
    pub node_ids: Vec<String>,
    /// 阶段状态。
    pub status: RunStatus,
}

/// 一次工作流运行的结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExecutionResult {
    /// 运行 ID。
    pub run_id: String,
    /// 定义摘要。
    pub definition_hash: String,
    /// 输入摘要。
    pub input_hash: String,
    /// 运行终态。
    pub status: RunStatus,
    /// 每个稳定节点 ID 的最新输出。
    pub outputs: BTreeMap<String, TypedOutput>,
    /// 所有动态调用的结果。
    pub node_results: Vec<NodeResult>,
    /// 产物节点的固定视图。
    pub artifacts: Vec<WorkflowArtifactView>,
    /// UI 可用的稳定图。
    pub graph: WorkflowGraph,
    /// 运行消耗的输出字节。
    pub output_bytes: u64,
}

/// 启动一次工作流运行的请求。
#[derive(Clone, Debug)]
pub struct ExecutionRequest {
    /// 调用方稳定运行 ID。
    pub run_id: String,
    /// 顶层 JSON 输入对象。
    pub inputs: Value,
    /// 调用方可收紧的限制与取消令牌。
    pub options: ExecutionOptions,
    /// 可选恢复快照；没有快照时视为全新运行。
    pub recovery: Option<RecoverySnapshot>,
}

/// 执行选项。
#[derive(Clone, Debug)]
pub struct ExecutionOptions {
    /// 不得超过编译时限制。
    pub limits: WorkflowLimits,
    /// 调用方取消令牌。
    pub cancellation_token: CancellationToken,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            limits: WorkflowLimits::default(),
            cancellation_token: CancellationToken::new(),
        }
    }
}

/// 工作流执行错误。
#[derive(Clone, Debug)]
pub enum WorkflowError {
    /// 定义验证失败。
    Validation(ValidationError),
    /// 输入对象不合法。
    InvalidInputs(String),
    /// 定义摘要不同，拒绝恢复。
    DefinitionChanged {
        /// Journal 中冻结的摘要。
        expected: String,
        /// 当前编译摘要。
        actual: String,
    },
    /// 输入摘要不同，拒绝恢复。
    InputChanged {
        /// Journal 中冻结的摘要。
        expected: String,
        /// 当前输入摘要。
        actual: String,
    },
    /// 恢复快照与当前冻结定义或自身记录冲突。
    InvalidRecovery(String),
    /// 恢复检测到不可安全重放的副作用。
    Indeterminate {
        /// 未结算节点地址。
        address: NodeAddress,
        /// 节点副作用等级。
        effect: EffectClass,
    },
    /// Driver 执行失败。
    Driver {
        /// Driver 调用的节点地址。
        address: NodeAddress,
        /// 宿主返回的错误。
        source: DriverError,
    },
    /// Journal 持久化或通知失败。
    Journal(JournalError),
    /// 调用方限制没有收紧而是放宽。
    LimitsNotTightened,
    /// 运行被取消。
    Cancelled,
    /// 运行耗时超过限制。
    DurationExceeded,
    /// 输出超出限制。
    OutputLimitExceeded,
    /// 节点预算超出限制。
    NodeLimitExceeded,
    /// 数据求值失败。
    Evaluation(String),
    /// 规范化 JSON 失败。
    Serialization(String),
}

impl WorkflowError {
    /// 返回稳定错误代码。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation_error",
            Self::InvalidInputs(_) => "invalid_inputs",
            Self::DefinitionChanged { .. } => "definition_changed",
            Self::InputChanged { .. } => "input_changed",
            Self::InvalidRecovery(_) => "invalid_recovery",
            Self::Indeterminate { .. } => "indeterminate",
            Self::Driver { .. } => "driver_error",
            Self::Journal(_) => "journal_error",
            Self::LimitsNotTightened => "limits_not_tightened",
            Self::Cancelled => "cancelled",
            Self::DurationExceeded => "duration_exceeded",
            Self::OutputLimitExceeded => "output_limit_exceeded",
            Self::NodeLimitExceeded => "node_limit_exceeded",
            Self::Evaluation(_) => "evaluation_error",
            Self::Serialization(_) => "serialization_error",
        }
    }
}

impl Display for WorkflowError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(error) => write!(f, "{error}"),
            Self::InvalidInputs(message) => write!(f, "invalid inputs: {message}"),
            Self::DefinitionChanged { expected, actual } => {
                write!(
                    f,
                    "definition hash changed: expected {expected}, got {actual}"
                )
            }
            Self::InputChanged { expected, actual } => {
                write!(f, "input hash changed: expected {expected}, got {actual}")
            }
            Self::InvalidRecovery(message) => {
                write!(f, "invalid recovery snapshot: {message}")
            }
            Self::Indeterminate { address, effect } => {
                write!(
                    f,
                    "node {} is indeterminate ({effect:?})",
                    address.execution_id()
                )
            }
            Self::Driver { address, source } => {
                write!(f, "node {} driver failed: {source}", address.execution_id())
            }
            Self::Journal(error) => write!(f, "journal failed: {error}"),
            Self::LimitsNotTightened => write!(f, "requested limits must tighten compiled limits"),
            Self::Cancelled => write!(f, "workflow cancelled"),
            Self::DurationExceeded => write!(f, "workflow duration exceeded"),
            Self::OutputLimitExceeded => write!(f, "workflow output limit exceeded"),
            Self::NodeLimitExceeded => write!(f, "workflow node limit exceeded"),
            Self::Evaluation(message) => write!(f, "evaluation failed: {message}"),
            Self::Serialization(message) => write!(f, "serialization failed: {message}"),
        }
    }
}

impl std::error::Error for WorkflowError {}

/// 定义验证错误。
#[derive(Clone, Debug)]
pub enum ValidationError {
    /// 版本不支持。
    UnsupportedVersion(u32),
    /// 元数据名称为空。
    EmptyMetadataName,
    /// 限制为零或不合法。
    InvalidLimits,
    /// 输入名不合法。
    InvalidInputName(String),
    /// 默认值类型不匹配。
    InputDefaultType {
        /// 输入名称。
        name: String,
        /// 声明的类型。
        expected: InputType,
    },
    /// 节点 ID 重复。
    DuplicateNodeId(String),
    /// 节点 ID 不合法。
    InvalidNodeId(String),
    /// 宿主操作名为空。
    EmptyOperationName,
    /// 输出类型为空。
    EmptyOutputType,
    /// Pointer 语法不合法。
    InvalidPointer(String),
    /// Pointer 根作用域被禁止。
    DisallowedPointer(String),
    /// 引用了未知输入。
    UnknownInputReference(String),
    /// 引用了未知节点。
    UnknownNodeReference(String),
    /// 引用了当前执行位置尚未确定完成的节点（前向、并行兄弟或不可达分支）。
    NodeReferenceUnavailable(String),
    /// 循环没有提供正数预算。
    MissingIterationBudget(String),
    /// 循环预算超过编译限制。
    IterationBudgetExceeded {
        /// 循环节点 ID。
        node_id: String,
        /// 定义请求的迭代数。
        count: u32,
        /// 编译上限。
        max: u32,
    },
    /// 并行并发为零。
    InvalidParallelConcurrency(String),
    /// 条件集合为空。
    EmptyCondition(String),
    /// 图深度超过限制。
    DepthExceeded {
        /// 实际深度。
        depth: u16,
        /// 编译上限。
        max: u16,
    },
    /// 节点总预算超过限制。
    NodeBudgetExceeded {
        /// 静态估算节点数。
        count: u32,
        /// 编译上限。
        max: u32,
    },
    /// JSON 编码失败。
    Serialization(String),
}

impl Display for ValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported workflow version {version}")
            }
            Self::EmptyMetadataName => write!(f, "workflow metadata name is empty"),
            Self::InvalidLimits => write!(f, "workflow limits must be non-zero"),
            Self::InvalidInputName(name) => write!(f, "invalid input name {name}"),
            Self::InputDefaultType { name, expected } => {
                write!(f, "default for input {name} does not match {expected:?}")
            }
            Self::DuplicateNodeId(id) => write!(f, "duplicate node id {id}"),
            Self::InvalidNodeId(id) => write!(f, "invalid node id {id}"),
            Self::EmptyOperationName => write!(f, "operation name is empty"),
            Self::EmptyOutputType => write!(f, "output type is empty"),
            Self::InvalidPointer(pointer) => write!(f, "invalid JSON Pointer {pointer}"),
            Self::DisallowedPointer(pointer) => write!(f, "disallowed JSON Pointer {pointer}"),
            Self::UnknownInputReference(name) => write!(f, "unknown input reference {name}"),
            Self::UnknownNodeReference(id) => write!(f, "unknown node reference {id}"),
            Self::NodeReferenceUnavailable(id) => {
                write!(
                    f,
                    "node reference {id} is not available at this execution point"
                )
            }
            Self::MissingIterationBudget(id) => write!(f, "loop {id} requires max_iterations"),
            Self::IterationBudgetExceeded {
                node_id,
                count,
                max,
            } => {
                write!(f, "loop {node_id} has {count} iterations, maximum is {max}")
            }
            Self::InvalidParallelConcurrency(id) => {
                write!(f, "parallel node {id} has zero concurrency")
            }
            Self::EmptyCondition(kind) => write!(f, "condition {kind} cannot be empty"),
            Self::DepthExceeded { depth, max } => write!(f, "workflow depth {depth} exceeds {max}"),
            Self::NodeBudgetExceeded { count, max } => {
                write!(f, "workflow node budget {count} exceeds {max}")
            }
            Self::Serialization(message) => write!(f, "serialization failed: {message}"),
        }
    }
}

impl std::error::Error for ValidationError {}

impl From<ValidationError> for WorkflowError {
    fn from(value: ValidationError) -> Self {
        Self::Validation(value)
    }
}

fn hash_json<T: Serialize>(value: &T) -> Result<String, String> {
    let encoded = serde_json::to_value(value).map_err(|error| error.to_string())?;
    let canonical = canonical_value(encoded);
    let bytes = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{}", hex_digest(&digest)))
}

fn canonical_value(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted = object
                .into_iter()
                .map(|(key, value)| (key, canonical_value(value)))
                .collect::<BTreeMap<_, _>>();
            let mut object = Map::new();
            for (key, value) in sorted {
                object.insert(key, value);
            }
            Value::Object(object)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_value).collect()),
        value => value,
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// 将已补全输入规范化并计算 `sha256:<hex>` 摘要。
pub fn hash_inputs(inputs: &Value) -> Result<String, WorkflowError> {
    hash_json(inputs).map_err(WorkflowError::Serialization)
}

fn validate_and_resolve_inputs(
    specs: &BTreeMap<String, InputSpec>,
    inputs: Value,
) -> Result<Value, WorkflowError> {
    let Value::Object(mut provided) = inputs else {
        return Err(WorkflowError::InvalidInputs(
            "top-level inputs must be an object".into(),
        ));
    };
    for key in provided.keys() {
        if !specs.contains_key(key) {
            return Err(WorkflowError::InvalidInputs(format!("unknown input {key}")));
        }
    }
    let mut resolved = Map::new();
    for (name, spec) in specs {
        let value = provided.remove(name).or_else(|| spec.default.clone());
        let Some(value) = value else {
            if spec.required {
                return Err(WorkflowError::InvalidInputs(format!(
                    "missing input {name}"
                )));
            }
            continue;
        };
        if !spec.value_type.accepts(&value) {
            return Err(WorkflowError::InvalidInputs(format!(
                "input {name} does not match {:?}",
                spec.value_type
            )));
        }
        resolved.insert(name.clone(), value);
    }
    Ok(Value::Object(resolved))
}

#[derive(Clone, Debug)]
struct RecoveryNodeContract {
    kind: NodeKind,
    effect: EffectClass,
    output_type: Option<String>,
    /// 节点所在循环祖先的最大迭代数；地址必须为同样层级且每层索引小于上界。
    invocation_bounds: Vec<u32>,
}

fn collect_recovery_contracts(
    nodes: &[Node],
    contracts: &mut BTreeMap<String, RecoveryNodeContract>,
    invocation_bounds: &[u32],
) {
    for node in nodes {
        let effect = match node {
            Node::Sequence { .. }
            | Node::Parallel { .. }
            | Node::If { .. }
            | Node::Foreach { .. }
            | Node::Repeat { .. } => EffectClass::ReadOnly,
            Node::Agent(value) => value.effect,
            Node::Tool(value) => value.effect,
            Node::Artifact(value) => value.effect,
        };
        let output_type = match node {
            Node::Sequence { .. }
            | Node::Parallel { .. }
            | Node::If { .. }
            | Node::Foreach { .. }
            | Node::Repeat { .. } => Some(format!("control.{:?}", node.kind()).to_lowercase()),
            Node::Agent(value) => value.output_type.clone(),
            Node::Tool(value) => value.output_type.clone(),
            Node::Artifact(value) => value.output_type.clone(),
        };
        contracts.insert(
            node.node_id().to_owned(),
            RecoveryNodeContract {
                kind: node.kind(),
                effect,
                output_type,
                invocation_bounds: invocation_bounds.to_vec(),
            },
        );
        match node {
            Node::Sequence { nodes, .. } | Node::Parallel { nodes, .. } => {
                collect_recovery_contracts(nodes, contracts, invocation_bounds)
            }
            Node::If {
                then_body,
                else_body,
                ..
            } => {
                collect_recovery_contracts(then_body, contracts, invocation_bounds);
                collect_recovery_contracts(else_body, contracts, invocation_bounds);
            }
            Node::Foreach {
                body,
                max_iterations,
                ..
            }
            | Node::Repeat {
                body,
                max_iterations,
                ..
            } => {
                let mut body_bounds = invocation_bounds.to_vec();
                body_bounds.push(*max_iterations);
                collect_recovery_contracts(body, contracts, &body_bounds)
            }
            Node::Agent(_) | Node::Tool(_) | Node::Artifact(_) => {}
        }
    }
}

/// 校验恢复输入来自当前冻结图，并且不存在重复或相互冲突的事实。
///
/// RecoverySnapshot 是公开执行入口的输入，不能假设它一定由本 crate 的 Journal
/// 适配器生成；这里拒绝未知节点、循环地址形状、重复地址、started/completed
/// 冲突，以及冻结节点 kind 的 effect/output_type 契约不一致。
fn validate_recovery_snapshot(
    workflow: &CompiledWorkflow,
    recovery: &RecoverySnapshot,
) -> Result<(), WorkflowError> {
    let mut contracts = BTreeMap::new();
    collect_recovery_contracts(&workflow.definition.body, &mut contracts, &[]);
    let mut completed_addresses = HashSet::new();
    for completed in &recovery.completed {
        let Some(contract) = contracts.get(&completed.address.node_id) else {
            return Err(WorkflowError::InvalidRecovery(format!(
                "completed node {} is not in the frozen definition",
                completed.address.execution_id()
            )));
        };
        validate_recovery_address(&completed.address, contract)?;
        if !completed_addresses.insert(completed.address.clone()) {
            return Err(WorkflowError::InvalidRecovery(format!(
                "completed node {} appears more than once",
                completed.address.execution_id()
            )));
        }
        if completed.output.effect < contract.effect {
            return Err(WorkflowError::InvalidRecovery(format!(
                "completed node {} effect {:?} is weaker than frozen {:?}",
                completed.address.execution_id(),
                completed.output.effect,
                contract.effect
            )));
        }
        if let Some(expected) = &contract.output_type
            && completed.output.output_type.as_str() != expected.as_str()
        {
            return Err(WorkflowError::InvalidRecovery(format!(
                "completed node {} frozen kind {:?} requires output_type {expected}",
                completed.address.execution_id(),
                contract.kind
            )));
        }
    }

    let mut started_addresses = HashSet::new();
    for started in &recovery.started {
        let Some(contract) = contracts.get(&started.address.node_id) else {
            return Err(WorkflowError::InvalidRecovery(format!(
                "started node {} is not in the frozen definition",
                started.address.execution_id()
            )));
        };
        validate_recovery_address(&started.address, contract)?;
        if !started_addresses.insert(started.address.clone()) {
            return Err(WorkflowError::InvalidRecovery(format!(
                "started node {} appears more than once",
                started.address.execution_id()
            )));
        }
        if completed_addresses.contains(&started.address) {
            return Err(WorkflowError::InvalidRecovery(format!(
                "node {} is both completed and started",
                started.address.execution_id()
            )));
        }
        if started.effect != contract.effect {
            return Err(WorkflowError::InvalidRecovery(format!(
                "started node {} effect {:?} does not match frozen {:?}",
                started.address.execution_id(),
                started.effect,
                contract.effect
            )));
        }
    }
    Ok(())
}

fn validate_recovery_address(
    address: &NodeAddress,
    contract: &RecoveryNodeContract,
) -> Result<(), WorkflowError> {
    if address.invocation.len() != contract.invocation_bounds.len()
        || address
            .invocation
            .iter()
            .zip(&contract.invocation_bounds)
            .any(|(index, bound)| index >= bound)
    {
        return Err(WorkflowError::InvalidRecovery(format!(
            "node {} has an invocation address incompatible with frozen {:?}",
            address.execution_id(),
            contract.kind
        )));
    }
    Ok(())
}

#[derive(Clone)]
struct RuntimeState {
    nodes: Arc<Mutex<BTreeMap<String, Value>>>,
    outputs: Arc<Mutex<BTreeMap<String, TypedOutput>>>,
    results: Arc<Mutex<Vec<NodeResult>>>,
    artifacts: Arc<Mutex<Vec<WorkflowArtifactView>>>,
    output_bytes: Arc<Mutex<u64>>,
    started_nodes: Arc<Mutex<u32>>,
}

impl RuntimeState {
    fn new() -> Self {
        Self {
            nodes: Arc::new(Mutex::new(BTreeMap::new())),
            outputs: Arc::new(Mutex::new(BTreeMap::new())),
            results: Arc::new(Mutex::new(Vec::new())),
            artifacts: Arc::new(Mutex::new(Vec::new())),
            output_bytes: Arc::new(Mutex::new(0)),
            started_nodes: Arc::new(Mutex::new(0)),
        }
    }
}

/// 工作流执行器。
pub struct WorkflowExecutor {
    shared: Arc<ExecutorShared>,
}

struct ExecutorShared {
    workflow: Arc<CompiledWorkflow>,
    driver: Arc<dyn WorkflowDriver>,
    journal: Arc<dyn WorkflowJournal>,
}

impl WorkflowExecutor {
    /// 创建绑定编译定义、宿主 Driver 和 Journal 的执行器。
    pub fn new(
        workflow: CompiledWorkflow,
        driver: Arc<dyn WorkflowDriver>,
        journal: Arc<dyn WorkflowJournal>,
    ) -> Self {
        Self {
            shared: Arc::new(ExecutorShared {
                workflow: Arc::new(workflow),
                driver,
                journal,
            }),
        }
    }

    /// 返回绑定的编译定义。
    pub fn workflow(&self) -> &CompiledWorkflow {
        &self.shared.workflow
    }

    /// 从 Journal 加载恢复快照后执行；不存在快照时按新运行执行。
    pub fn resume(
        &self,
        mut request: ExecutionRequest,
    ) -> WorkflowFuture<'_, Result<ExecutionResult, WorkflowError>> {
        let journal = self.shared.journal.clone();
        Box::pin(async move {
            request.recovery = journal
                .load_recovery(&request.run_id)
                .await
                .map_err(WorkflowError::Journal)?;
            self.execute(request).await
        })
    }

    /// 执行定义。Journal 追加失败会在 Driver 调用前返回。
    pub fn execute(
        &self,
        request: ExecutionRequest,
    ) -> WorkflowFuture<'_, Result<ExecutionResult, WorkflowError>> {
        Box::pin(async move {
            let input_hash_before = hash_inputs(&request.inputs)?;
            let inputs = validate_and_resolve_inputs(
                &self.shared.workflow.definition.inputs,
                request.inputs,
            )?;
            let input_hash = hash_inputs(&inputs)?;
            if let Some(recovery) = &request.recovery {
                if recovery.run_id != request.run_id {
                    return Err(WorkflowError::InvalidInputs(
                        "recovery run id mismatch".into(),
                    ));
                }
                if recovery.definition_hash != self.shared.workflow.definition_hash {
                    return Err(WorkflowError::DefinitionChanged {
                        expected: recovery.definition_hash.clone(),
                        actual: self.shared.workflow.definition_hash.clone(),
                    });
                }
                if recovery.input_hash != input_hash && recovery.input_hash != input_hash_before {
                    return Err(WorkflowError::InputChanged {
                        expected: recovery.input_hash.clone(),
                        actual: input_hash.clone(),
                    });
                }
                validate_recovery_snapshot(&self.shared.workflow, recovery)?;
            }
            if !self.shared.workflow.limits.allows(&request.options.limits)
                || request.options.limits.validate().is_err()
            {
                return Err(WorkflowError::LimitsNotTightened);
            }
            let runtime_token = request.options.cancellation_token.child_token();
            let state = RuntimeState::new();
            let semaphore = Arc::new(Semaphore::new(
                request.options.limits.max_concurrency as usize,
            ));
            let write_gate = Arc::new(Mutex::new(()));
            let context = RunContext {
                executor: self.shared.clone(),
                run_id: request.run_id.clone(),
                inputs: Arc::new(inputs),
                runtime_token: runtime_token.clone(),
                semaphore,
                write_gate,
                state: state.clone(),
                limits: request.options.limits,
                recovery: request.recovery,
            };
            let run = context.execute_body(
                &self.shared.workflow.definition.body,
                Vec::new(),
                Value::Null,
            );
            let outcome = time::timeout(
                Duration::from_millis(request.options.limits.max_duration_ms),
                run,
            )
            .await;
            let status = match outcome {
                Ok(Ok(())) => {
                    if runtime_token.is_cancelled() {
                        RunStatus::Cancelled
                    } else {
                        RunStatus::Succeeded
                    }
                }
                Ok(Err(error @ WorkflowError::Indeterminate { .. })) => {
                    runtime_token.cancel();
                    return context
                        .finish_with_error(RunStatus::Indeterminate, input_hash, error)
                        .await;
                }
                Ok(Err(WorkflowError::Cancelled)) => RunStatus::Cancelled,
                Ok(Err(error)) => {
                    runtime_token.cancel();
                    return context
                        .finish_with_error(RunStatus::Failed, input_hash, error)
                        .await;
                }
                Err(_) => {
                    runtime_token.cancel();
                    return context
                        .finish_with_error(
                            RunStatus::Cancelled,
                            input_hash,
                            WorkflowError::DurationExceeded,
                        )
                        .await;
                }
            };
            context.finish(status, input_hash).await
        })
    }
}

struct RunContext {
    executor: Arc<ExecutorShared>,
    run_id: String,
    inputs: Arc<Value>,
    runtime_token: CancellationToken,
    semaphore: Arc<Semaphore>,
    write_gate: Arc<Mutex<()>>,
    state: RuntimeState,
    limits: WorkflowLimits,
    recovery: Option<RecoverySnapshot>,
}

impl RunContext {
    async fn finish(
        &self,
        status: RunStatus,
        input_hash: String,
    ) -> Result<ExecutionResult, WorkflowError> {
        let event = JournalEvent::RunFinished(RunFinished {
            run_id: self.run_id.clone(),
            status,
            definition_hash: self.executor.workflow.definition_hash.clone(),
            input_hash: input_hash.clone(),
            error: None,
        });
        self.record(event).await?;
        Ok(self.result(status, input_hash).await)
    }

    async fn finish_with_error(
        &self,
        status: RunStatus,
        input_hash: String,
        error: WorkflowError,
    ) -> Result<ExecutionResult, WorkflowError> {
        let event = JournalEvent::RunFinished(RunFinished {
            run_id: self.run_id.clone(),
            status,
            definition_hash: self.executor.workflow.definition_hash.clone(),
            input_hash,
            error: Some(PublicError::from(&error)),
        });
        self.record(event).await?;
        Err(error)
    }

    async fn result(&self, status: RunStatus, input_hash: String) -> ExecutionResult {
        let outputs = self.state.outputs.lock().await.clone();
        let node_results = self.state.results.lock().await.clone();
        let artifacts = self.state.artifacts.lock().await.clone();
        let output_bytes = *self.state.output_bytes.lock().await;
        ExecutionResult {
            run_id: self.run_id.clone(),
            definition_hash: self.executor.workflow.definition_hash.clone(),
            input_hash,
            status,
            outputs,
            node_results,
            artifacts,
            graph: self.executor.workflow.graph.clone(),
            output_bytes,
        }
    }

    async fn record(&self, event: JournalEvent) -> Result<(), WorkflowError> {
        self.executor
            .journal
            .append(event.clone())
            .await
            .map_err(WorkflowError::Journal)?;
        self.executor
            .journal
            .notify(event)
            .await
            .map_err(WorkflowError::Journal)
    }

    fn execute_body<'b>(
        &'b self,
        nodes: &'b [Node],
        invocation: Vec<u32>,
        iteration: Value,
    ) -> WorkflowFuture<'b, Result<(), WorkflowError>> {
        Box::pin(async move {
            for node in nodes {
                self.check_cancelled()?;
                self.execute_node(node, invocation.clone(), iteration.clone())
                    .await?;
            }
            Ok(())
        })
    }

    fn execute_node<'b>(
        &'b self,
        node: &'b Node,
        invocation: Vec<u32>,
        iteration: Value,
    ) -> WorkflowFuture<'b, Result<TypedOutput, WorkflowError>> {
        Box::pin(async move {
            self.check_cancelled()?;
            if matches!(
                node,
                Node::Sequence { .. }
                    | Node::Parallel { .. }
                    | Node::If { .. }
                    | Node::Foreach { .. }
                    | Node::Repeat { .. }
            ) {
                return self.execute_control(node, invocation, iteration).await;
            }
            match node {
                Node::Agent(agent) => {
                    self.execute_leaf(
                        node,
                        agent.node_id.clone(),
                        agent.name.clone(),
                        agent.input.clone(),
                        agent.config.clone(),
                        agent.output_type.clone(),
                        agent.effect,
                        invocation,
                        iteration,
                        DriverLeafKind::Agent,
                    )
                    .await
                }
                Node::Tool(tool) => {
                    self.execute_leaf(
                        node,
                        tool.node_id.clone(),
                        tool.name.clone(),
                        tool.input.clone(),
                        tool.config.clone(),
                        tool.output_type.clone(),
                        tool.effect,
                        invocation,
                        iteration,
                        DriverLeafKind::Tool,
                    )
                    .await
                }
                Node::Artifact(artifact) => {
                    self.execute_leaf(
                        node,
                        artifact.node_id.clone(),
                        artifact.name.clone(),
                        artifact.input.clone(),
                        artifact.config.clone(),
                        artifact.output_type.clone(),
                        artifact.effect,
                        invocation,
                        iteration,
                        DriverLeafKind::Artifact,
                    )
                    .await
                }
                Node::Sequence { .. }
                | Node::Parallel { .. }
                | Node::If { .. }
                | Node::Foreach { .. }
                | Node::Repeat { .. } => unreachable!("control nodes are handled above"),
            }
        })
    }

    async fn execute_control(
        &self,
        node: &Node,
        invocation: Vec<u32>,
        iteration: Value,
    ) -> Result<TypedOutput, WorkflowError> {
        let address = NodeAddress {
            node_id: node.node_id().to_owned(),
            invocation: invocation.clone(),
        };
        if let Some(recovery) = &self.recovery
            && let Some(cached) = recovery
                .completed
                .iter()
                .find(|item| item.address == address)
        {
            let output = cached.output.clone();
            self.store_output(
                &address,
                node.kind(),
                NodeStatus::Succeeded,
                output.clone(),
                None,
            )
            .await?;
            self.record(JournalEvent::NodeReused(NodeReused {
                run_id: self.run_id.clone(),
                address,
                output: output.clone(),
            }))
            .await?;
            return Ok(output);
        }
        self.count_node().await?;
        self.record(JournalEvent::NodeStarted(NodeStarted {
            run_id: self.run_id.clone(),
            address: address.clone(),
            effect: EffectClass::ReadOnly,
        }))
        .await?;
        let result = self
            .execute_control_inner(node, invocation, iteration)
            .await;
        match result {
            Ok(output) => {
                if let Err(error) = self.account_output_bytes(output.bytes).await {
                    let _ = self
                        .settle_control_error(&address, node.kind(), &error)
                        .await;
                    return Err(error);
                }
                self.record(JournalEvent::NodeSettled(NodeSettled {
                    run_id: self.run_id.clone(),
                    address: address.clone(),
                    status: NodeStatus::Succeeded,
                    output: Some(output.clone()),
                    error: None,
                }))
                .await?;
                self.store_output(
                    &address,
                    node.kind(),
                    NodeStatus::Succeeded,
                    output.clone(),
                    None,
                )
                .await?;
                Ok(output)
            }
            Err(error) => {
                self.settle_control_error(&address, node.kind(), &error)
                    .await?;
                Err(error)
            }
        }
    }

    async fn execute_control_inner(
        &self,
        node: &Node,
        invocation: Vec<u32>,
        iteration: Value,
    ) -> Result<TypedOutput, WorkflowError> {
        match node {
            Node::Sequence { nodes, .. } => {
                self.execute_body(nodes, invocation, iteration).await?;
                Ok(self.control_output(node, nodes.iter().map(Node::node_id).collect()))
            }
            Node::Parallel {
                nodes, concurrency, ..
            } => {
                self.execute_parallel(nodes, &invocation, &iteration, *concurrency)
                    .await?;
                Ok(self.control_output(node, nodes.iter().map(Node::node_id).collect()))
            }
            Node::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                let branch = self.evaluate_condition(condition, &iteration).await?;
                let selected = if branch { then_body } else { else_body };
                self.execute_body(selected, invocation, iteration).await?;
                Ok(TypedOutput::new(
                    serde_json::json!({"branch": if branch {"then"} else {"else"}}),
                    "control.if",
                    EffectClass::ReadOnly,
                ))
            }
            Node::Foreach {
                items,
                body,
                max_iterations,
                ..
            } => {
                let items = self.resolve_expr(items, &iteration).await?;
                let array = items.as_array().ok_or_else(|| {
                    WorkflowError::Evaluation(format!(
                        "foreach {} expects an array",
                        node.node_id()
                    ))
                })?;
                if array.len() > *max_iterations as usize
                    || array.len() > self.limits.max_iterations as usize
                {
                    return Err(WorkflowError::NodeLimitExceeded);
                }
                for (index, item) in array.iter().enumerate() {
                    let mut child_invocation = invocation.clone();
                    child_invocation.push(index as u32);
                    let loop_value = serde_json::json!({"index": index, "item": item});
                    self.execute_body(body, child_invocation, loop_value)
                        .await?;
                }
                Ok(TypedOutput::new(
                    serde_json::json!({"iterations": array.len()}),
                    "control.foreach",
                    EffectClass::ReadOnly,
                ))
            }
            Node::Repeat {
                body,
                max_iterations,
                ..
            } => {
                if *max_iterations > self.limits.max_iterations {
                    return Err(WorkflowError::NodeLimitExceeded);
                }
                for index in 0..*max_iterations {
                    let mut child_invocation = invocation.clone();
                    child_invocation.push(index);
                    self.execute_body(body, child_invocation, serde_json::json!({"index": index}))
                        .await?;
                }
                Ok(TypedOutput::new(
                    serde_json::json!({"iterations": max_iterations}),
                    "control.repeat",
                    EffectClass::ReadOnly,
                ))
            }
            Node::Agent(_) | Node::Tool(_) | Node::Artifact(_) => {
                unreachable!("leaf nodes are handled by execute_node")
            }
        }
    }

    async fn execute_parallel(
        &self,
        nodes: &[Node],
        invocation: &[u32],
        iteration: &Value,
        local_concurrency: Option<u16>,
    ) -> Result<(), WorkflowError> {
        let local_gate = local_concurrency.map(|limit| Arc::new(Semaphore::new(limit as usize)));
        let mut tasks = JoinSet::new();
        for node in nodes {
            let context = self.clone_for_task();
            let node = node.clone();
            let invocation = invocation.to_vec();
            let iteration = iteration.clone();
            let local_gate = local_gate.clone();
            tasks.spawn(async move {
                let _permit = match local_gate {
                    Some(gate) => Some(
                        gate.acquire_owned()
                            .await
                            .map_err(|_| WorkflowError::Cancelled)?,
                    ),
                    None => None,
                };
                context
                    .execute_node(&node, invocation, iteration)
                    .await
                    .map(|_| ())
            });
        }
        let mut first_error = None;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let cancelled = matches!(&error, WorkflowError::Cancelled);
                    let replace_cancelled = first_error
                        .as_ref()
                        .is_some_and(|existing| matches!(existing, WorkflowError::Cancelled));
                    if first_error.is_none() || (replace_cancelled && !cancelled) {
                        first_error = Some(error);
                    }
                    if !cancelled {
                        // 让在途 Driver 先收到取消令牌并自行收尾，避免 abort
                        // 直接丢弃其清理逻辑和取消证据。忽略取消的 Driver 仍受
                        // 外层 max_duration_ms 超时保护。
                        self.runtime_token.cancel();
                    }
                }
                Err(join_error) => {
                    let error = WorkflowError::Evaluation(join_error.to_string());
                    let replace_cancelled = first_error
                        .as_ref()
                        .is_some_and(|existing| matches!(existing, WorkflowError::Cancelled));
                    if first_error.is_none() || replace_cancelled {
                        first_error = Some(error);
                    }
                    self.runtime_token.cancel();
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn clone_for_task(&self) -> RunContext {
        RunContext {
            executor: self.executor.clone(),
            run_id: self.run_id.clone(),
            inputs: self.inputs.clone(),
            runtime_token: self.runtime_token.clone(),
            semaphore: self.semaphore.clone(),
            write_gate: self.write_gate.clone(),
            state: self.state.clone(),
            limits: self.limits,
            recovery: self.recovery.clone(),
        }
    }

    // 叶节点的静态配置需要完整传入 Driver，保持宿主适配不依赖隐藏的全局状态。
    #[allow(clippy::too_many_arguments)]
    async fn execute_leaf(
        &self,
        node: &Node,
        node_id: String,
        name: String,
        input_expr: ValueExpr,
        config: Value,
        output_type: Option<String>,
        declared_effect: EffectClass,
        invocation: Vec<u32>,
        iteration: Value,
        kind: DriverLeafKind,
    ) -> Result<TypedOutput, WorkflowError> {
        let address = NodeAddress {
            node_id: node_id.clone(),
            invocation,
        };
        if let Some(recovery) = &self.recovery {
            if let Some(cached) = recovery
                .completed
                .iter()
                .find(|item| item.address == address)
            {
                let output = cached.output.clone();
                self.store_output(
                    &address,
                    node.kind(),
                    NodeStatus::Succeeded,
                    output.clone(),
                    None,
                )
                .await?;
                self.record(JournalEvent::NodeReused(NodeReused {
                    run_id: self.run_id.clone(),
                    address,
                    output: output.clone(),
                }))
                .await?;
                return Ok(output);
            }
            if let Some(started) = recovery.started.iter().find(|item| item.address == address) {
                if !started.effect.is_mutating() {
                    let input = self.resolve_expr(&input_expr, &iteration).await?;
                    let context = self.make_context(&address, &iteration).await;
                    let request = DriverRequest {
                        leaf_kind: kind,
                        node_id: node_id.clone(),
                        name: name.clone(),
                        input,
                        config: config.clone(),
                        output_type: output_type.clone(),
                        effect: declared_effect,
                    };
                    let output = match kind {
                        DriverLeafKind::Agent => {
                            self.executor.driver.recover_agent(request, context)
                        }
                        DriverLeafKind::Tool => self.executor.driver.recover_tool(request, context),
                        DriverLeafKind::Artifact => {
                            self.executor.driver.recover_artifact(request, context)
                        }
                    }
                    .await
                    .map_err(|source| WorkflowError::Driver {
                        address: address.clone(),
                        source,
                    })?;
                    return self
                        .settle_leaf(&address, node.kind(), declared_effect, output, None)
                        .await;
                }
                return Err(WorkflowError::Indeterminate {
                    address,
                    effect: started.effect,
                });
            }
        }
        self.count_node().await?;
        let input = self.resolve_expr(&input_expr, &iteration).await?;
        let permit = tokio::select! {
            _ = self.runtime_token.cancelled() => return Err(WorkflowError::Cancelled),
            permit = self.semaphore.clone().acquire_owned() => {
                permit.map_err(|_| WorkflowError::Cancelled)?
            }
        };
        let request = DriverRequest {
            leaf_kind: kind,
            node_id: node_id.clone(),
            name,
            input,
            config,
            output_type,
            effect: declared_effect,
        };
        let _write_guard = if declared_effect.requires_serial_gate() {
            Some(tokio::select! {
                _ = self.runtime_token.cancelled() => return Err(WorkflowError::Cancelled),
                guard = self.write_gate.lock() => guard,
            })
        } else {
            None
        };
        self.check_cancelled()?;
        // 只有在取得并发/写入许可且确认未取消后，才把节点标记为已启动。
        self.record(JournalEvent::NodeStarted(NodeStarted {
            run_id: self.run_id.clone(),
            address: address.clone(),
            effect: declared_effect,
        }))
        .await?;
        if self.runtime_token.is_cancelled() {
            let error = WorkflowError::Cancelled;
            self.settle_leaf_error(&address, node.kind(), declared_effect, &error)
                .await?;
            return Err(error);
        }
        let context = self.make_context(&address, &iteration).await;
        let result = match kind {
            DriverLeafKind::Agent => self.executor.driver.execute_agent(request, context).await,
            DriverLeafKind::Tool => self.executor.driver.execute_tool(request, context).await,
            DriverLeafKind::Artifact => {
                self.executor
                    .driver
                    .execute_artifact(request, context)
                    .await
            }
        };
        // 只把 Driver 返回前已经触发的取消视为取消结算；首次真实 Driver 错误仍需
        // 保留为失败，不能因为错误路径随后广播取消而被误分类。
        let cancellation_requested = self.runtime_token.is_cancelled();
        if result.is_err() && !cancellation_requested {
            // 在释放并发许可前广播失败，避免排队兄弟抢到许可后又开始新的 Driver 调用。
            self.runtime_token.cancel();
        }
        drop(_write_guard);
        drop(permit);
        match result {
            Ok(output) => {
                self.settle_leaf(&address, node.kind(), declared_effect, output, None)
                    .await
            }
            Err(source) => {
                let error = if cancellation_requested {
                    WorkflowError::Cancelled
                } else {
                    WorkflowError::Driver {
                        address: address.clone(),
                        source,
                    }
                };
                self.settle_leaf_error(&address, node.kind(), declared_effect, &error)
                    .await?;
                Err(error)
            }
        }
    }

    async fn count_node(&self) -> Result<(), WorkflowError> {
        let mut started_nodes = self.state.started_nodes.lock().await;
        *started_nodes = started_nodes.saturating_add(1);
        if *started_nodes > self.limits.max_nodes {
            return Err(WorkflowError::NodeLimitExceeded);
        }
        Ok(())
    }

    async fn account_output_bytes(&self, bytes: u64) -> Result<(), WorkflowError> {
        let mut total = self.state.output_bytes.lock().await;
        *total = total.saturating_add(bytes);
        if *total > self.limits.max_output_bytes {
            return Err(WorkflowError::OutputLimitExceeded);
        }
        Ok(())
    }

    async fn settle_leaf(
        &self,
        address: &NodeAddress,
        kind: NodeKind,
        declared_effect: EffectClass,
        mut output: TypedOutput,
        error: Option<PublicError>,
    ) -> Result<TypedOutput, WorkflowError> {
        output.effect = declared_effect.join(output.effect);
        let bytes = serde_json::to_vec(&output.value)
            .map_err(|error| WorkflowError::Serialization(error.to_string()))?
            .len() as u64;
        output.bytes = bytes;
        if let Err(error) = self.account_output_bytes(bytes).await {
            self.settle_leaf_error(address, kind, declared_effect, &error)
                .await?;
            return Err(error);
        }
        self.record(JournalEvent::NodeSettled(NodeSettled {
            run_id: self.run_id.clone(),
            address: address.clone(),
            status: NodeStatus::Succeeded,
            output: Some(output.clone()),
            error,
        }))
        .await?;
        self.store_output(address, kind, NodeStatus::Succeeded, output.clone(), None)
            .await?;
        Ok(output)
    }

    async fn settle_leaf_error(
        &self,
        address: &NodeAddress,
        kind: NodeKind,
        declared_effect: EffectClass,
        error: &WorkflowError,
    ) -> Result<(), WorkflowError> {
        self.record(JournalEvent::NodeSettled(NodeSettled {
            run_id: self.run_id.clone(),
            address: address.clone(),
            status: if matches!(error, WorkflowError::Cancelled) {
                NodeStatus::Cancelled
            } else {
                NodeStatus::Failed
            },
            output: None,
            error: Some(PublicError::from(error)),
        }))
        .await?;
        self.store_output(
            address,
            kind,
            if matches!(error, WorkflowError::Cancelled) {
                NodeStatus::Cancelled
            } else {
                NodeStatus::Failed
            },
            TypedOutput::new(Value::Null, "error", declared_effect),
            Some(PublicError::from(error)),
        )
        .await
    }

    async fn settle_control_error(
        &self,
        address: &NodeAddress,
        kind: NodeKind,
        error: &WorkflowError,
    ) -> Result<(), WorkflowError> {
        self.record(JournalEvent::NodeSettled(NodeSettled {
            run_id: self.run_id.clone(),
            address: address.clone(),
            status: if matches!(error, WorkflowError::Cancelled) {
                NodeStatus::Cancelled
            } else {
                NodeStatus::Failed
            },
            output: None,
            error: Some(PublicError::from(error)),
        }))
        .await?;
        self.store_output(
            address,
            kind,
            if matches!(error, WorkflowError::Cancelled) {
                NodeStatus::Cancelled
            } else {
                NodeStatus::Failed
            },
            TypedOutput::new(Value::Null, "error", EffectClass::ReadOnly),
            Some(PublicError::from(error)),
        )
        .await
    }

    async fn store_output(
        &self,
        address: &NodeAddress,
        kind: NodeKind,
        status: NodeStatus,
        output: TypedOutput,
        error: Option<PublicError>,
    ) -> Result<(), WorkflowError> {
        self.state
            .nodes
            .lock()
            .await
            .insert(address.node_id.clone(), output.value.clone());
        self.state
            .outputs
            .lock()
            .await
            .insert(address.node_id.clone(), output.clone());
        self.state.results.lock().await.push(NodeResult {
            address: address.clone(),
            kind,
            status,
            output: if status == NodeStatus::Succeeded {
                Some(output.clone())
            } else {
                None
            },
            error,
        });
        if kind == NodeKind::Artifact && status == NodeStatus::Succeeded {
            self.state
                .artifacts
                .lock()
                .await
                .push(WorkflowArtifactView {
                    address: address.clone(),
                    output_type: output.output_type.clone(),
                    value: output.value.clone(),
                    bytes: output.bytes,
                });
        }
        Ok(())
    }

    fn control_output(&self, node: &Node, child_ids: Vec<&str>) -> TypedOutput {
        TypedOutput::new(
            serde_json::json!({"node_id": node.node_id(), "children": child_ids}),
            format!("control.{:?}", node.kind()).to_lowercase(),
            EffectClass::ReadOnly,
        )
    }

    async fn make_context(&self, address: &NodeAddress, iteration: &Value) -> NodeContext {
        NodeContext {
            run_id: self.run_id.clone(),
            node_id: address.node_id.clone(),
            address: address.clone(),
            inputs: self.inputs.clone(),
            nodes: Arc::new(self.state.nodes.lock().await.clone()),
            iteration: iteration.clone(),
            cancellation_token: self.runtime_token.clone(),
        }
    }

    async fn resolve_expr(
        &self,
        expression: &ValueExpr,
        iteration: &Value,
    ) -> Result<Value, WorkflowError> {
        match expression {
            ValueExpr::Literal { value } => Ok(value.clone()),
            ValueExpr::Ref { pointer } => {
                let parts = decode_pointer(pointer)
                    .map_err(|error| WorkflowError::Evaluation(error.to_string()))?;
                let nodes = self.state.nodes.lock().await.clone();
                let root = serde_json::json!({
                    "inputs": self.inputs.as_ref(),
                    "nodes": nodes,
                    "iteration": iteration,
                });
                resolve_pointer(&root, &parts).cloned().ok_or_else(|| {
                    WorkflowError::Evaluation(format!("reference {pointer} is missing"))
                })
            }
        }
    }

    fn evaluate_condition<'b>(
        &'b self,
        condition: &'b Condition,
        iteration: &'b Value,
    ) -> WorkflowFuture<'b, Result<bool, WorkflowError>> {
        Box::pin(async move {
            match condition {
                Condition::Eq { left, right } => Ok(self.resolve_expr(left, iteration).await?
                    == self.resolve_expr(right, iteration).await?),
                Condition::Ne { left, right } => Ok(self.resolve_expr(left, iteration).await?
                    != self.resolve_expr(right, iteration).await?),
                Condition::Lt { left, right } => {
                    self.compare_numeric(left, right, iteration, |a, b| a < b)
                        .await
                }
                Condition::Lte { left, right } => {
                    self.compare_numeric(left, right, iteration, |a, b| a <= b)
                        .await
                }
                Condition::Gt { left, right } => {
                    self.compare_numeric(left, right, iteration, |a, b| a > b)
                        .await
                }
                Condition::Gte { left, right } => {
                    self.compare_numeric(left, right, iteration, |a, b| a >= b)
                        .await
                }
                Condition::Exists { value } => match value {
                    ValueExpr::Literal { .. } => Ok(true),
                    ValueExpr::Ref { pointer } => {
                        let parts = decode_pointer(pointer)
                            .map_err(|error| WorkflowError::Evaluation(error.to_string()))?;
                        let nodes = self.state.nodes.lock().await.clone();
                        let root = serde_json::json!({
                            "inputs": self.inputs.as_ref(),
                            "nodes": nodes,
                            "iteration": iteration,
                        });
                        Ok(resolve_pointer(&root, &parts).is_some())
                    }
                },
                Condition::And { all } => {
                    for condition in all {
                        if !self.evaluate_condition(condition, iteration).await? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                Condition::Or { any } => {
                    for condition in any {
                        if self.evaluate_condition(condition, iteration).await? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
                Condition::Not { condition } => {
                    Ok(!self.evaluate_condition(condition, iteration).await?)
                }
            }
        })
    }

    async fn compare_numeric<F: FnOnce(f64, f64) -> bool>(
        &self,
        left: &ValueExpr,
        right: &ValueExpr,
        iteration: &Value,
        compare: F,
    ) -> Result<bool, WorkflowError> {
        let left = self.resolve_expr(left, iteration).await?.as_f64();
        let right = self.resolve_expr(right, iteration).await?.as_f64();
        match (left, right) {
            (Some(left), Some(right)) => Ok(compare(left, right)),
            _ => Err(WorkflowError::Evaluation(
                "numeric condition needs two numbers".into(),
            )),
        }
    }

    fn check_cancelled(&self) -> Result<(), WorkflowError> {
        if self.runtime_token.is_cancelled() {
            Err(WorkflowError::Cancelled)
        } else {
            Ok(())
        }
    }
}

fn resolve_pointer<'a>(root: &'a Value, parts: &[String]) -> Option<&'a Value> {
    let mut current = root;
    for part in parts {
        current = match current {
            Value::Object(object) => object.get(part)?,
            Value::Array(array) => array.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::time::sleep;

    #[test]
    fn rejects_unknown_control_fields_and_script_conditions() {
        // 执行定义不能默默忽略作者以为会执行的脚本或字段，避免保存后语义改变。
        let invalid_nodes = [
            serde_json::json!({"type":"sequence","node_id":"a","nodes":[],"script":"run()"}),
            serde_json::json!({"type":"parallel","node_id":"a","nodes":[],"concurreny":2}),
            serde_json::json!({"type":"repeat","node_id":"a","body":[],"max_iterations":1,"until":"ready"}),
        ];
        for node in invalid_nodes {
            assert!(serde_json::from_value::<Node>(node).is_err());
        }
        let invalid_conditions = [
            serde_json::json!({"op":"expression","code":"ready === true"}),
            serde_json::json!({"op":"exists","value":{"type":"literal","value":true},"script":"ready"}),
        ];
        for condition in invalid_conditions {
            assert!(serde_json::from_value::<Condition>(condition).is_err());
        }
        assert!(
            serde_json::from_value::<WorkflowLimits>(serde_json::json!({"maxConcurrency":2}))
                .is_err()
        );
    }

    #[test]
    fn input_spec_preserves_source_json_type_description_and_hashes_both() {
        let spec: InputSpec = serde_json::from_value(serde_json::json!({
            "type": "json",
            "description": "自由格式筛选条件",
            "required": true,
            "default": {"enabled": true}
        }))
        .expect("source JSON input declaration should deserialize");
        assert_eq!(spec.value_type, InputType::Json);
        assert_eq!(spec.description.as_deref(), Some("自由格式筛选条件"));
        assert!(spec.value_type.accepts(&serde_json::json!(null)));
        assert!(
            spec.value_type
                .accepts(&serde_json::json!([1, {"ok": true}]))
        );

        let mut with_description = definition(Vec::new());
        with_description.inputs.insert("filter".into(), spec);
        let mut without_description = with_description.clone();
        without_description
            .inputs
            .get_mut("filter")
            .expect("filter input")
            .description = None;
        assert_ne!(
            compile(with_description).unwrap().definition_hash(),
            compile(without_description).unwrap().definition_hash()
        );
    }

    fn literal(value: Value) -> ValueExpr {
        ValueExpr::Literal { value }
    }

    fn tool(id: &str, input: ValueExpr, effect: EffectClass) -> Node {
        Node::Tool(ToolNode {
            node_id: id.into(),
            name: "echo".into(),
            input,
            config: Value::Null,
            output_type: Some("json".into()),
            effect,
        })
    }

    fn definition(body: Vec<Node>) -> WorkflowDefinitionV1 {
        WorkflowDefinitionV1 {
            version: 1,
            meta: WorkflowMeta {
                name: "test".into(),
                ..WorkflowMeta::default()
            },
            inputs: BTreeMap::new(),
            body,
        }
    }

    #[test]
    fn artifact_slot_estimate_matches_parent_save_points() {
        let workflow = definition(vec![
            Node::Agent(AgentNode {
                node_id: "agent".into(),
                name: "agent".into(),
                input: literal(Value::Null),
                config: Value::Null,
                output_type: Some("json".into()),
                effect: EffectClass::ReadOnly,
            }),
            tool("read", literal(Value::Null), EffectClass::ReadOnly),
            Node::Artifact(ArtifactNode {
                node_id: "artifact".into(),
                name: "artifact".into(),
                input: literal(serde_json::json!({"data": "ok"})),
                config: Value::Null,
                output_type: Some("markdown".into()),
                effect: EffectClass::Write,
            }),
            Node::Repeat {
                node_id: "loop".into(),
                body: vec![tool(
                    "loop-read",
                    literal(Value::Null),
                    EffectClass::ReadOnly,
                )],
                max_iterations: 3,
            },
        ]);
        let compiled = compile(workflow).expect("artifact slot definition should compile");
        assert_eq!(compiled.artifact_slot_count(), 5);
        assert_eq!(
            compiled.artifact_slots_after_completed(&[
                NodeAddress {
                    node_id: "read".into(),
                    invocation: Vec::new(),
                },
                NodeAddress {
                    node_id: "loop-read".into(),
                    invocation: vec![0],
                },
                NodeAddress {
                    node_id: "loop-read".into(),
                    invocation: vec![0],
                },
                NodeAddress {
                    node_id: "agent".into(),
                    invocation: Vec::new(),
                },
            ]),
            3
        );
    }

    #[derive(Default)]
    struct TestDriver {
        calls: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
        cancelled: AtomicBool,
        delay_ms: u64,
        addresses: Mutex<Vec<NodeAddress>>,
        requests: Mutex<Vec<(NodeAddress, Value)>>,
    }

    impl TestDriver {
        fn enter(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
        }

        fn leave(&self) {
            self.active.fetch_sub(1, Ordering::SeqCst);
        }

        fn run<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            Box::pin(async move {
                self.addresses.lock().await.push(context.address.clone());
                self.requests
                    .lock()
                    .await
                    .push((context.address.clone(), request.input.clone()));
                self.enter();
                if self.delay_ms > 0 {
                    tokio::select! {
                        _ = sleep(Duration::from_millis(self.delay_ms)) => {}
                        _ = context.cancellation_token.cancelled() => {
                            self.cancelled.store(true, Ordering::SeqCst);
                            self.leave();
                            return Err(DriverError::new("cancelled", "cancelled"));
                        }
                    }
                }
                self.leave();
                Ok(TypedOutput::new(request.input, "json", request.effect))
            })
        }
    }

    impl WorkflowDriver for TestDriver {
        fn execute_agent<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }

        fn execute_tool<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }

        fn execute_artifact<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }
    }

    #[derive(Default)]
    struct LeafRecordingDriver {
        calls: Mutex<Vec<(DriverLeafKind, String)>>,
        trace: Arc<Mutex<Vec<String>>>,
    }

    impl LeafRecordingDriver {
        fn run<'a>(
            &'a self,
            request: DriverRequest,
            _context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            Box::pin(async move {
                let output_type = request.output_type.clone().unwrap_or_else(|| "json".into());
                self.trace.lock().await.push(format!(
                    "driver:{:?}:{}",
                    request.leaf_kind, request.node_id
                ));
                self.calls
                    .lock()
                    .await
                    .push((request.leaf_kind, request.node_id.clone()));
                Ok(TypedOutput::new(request.input, output_type, request.effect))
            })
        }
    }

    impl WorkflowDriver for LeafRecordingDriver {
        fn execute_agent<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }

        fn execute_tool<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }

        fn execute_artifact<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }
    }

    #[derive(Default)]
    struct OrderingJournal {
        trace: Arc<Mutex<Vec<String>>>,
    }

    fn journal_event_label(event: &JournalEvent) -> String {
        match event {
            JournalEvent::NodeStarted(event) => {
                format!("node_started:{}", event.address.node_id)
            }
            JournalEvent::NodeSettled(event) => {
                format!("node_settled:{}", event.address.node_id)
            }
            JournalEvent::NodeReused(event) => format!("node_reused:{}", event.address.node_id),
            JournalEvent::RunFinished(_) => "run_finished".into(),
        }
    }

    impl WorkflowJournal for OrderingJournal {
        fn append<'a>(
            &'a self,
            event: JournalEvent,
        ) -> WorkflowFuture<'a, Result<(), JournalError>> {
            Box::pin(async move {
                self.trace
                    .lock()
                    .await
                    .push(format!("append:{}", journal_event_label(&event)));
                Ok(())
            })
        }

        fn notify<'a>(
            &'a self,
            event: JournalEvent,
        ) -> WorkflowFuture<'a, Result<(), JournalError>> {
            Box::pin(async move {
                self.trace
                    .lock()
                    .await
                    .push(format!("notify:{}", journal_event_label(&event)));
                Ok(())
            })
        }

        fn load_recovery<'a>(
            &'a self,
            _run_id: &'a str,
        ) -> WorkflowFuture<'a, Result<Option<RecoverySnapshot>, JournalError>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[derive(Default)]
    struct SequenceDriver {
        calls: Mutex<Vec<String>>,
        fail_node: Option<String>,
    }

    impl SequenceDriver {
        fn run<'a>(
            &'a self,
            request: DriverRequest,
            _context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            let fail_node = self.fail_node.clone();
            Box::pin(async move {
                self.calls.lock().await.push(request.node_id.clone());
                if fail_node.as_deref() == Some(request.node_id.as_str()) {
                    return Err(DriverError::new("sequence_failure", "injected failure"));
                }
                Ok(TypedOutput::new(request.input, "json", request.effect))
            })
        }
    }

    impl WorkflowDriver for SequenceDriver {
        fn execute_agent<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }

        fn execute_tool<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }

        fn execute_artifact<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }
    }

    #[derive(Default)]
    struct ParallelFailureDriver {
        calls: AtomicUsize,
        slow_started: AtomicBool,
        sibling_cancelled: AtomicBool,
        queued_called: AtomicBool,
    }

    impl ParallelFailureDriver {
        fn run<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                match request.node_id.as_str() {
                    "slow" => {
                        self.slow_started.store(true, Ordering::SeqCst);
                        tokio::select! {
                            _ = context.cancellation_token.cancelled() => {
                                self.sibling_cancelled.store(true, Ordering::SeqCst);
                                Err(DriverError::new("cancelled", "sibling cancelled"))
                            }
                            _ = sleep(Duration::from_secs(30)) => {
                                Ok(TypedOutput::new(request.input, "json", request.effect))
                            }
                        }
                    }
                    "fail" => {
                        while !self.slow_started.load(Ordering::SeqCst) {
                            tokio::task::yield_now().await;
                        }
                        Err(DriverError::new("boom", "injected parallel failure"))
                    }
                    "queued" => {
                        self.queued_called.store(true, Ordering::SeqCst);
                        Ok(TypedOutput::new(request.input, "json", request.effect))
                    }
                    _ => Ok(TypedOutput::new(request.input, "json", request.effect)),
                }
            })
        }
    }

    impl WorkflowDriver for ParallelFailureDriver {
        fn execute_agent<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }

        fn execute_tool<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }

        fn execute_artifact<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.run(request, context)
        }
    }

    #[derive(Default)]
    struct ReadRecoveryDriver {
        execute_calls: AtomicUsize,
        recovery_calls: AtomicUsize,
        recovery_tool_calls: AtomicUsize,
    }

    impl ReadRecoveryDriver {
        fn execute<'a>(
            &'a self,
            request: DriverRequest,
            _context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            Box::pin(async move {
                self.execute_calls.fetch_add(1, Ordering::SeqCst);
                Ok(TypedOutput::new(request.input, "json", request.effect))
            })
        }
    }

    impl WorkflowDriver for ReadRecoveryDriver {
        fn execute_agent<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.execute(request, context)
        }

        fn execute_tool<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.execute(request, context)
        }

        fn execute_artifact<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.execute(request, context)
        }

        fn recover_read_only<'a>(
            &'a self,
            request: DriverRequest,
            _context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            Box::pin(async move {
                self.recovery_calls.fetch_add(1, Ordering::SeqCst);
                Ok(TypedOutput::new(
                    serde_json::json!({"recovered": request.input}),
                    "json",
                    EffectClass::ReadOnly,
                ))
            })
        }

        fn recover_tool<'a>(
            &'a self,
            request: DriverRequest,
            context: NodeContext,
        ) -> WorkflowFuture<'a, Result<TypedOutput, DriverError>> {
            self.recovery_tool_calls.fetch_add(1, Ordering::SeqCst);
            self.recover_read_only(request, context)
        }
    }

    #[derive(Default)]
    struct TestJournal {
        events: Mutex<Vec<JournalEvent>>,
        fail_first: AtomicBool,
        fail_notify: AtomicBool,
    }

    impl WorkflowJournal for TestJournal {
        fn append<'a>(
            &'a self,
            event: JournalEvent,
        ) -> WorkflowFuture<'a, Result<(), JournalError>> {
            Box::pin(async move {
                if self.fail_first.swap(false, Ordering::SeqCst) {
                    return Err(JournalError::new("disk", "injected"));
                }
                self.events.lock().await.push(event);
                Ok(())
            })
        }

        fn notify<'a>(
            &'a self,
            event: JournalEvent,
        ) -> WorkflowFuture<'a, Result<(), JournalError>> {
            Box::pin(async move {
                if self.fail_notify.swap(false, Ordering::SeqCst) {
                    return Err(JournalError::new("notify", "injected"));
                }
                self.events.lock().await.push(event);
                Ok(())
            })
        }

        fn load_recovery<'a>(
            &'a self,
            _run_id: &'a str,
        ) -> WorkflowFuture<'a, Result<Option<RecoverySnapshot>, JournalError>> {
            Box::pin(async { Ok(None) })
        }
    }

    fn executor(
        definition: WorkflowDefinitionV1,
        driver: Arc<TestDriver>,
        journal: Arc<TestJournal>,
    ) -> WorkflowExecutor {
        WorkflowExecutor::new(compile(definition).unwrap(), driver, journal)
    }

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            run_id: "run-1".into(),
            inputs: serde_json::json!({}),
            options: ExecutionOptions::default(),
            recovery: None,
        }
    }

    #[test]
    fn validates_duplicate_ids_and_refs() {
        let duplicate = definition(vec![
            tool("a", literal(Value::Null), EffectClass::ReadOnly),
            tool("a", literal(Value::Null), EffectClass::ReadOnly),
        ]);
        assert!(matches!(
            compile(duplicate),
            Err(ValidationError::DuplicateNodeId(_))
        ));
        let mut inputs = BTreeMap::new();
        inputs.insert("known".into(), InputSpec::default());
        let missing = WorkflowDefinitionV1 {
            inputs,
            ..definition(vec![tool(
                "a",
                ValueExpr::reference("/inputs/missing"),
                EffectClass::ReadOnly,
            )])
        };
        assert!(matches!(
            compile(missing),
            Err(ValidationError::UnknownInputReference(_))
        ));
    }

    #[test]
    fn rejects_unavailable_node_references_before_execution() {
        let self_reference = definition(vec![tool(
            "self",
            ValueExpr::reference("/nodes/self"),
            EffectClass::ReadOnly,
        )]);
        assert!(matches!(
            compile(self_reference),
            Err(ValidationError::NodeReferenceUnavailable(id)) if id == "self"
        ));

        let forward = definition(vec![
            tool(
                "first",
                ValueExpr::reference("/nodes/second"),
                EffectClass::ReadOnly,
            ),
            tool(
                "second",
                literal(serde_json::json!(2)),
                EffectClass::ReadOnly,
            ),
        ]);
        assert!(matches!(
            compile(forward),
            Err(ValidationError::NodeReferenceUnavailable(id)) if id == "second"
        ));

        let parallel_sibling = definition(vec![Node::Parallel {
            node_id: "parallel".into(),
            nodes: vec![
                tool("left", literal(serde_json::json!(1)), EffectClass::ReadOnly),
                tool(
                    "right",
                    ValueExpr::reference("/nodes/left"),
                    EffectClass::ReadOnly,
                ),
            ],
            concurrency: None,
        }]);
        assert!(matches!(
            compile(parallel_sibling),
            Err(ValidationError::NodeReferenceUnavailable(id)) if id == "left"
        ));

        let branch_leak = definition(vec![
            Node::If {
                node_id: "branch".into(),
                condition: Condition::Exists {
                    value: literal(serde_json::json!(true)),
                },
                then_body: vec![tool(
                    "then-only",
                    literal(serde_json::json!(1)),
                    EffectClass::ReadOnly,
                )],
                else_body: Vec::new(),
            },
            tool(
                "after",
                ValueExpr::reference("/nodes/then-only"),
                EffectClass::ReadOnly,
            ),
        ]);
        assert!(matches!(
            compile(branch_leak),
            Err(ValidationError::NodeReferenceUnavailable(id)) if id == "then-only"
        ));

        let valid = definition(vec![
            tool(
                "first",
                literal(serde_json::json!(1)),
                EffectClass::ReadOnly,
            ),
            tool(
                "second",
                ValueExpr::reference("/nodes/first"),
                EffectClass::ReadOnly,
            ),
        ]);
        assert!(compile(valid).is_ok());
    }

    #[test]
    fn node_json_is_tagged_by_known_kind() {
        let encoded = serde_json::to_value(tool(
            "tool-1",
            literal(serde_json::json!({"ok": true})),
            EffectClass::ReadOnly,
        ))
        .unwrap();
        assert_eq!(encoded.get("type").and_then(Value::as_str), Some("tool"));
        assert_eq!(
            encoded.get("node_id").and_then(Value::as_str),
            Some("tool-1")
        );
    }

    #[test]
    fn requires_bounded_loops_and_hash_changes() {
        let loop_node = Node::Repeat {
            node_id: "r".into(),
            body: vec![tool("a", literal(Value::Null), EffectClass::ReadOnly)],
            max_iterations: 0,
        };
        assert!(matches!(
            compile(definition(vec![loop_node])),
            Err(ValidationError::MissingIterationBudget(_))
        ));
        let over_limit = Node::Repeat {
            node_id: "over-limit".into(),
            body: vec![tool("a", literal(Value::Null), EffectClass::ReadOnly)],
            max_iterations: 2,
        };
        assert!(matches!(
            compile_with_limits(
                definition(vec![over_limit]),
                WorkflowLimits {
                    max_iterations: 1,
                    ..WorkflowLimits::default()
                }
            ),
            Err(ValidationError::IterationBudgetExceeded { .. })
        ));
        let invalid_pointer = definition(vec![tool(
            "bad-pointer",
            ValueExpr::reference("/inputs/value~2"),
            EffectClass::ReadOnly,
        )]);
        assert!(matches!(
            compile(invalid_pointer),
            Err(ValidationError::InvalidPointer(_))
        ));
        let first = compile(definition(vec![tool(
            "a",
            literal(Value::Null),
            EffectClass::ReadOnly,
        )]))
        .unwrap();
        let second = compile(definition(vec![tool(
            "b",
            literal(Value::Null),
            EffectClass::ReadOnly,
        )]))
        .unwrap();
        assert_ne!(first.definition_hash(), second.definition_hash());
    }

    #[test]
    fn metadata_when_to_use_round_trips_and_changes_canonical_hash() {
        let mut with_metadata =
            definition(vec![tool("a", literal(Value::Null), EffectClass::ReadOnly)]);
        with_metadata.meta.when_to_use = Some("用于读取项目状态".to_owned());
        let encoded = serde_json::to_value(&with_metadata).unwrap();
        assert_eq!(encoded["meta"]["whenToUse"], "用于读取项目状态");
        assert!(encoded["meta"].get("when_to_use").is_none());

        let decoded: WorkflowDefinitionV1 = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.meta.when_to_use, with_metadata.meta.when_to_use);

        let without_metadata =
            definition(vec![tool("a", literal(Value::Null), EffectClass::ReadOnly)]);
        assert_ne!(
            compile(with_metadata).unwrap().definition_hash(),
            compile(without_metadata).unwrap().definition_hash()
        );

        let mut legacy = serde_json::to_value(decoded).unwrap();
        legacy["meta"]["when_to_use"] = serde_json::json!("旧字段");
        assert!(serde_json::from_value::<WorkflowDefinitionV1>(legacy).is_err());
    }

    #[tokio::test]
    async fn executes_sequence_and_parallel_bound() {
        let body = vec![Node::Parallel {
            node_id: "p".into(),
            nodes: (0..8)
                .map(|index| {
                    tool(
                        &format!("n{index}"),
                        literal(serde_json::json!(index)),
                        EffectClass::ReadOnly,
                    )
                })
                .collect(),
            concurrency: None,
        }];
        let driver = Arc::new(TestDriver {
            delay_ms: 10,
            ..Default::default()
        });
        let journal = Arc::new(TestJournal::default());
        let result = executor(definition(body), driver.clone(), journal)
            .execute(request())
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Succeeded);
        assert!(driver.max_active.load(Ordering::SeqCst) <= 4);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 8);
    }

    #[tokio::test]
    async fn executes_agent_and_artifact_with_journal_before_driver_and_typed_outputs() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(LeafRecordingDriver {
            trace: trace.clone(),
            ..Default::default()
        });
        let journal = Arc::new(OrderingJournal {
            trace: trace.clone(),
        });
        let body = vec![
            Node::Agent(AgentNode {
                node_id: "agent".into(),
                name: "planner".into(),
                input: literal(serde_json::json!({"prompt": "hello"})),
                config: serde_json::json!({"mode": "test"}),
                output_type: Some("json".into()),
                effect: EffectClass::ReadOnly,
            }),
            Node::Artifact(ArtifactNode {
                node_id: "artifact".into(),
                name: "save-markdown".into(),
                input: literal(serde_json::json!({"markdown": "done"})),
                config: Value::Null,
                output_type: Some("markdown".into()),
                effect: EffectClass::Write,
            }),
        ];

        let result =
            WorkflowExecutor::new(compile(definition(body)).unwrap(), driver.clone(), journal)
                .execute(request())
                .await
                .unwrap();

        assert_eq!(result.status, RunStatus::Succeeded);
        assert_eq!(
            driver.calls.lock().await.clone(),
            vec![
                (DriverLeafKind::Agent, "agent".into()),
                (DriverLeafKind::Artifact, "artifact".into()),
            ]
        );
        assert_eq!(
            result.outputs["agent"].value,
            serde_json::json!({"prompt": "hello"})
        );
        assert_eq!(result.outputs["artifact"].output_type, "markdown");
        assert_eq!(result.artifacts.len(), 1);
        assert_eq!(result.artifacts[0].address.node_id, "artifact");

        let trace = trace.lock().await.clone();
        let index = |entry: &str| {
            trace
                .iter()
                .position(|item| item == entry)
                .unwrap_or_else(|| panic!("missing trace entry {entry}: {trace:?}"))
        };
        assert!(index("append:node_started:agent") < index("notify:node_started:agent"));
        assert!(index("notify:node_started:agent") < index("driver:Agent:agent"));
        assert!(index("driver:Agent:agent") < index("append:node_settled:agent"));
        assert!(index("notify:node_settled:agent") < index("append:node_started:artifact"));
        assert!(index("notify:node_started:artifact") < index("driver:Artifact:artifact"));
        assert!(index("driver:Artifact:artifact") < index("append:node_settled:artifact"));
    }

    #[tokio::test]
    async fn sequence_preserves_order_and_stops_after_failure() {
        let body = vec![
            tool(
                "first",
                literal(serde_json::json!(1)),
                EffectClass::ReadOnly,
            ),
            tool(
                "second",
                literal(serde_json::json!(2)),
                EffectClass::ReadOnly,
            ),
            tool(
                "third",
                literal(serde_json::json!(3)),
                EffectClass::ReadOnly,
            ),
        ];
        let success_driver = Arc::new(SequenceDriver::default());
        let success = WorkflowExecutor::new(
            compile(definition(body.clone())).unwrap(),
            success_driver.clone(),
            Arc::new(TestJournal::default()),
        )
        .execute(request())
        .await
        .unwrap();
        assert_eq!(success.status, RunStatus::Succeeded);
        assert_eq!(
            success_driver.calls.lock().await.clone(),
            vec!["first", "second", "third"]
        );

        let failure_driver = Arc::new(SequenceDriver {
            fail_node: Some("second".into()),
            ..Default::default()
        });
        let failure = WorkflowExecutor::new(
            compile(definition(body)).unwrap(),
            failure_driver.clone(),
            Arc::new(TestJournal::default()),
        )
        .execute(request())
        .await;
        assert!(matches!(
            failure,
            Err(WorkflowError::Driver { address, source })
                if address.node_id == "second" && source.code == "sequence_failure"
        ));
        assert_eq!(
            failure_driver.calls.lock().await.clone(),
            vec!["first", "second"]
        );
    }

    #[tokio::test]
    async fn false_branch_empty_foreach_and_composed_conditions_execute_safely() {
        let mut inputs = BTreeMap::new();
        inputs.insert(
            "value".into(),
            InputSpec {
                value_type: InputType::Integer,
                required: true,
                ..InputSpec::default()
            },
        );
        inputs.insert(
            "items".into(),
            InputSpec {
                value_type: InputType::Array,
                required: true,
                ..InputSpec::default()
            },
        );
        inputs.insert(
            "optional".into(),
            InputSpec {
                value_type: InputType::String,
                ..InputSpec::default()
            },
        );
        let condition = Condition::And {
            all: vec![
                Condition::Eq {
                    left: ValueExpr::reference("/inputs/value"),
                    right: literal(serde_json::json!(2)),
                },
                Condition::Ne {
                    left: literal(serde_json::json!(2)),
                    right: literal(serde_json::json!(3)),
                },
                Condition::Lt {
                    left: literal(serde_json::json!(1)),
                    right: literal(serde_json::json!(2)),
                },
                Condition::Lte {
                    left: literal(serde_json::json!(2)),
                    right: literal(serde_json::json!(2)),
                },
                Condition::Gt {
                    left: literal(serde_json::json!(3)),
                    right: literal(serde_json::json!(2)),
                },
                Condition::Gte {
                    left: literal(serde_json::json!(3)),
                    right: literal(serde_json::json!(3)),
                },
                Condition::Or {
                    any: vec![
                        Condition::Gt {
                            left: literal(serde_json::json!(2)),
                            right: literal(serde_json::json!(3)),
                        },
                        Condition::Not {
                            condition: Box::new(Condition::Exists {
                                value: ValueExpr::reference("/inputs/optional"),
                            }),
                        },
                    ],
                },
                // 前面的比较全部为真，最后一项刻意选择 else 分支。
                Condition::Eq {
                    left: ValueExpr::reference("/inputs/value"),
                    right: literal(serde_json::json!(3)),
                },
            ],
        };
        let body = vec![
            Node::If {
                node_id: "condition".into(),
                condition,
                then_body: vec![tool(
                    "then-only",
                    literal(serde_json::json!("then")),
                    EffectClass::ReadOnly,
                )],
                else_body: vec![tool(
                    "else-only",
                    literal(serde_json::json!("else")),
                    EffectClass::ReadOnly,
                )],
            },
            Node::Foreach {
                node_id: "empty-loop".into(),
                items: ValueExpr::reference("/inputs/items"),
                body: vec![tool(
                    "empty-item",
                    ValueExpr::reference("/iteration/item"),
                    EffectClass::ReadOnly,
                )],
                max_iterations: 1,
            },
        ];
        let driver = Arc::new(TestDriver::default());
        let mut req = request();
        req.inputs = serde_json::json!({"value": 2, "items": []});
        let result = WorkflowExecutor::new(
            compile(WorkflowDefinitionV1 {
                inputs,
                ..definition(body)
            })
            .unwrap(),
            driver.clone(),
            Arc::new(TestJournal::default()),
        )
        .execute(req)
        .await
        .unwrap();

        assert_eq!(result.status, RunStatus::Succeeded);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        let requests = driver.requests.lock().await.clone();
        assert_eq!(requests[0].0.node_id, "else-only");
        assert_eq!(
            result.outputs["condition"].value,
            serde_json::json!({"branch": "else"})
        );
        assert_eq!(
            result.outputs["empty-loop"].value,
            serde_json::json!({"iterations": 0})
        );
        assert!(
            !result
                .node_results
                .iter()
                .any(|item| item.address.node_id == "then-only")
        );
        assert!(
            !result
                .node_results
                .iter()
                .any(|item| item.address.node_id == "empty-item")
        );
    }

    #[tokio::test]
    async fn limits_reject_depth_and_node_budget_and_only_allow_tighter_runtime_limits() {
        let nested = definition(vec![Node::Sequence {
            node_id: "outer".into(),
            nodes: vec![Node::Sequence {
                node_id: "inner".into(),
                nodes: vec![tool("leaf", literal(Value::Null), EffectClass::ReadOnly)],
            }],
        }]);
        let depth_limits = WorkflowLimits {
            max_depth: 1,
            ..WorkflowLimits::default()
        };
        assert!(matches!(
            compile_with_limits(nested, depth_limits),
            Err(ValidationError::DepthExceeded { depth: 2, max: 1 })
        ));

        let node_limits = WorkflowLimits {
            max_nodes: 1,
            ..WorkflowLimits::default()
        };
        assert!(matches!(
            compile_with_limits(
                definition(vec![
                    tool("a", literal(Value::Null), EffectClass::ReadOnly),
                    tool("b", literal(Value::Null), EffectClass::ReadOnly),
                ]),
                node_limits,
            ),
            Err(ValidationError::NodeBudgetExceeded { .. })
        ));

        let body = vec![Node::Parallel {
            node_id: "parallel".into(),
            nodes: (0..6)
                .map(|index| {
                    tool(
                        &format!("node-{index}"),
                        literal(serde_json::json!(index)),
                        EffectClass::ReadOnly,
                    )
                })
                .collect(),
            concurrency: None,
        }];
        let workflow = compile(definition(body)).unwrap();
        let mut widened = request();
        widened.options.limits.max_concurrency = DEFAULT_MAX_CONCURRENCY + 1;
        assert!(!workflow.limits().allows(&widened.options.limits));
        let widened_driver = Arc::new(TestDriver::default());
        let widened_result = WorkflowExecutor::new(
            workflow.clone(),
            widened_driver.clone(),
            Arc::new(TestJournal::default()),
        )
        .execute(widened)
        .await;
        assert!(matches!(
            widened_result,
            Err(WorkflowError::LimitsNotTightened)
        ));
        assert_eq!(widened_driver.calls.load(Ordering::SeqCst), 0);

        let tightened_driver = Arc::new(TestDriver {
            delay_ms: 5,
            ..Default::default()
        });
        let mut tightened = request();
        tightened.options.limits.max_concurrency = 2;
        assert!(workflow.limits().allows(&tightened.options.limits));
        let tightened_result = WorkflowExecutor::new(
            workflow,
            tightened_driver.clone(),
            Arc::new(TestJournal::default()),
        )
        .execute(tightened)
        .await
        .unwrap();
        assert_eq!(tightened_result.status, RunStatus::Succeeded);
        assert!(tightened_driver.max_active.load(Ordering::SeqCst) <= 2);
    }

    #[tokio::test]
    async fn dynamic_node_budget_stops_a_bounded_loop_before_the_next_driver_call() {
        let workflow = compile(definition(vec![Node::Repeat {
            node_id: "repeat".into(),
            body: vec![tool(
                "item",
                ValueExpr::reference("/iteration/index"),
                EffectClass::ReadOnly,
            )],
            max_iterations: 2,
        }]))
        .unwrap();
        let driver = Arc::new(TestDriver::default());
        let mut request = request();
        request.options.limits.max_nodes = 2;
        let result =
            WorkflowExecutor::new(workflow, driver.clone(), Arc::new(TestJournal::default()))
                .execute(request)
                .await;

        assert!(matches!(result, Err(WorkflowError::NodeLimitExceeded)));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn parallel_failure_cancels_in_flight_sibling() {
        let body = vec![Node::Parallel {
            node_id: "parallel".into(),
            nodes: vec![
                tool("fail", literal(Value::Null), EffectClass::ReadOnly),
                tool("slow", literal(Value::Null), EffectClass::ReadOnly),
                tool("queued", literal(Value::Null), EffectClass::ReadOnly),
            ],
            concurrency: None,
        }];
        let driver = Arc::new(ParallelFailureDriver::default());
        let journal = Arc::new(TestJournal::default());
        let mut options = request();
        options.options.limits.max_concurrency = 2;
        let result =
            WorkflowExecutor::new(compile(definition(body)).unwrap(), driver.clone(), journal)
                .execute(options)
                .await;

        assert!(matches!(
            result,
            Err(WorkflowError::Driver { address, source })
                if address.node_id == "fail" && source.code == "boom"
        ));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        assert!(driver.sibling_cancelled.load(Ordering::SeqCst));
        assert!(!driver.queued_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn executes_if_foreach_and_repeat() {
        let mut inputs = BTreeMap::new();
        inputs.insert(
            "items".into(),
            InputSpec {
                value_type: InputType::Array,
                description: None,
                required: true,
                default: None,
            },
        );
        let body = vec![
            Node::If {
                node_id: "if".into(),
                condition: Condition::Eq {
                    left: literal(serde_json::json!(1)),
                    right: literal(serde_json::json!(1)),
                },
                then_body: vec![tool(
                    "then",
                    literal(Value::String("yes".into())),
                    EffectClass::ReadOnly,
                )],
                else_body: vec![tool(
                    "else",
                    literal(Value::String("no".into())),
                    EffectClass::ReadOnly,
                )],
            },
            Node::Foreach {
                node_id: "each".into(),
                items: ValueExpr::reference("/inputs/items"),
                body: vec![tool(
                    "item",
                    ValueExpr::reference("/iteration/item"),
                    EffectClass::ReadOnly,
                )],
                max_iterations: 3,
            },
            Node::Repeat {
                node_id: "repeat".into(),
                body: vec![tool(
                    "repeat-item",
                    ValueExpr::reference("/iteration/index"),
                    EffectClass::ReadOnly,
                )],
                max_iterations: 2,
            },
            tool(
                "after-if",
                ValueExpr::reference("/nodes/if"),
                EffectClass::ReadOnly,
            ),
            tool(
                "after-each",
                ValueExpr::reference("/nodes/each"),
                EffectClass::ReadOnly,
            ),
            tool(
                "after-repeat",
                ValueExpr::reference("/nodes/repeat"),
                EffectClass::ReadOnly,
            ),
        ];
        let driver = Arc::new(TestDriver::default());
        let journal = Arc::new(TestJournal::default());
        let mut req = request();
        req.inputs = serde_json::json!({"items": ["a", "b"]});
        let result = WorkflowExecutor::new(
            compile(WorkflowDefinitionV1 {
                inputs,
                ..definition(body)
            })
            .unwrap(),
            driver.clone(),
            journal,
        )
        .execute(req)
        .await
        .unwrap();
        assert_eq!(result.status, RunStatus::Succeeded);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 8);

        let result_for = |node_id: &str| {
            result
                .node_results
                .iter()
                .find(|item| item.address.node_id == node_id && item.address.invocation.is_empty())
                .unwrap_or_else(|| panic!("missing node result for {node_id}"))
        };
        assert_eq!(
            result_for("if").output.as_ref().unwrap().value,
            serde_json::json!({"branch": "then"})
        );
        assert_eq!(
            result_for("each").output.as_ref().unwrap().value,
            serde_json::json!({"iterations": 2})
        );
        assert_eq!(
            result_for("repeat").output.as_ref().unwrap().value,
            serde_json::json!({"iterations": 2})
        );
        assert!(result_for("then").output.is_some());
        assert!(
            result
                .node_results
                .iter()
                .all(|item| item.address.node_id != "else")
        );

        let requests = driver.requests.lock().await.clone();
        let inputs_for = |node_id: &str| {
            requests
                .iter()
                .filter(|(address, _)| address.node_id == node_id)
                .map(|(_, input)| input.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            inputs_for("item"),
            vec![serde_json::json!("a"), serde_json::json!("b")]
        );
        assert_eq!(
            inputs_for("repeat-item"),
            vec![serde_json::json!(0), serde_json::json!(1)]
        );
        assert_eq!(
            inputs_for("after-if"),
            vec![serde_json::json!({"branch": "then"})]
        );
        assert_eq!(
            inputs_for("after-each"),
            vec![serde_json::json!({"iterations": 2})]
        );
        assert_eq!(
            inputs_for("after-repeat"),
            vec![serde_json::json!({"iterations": 2})]
        );
    }

    #[tokio::test]
    async fn nested_loop_driver_context_keeps_full_node_address() {
        let mut inputs = BTreeMap::new();
        inputs.insert(
            "outer".into(),
            InputSpec {
                value_type: InputType::Array,
                description: None,
                required: true,
                default: None,
            },
        );
        let body = vec![Node::Foreach {
            node_id: "outer-loop".into(),
            items: ValueExpr::reference("/inputs/outer"),
            max_iterations: 2,
            body: vec![Node::Foreach {
                node_id: "inner-loop".into(),
                items: literal(serde_json::json!(["x", "y"])),
                max_iterations: 2,
                body: vec![tool(
                    "nested-leaf",
                    ValueExpr::reference("/iteration/item"),
                    EffectClass::ReadOnly,
                )],
            }],
        }];
        let driver = Arc::new(TestDriver::default());
        let journal = Arc::new(TestJournal::default());
        let mut req = request();
        req.inputs = serde_json::json!({"outer": ["a", "b"]});
        let result = WorkflowExecutor::new(
            compile(WorkflowDefinitionV1 {
                inputs,
                ..definition(body)
            })
            .unwrap(),
            driver.clone(),
            journal,
        )
        .execute(req)
        .await
        .unwrap();
        assert_eq!(result.status, RunStatus::Succeeded);

        let mut nested = driver
            .addresses
            .lock()
            .await
            .iter()
            .filter(|address| address.node_id == "nested-leaf")
            .map(|address| address.invocation.clone())
            .collect::<Vec<_>>();
        nested.sort();
        assert_eq!(nested, vec![vec![0, 0], vec![0, 1], vec![1, 0], vec![1, 1]]);
    }

    #[tokio::test]
    async fn journal_failure_prevents_driver_call() {
        let driver = Arc::new(TestDriver::default());
        let journal = Arc::new(TestJournal {
            fail_first: AtomicBool::new(true),
            ..Default::default()
        });
        let result = executor(
            definition(vec![tool("a", literal(Value::Null), EffectClass::ReadOnly)]),
            driver.clone(),
            journal,
        )
        .execute(request())
        .await;
        assert!(result.is_err());
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn notify_failure_keeps_append_fact_and_prevents_driver_call() {
        let driver = Arc::new(TestDriver::default());
        let journal = Arc::new(TestJournal {
            fail_notify: AtomicBool::new(true),
            ..Default::default()
        });
        let result = executor(
            definition(vec![tool("a", literal(Value::Null), EffectClass::ReadOnly)]),
            driver.clone(),
            journal.clone(),
        )
        .execute(request())
        .await;

        assert!(matches!(
            result,
            Err(WorkflowError::Journal(error)) if error.code == "notify"
        ));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
        let events = journal.events.lock().await.clone();
        assert!(events.iter().any(|event| matches!(
            event,
            JournalEvent::NodeStarted(started) if started.address.node_id == "a"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            JournalEvent::RunFinished(finished)
                if finished.status == RunStatus::Failed
                    && finished.error.as_ref().map(|error| error.code.as_str())
                        == Some("journal_error")
        )));
        let failed_run = events
            .iter()
            .find_map(|event| match event {
                JournalEvent::RunFinished(finished) => Some(finished),
                _ => None,
            })
            .expect("失败运行应有终态事件");
        let wire = serde_json::to_value(failed_run).expect("失败终态应可序列化");
        assert_eq!(wire["error"]["code"], "journal_error");
        assert!(
            !wire["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty()
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            JournalEvent::NodeSettled(settled) if settled.address.node_id == "a"
        )));
    }

    #[tokio::test]
    async fn recovery_reuses_completed_and_rejects_mutating_started() {
        let driver = Arc::new(TestDriver::default());
        let journal = Arc::new(TestJournal::default());
        let workflow = compile(definition(vec![
            tool("a", literal(serde_json::json!(1)), EffectClass::ReadOnly),
            tool("b", literal(serde_json::json!(2)), EffectClass::Write),
        ]))
        .unwrap();
        let input_hash = hash_inputs(&serde_json::json!({})).unwrap();
        let recovery = RecoverySnapshot {
            run_id: "run-1".into(),
            definition_hash: workflow.definition_hash().into(),
            input_hash,
            completed: vec![CompletedNode {
                address: NodeAddress {
                    node_id: "a".into(),
                    invocation: vec![],
                },
                output: TypedOutput::new(serde_json::json!(1), "json", EffectClass::ReadOnly),
            }],
            started: vec![StartedNode {
                address: NodeAddress {
                    node_id: "b".into(),
                    invocation: vec![],
                },
                effect: EffectClass::Write,
            }],
        };
        let mut req = request();
        req.recovery = Some(recovery);
        let result = WorkflowExecutor::new(workflow, driver.clone(), journal)
            .execute(req)
            .await;
        assert!(matches!(result, Err(WorkflowError::Indeterminate { .. })));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn recovery_reuses_completed_output_for_later_reference() {
        let workflow = compile(definition(vec![
            tool("a", literal(serde_json::json!(1)), EffectClass::ReadOnly),
            tool("b", ValueExpr::reference("/nodes/a"), EffectClass::ReadOnly),
        ]))
        .unwrap();
        let definition_hash = workflow.definition_hash().to_owned();
        let driver = Arc::new(TestDriver::default());
        let journal = Arc::new(TestJournal::default());
        let mut req = request();
        req.recovery = Some(RecoverySnapshot {
            run_id: "run-1".into(),
            definition_hash,
            input_hash: hash_inputs(&serde_json::json!({})).unwrap(),
            completed: vec![CompletedNode {
                address: NodeAddress {
                    node_id: "a".into(),
                    invocation: Vec::new(),
                },
                output: TypedOutput::new(serde_json::json!(1), "json", EffectClass::ReadOnly),
            }],
            started: Vec::new(),
        });

        let result = WorkflowExecutor::new(workflow, driver.clone(), journal.clone())
            .execute(req)
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Succeeded);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.outputs["a"].value, serde_json::json!(1));
        assert_eq!(result.outputs["b"].value, serde_json::json!(1));
        assert!(result.node_results.iter().any(|item| {
            item.address.node_id == "a"
                && item.status == NodeStatus::Succeeded
                && item
                    .output
                    .as_ref()
                    .is_some_and(|output| output.value == serde_json::json!(1))
        }));
        let requests = driver.requests.lock().await.clone();
        assert_eq!(
            requests,
            vec![(
                NodeAddress {
                    node_id: "b".into(),
                    invocation: Vec::new(),
                },
                serde_json::json!(1),
            )]
        );
        let events = journal.events.lock().await.clone();
        assert!(events.iter().any(|event| matches!(
            event,
            JournalEvent::NodeReused(reused) if reused.address.node_id == "a"
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            JournalEvent::NodeStarted(started) if started.address.node_id == "a"
        )));
    }

    #[tokio::test]
    async fn recovery_reuses_completed_write_without_replaying_driver() {
        let workflow = compile(definition(vec![
            tool("write", literal(serde_json::json!(1)), EffectClass::Write),
            tool(
                "read",
                ValueExpr::reference("/nodes/write"),
                EffectClass::ReadOnly,
            ),
        ]))
        .unwrap();
        let definition_hash = workflow.definition_hash().to_owned();
        let driver = Arc::new(TestDriver::default());
        let journal = Arc::new(TestJournal::default());
        let mut req = request();
        req.recovery = Some(RecoverySnapshot {
            run_id: "run-1".into(),
            definition_hash,
            input_hash: hash_inputs(&serde_json::json!({})).unwrap(),
            completed: vec![CompletedNode {
                address: NodeAddress {
                    node_id: "write".into(),
                    invocation: Vec::new(),
                },
                output: TypedOutput::new(serde_json::json!(1), "json", EffectClass::Write),
            }],
            started: Vec::new(),
        });

        let result = WorkflowExecutor::new(workflow, driver.clone(), journal.clone())
            .execute(req)
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Succeeded);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.outputs["read"].value, serde_json::json!(1));

        let events = journal.events.lock().await.clone();
        assert!(events.iter().any(|event| matches!(
            event,
            JournalEvent::NodeReused(reused) if reused.address.node_id == "write"
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            JournalEvent::NodeStarted(started) if started.address.node_id == "write"
        )));
    }

    #[tokio::test]
    async fn recovery_uses_explicit_read_only_recovery_without_replaying_driver() {
        let workflow = compile(definition(vec![tool(
            "read",
            literal(serde_json::json!(1)),
            EffectClass::ReadOnly,
        )]))
        .unwrap();
        let driver = Arc::new(ReadRecoveryDriver::default());
        let journal = Arc::new(TestJournal::default());
        let mut req = request();
        req.recovery = Some(RecoverySnapshot {
            run_id: "run-1".into(),
            definition_hash: workflow.definition_hash().to_owned(),
            input_hash: hash_inputs(&serde_json::json!({})).unwrap(),
            completed: Vec::new(),
            started: vec![StartedNode {
                address: NodeAddress {
                    node_id: "read".into(),
                    invocation: Vec::new(),
                },
                effect: EffectClass::ReadOnly,
            }],
        });

        let result = WorkflowExecutor::new(workflow, driver.clone(), journal)
            .execute(req)
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Succeeded);
        assert_eq!(driver.execute_calls.load(Ordering::SeqCst), 0);
        assert_eq!(driver.recovery_calls.load(Ordering::SeqCst), 1);
        assert_eq!(driver.recovery_tool_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            result.outputs["read"].value,
            serde_json::json!({"recovered": 1})
        );
    }

    #[tokio::test]
    async fn recovery_rejects_unknown_effect_and_changed_definition() {
        let driver = Arc::new(TestDriver::default());
        let journal = Arc::new(TestJournal::default());
        let workflow = compile(definition(vec![tool(
            "a",
            literal(Value::Null),
            EffectClass::Unknown,
        )]))
        .unwrap();
        let input_hash = hash_inputs(&serde_json::json!({})).unwrap();
        let mut recovery_request = request();
        recovery_request.recovery = Some(RecoverySnapshot {
            run_id: "run-1".into(),
            definition_hash: workflow.definition_hash().into(),
            input_hash: input_hash.clone(),
            completed: Vec::new(),
            started: vec![StartedNode {
                address: NodeAddress {
                    node_id: "a".into(),
                    invocation: Vec::new(),
                },
                effect: EffectClass::Unknown,
            }],
        });
        let result = WorkflowExecutor::new(workflow.clone(), driver.clone(), journal.clone())
            .execute(recovery_request)
            .await;
        assert!(matches!(
            result,
            Err(WorkflowError::Indeterminate {
                effect: EffectClass::Unknown,
                ..
            })
        ));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);

        let mut changed = request();
        changed.recovery = Some(RecoverySnapshot {
            run_id: "run-1".into(),
            definition_hash: "sha256:changed".into(),
            input_hash: input_hash.clone(),
            completed: Vec::new(),
            started: Vec::new(),
        });
        let result = WorkflowExecutor::new(workflow.clone(), driver.clone(), journal.clone())
            .execute(changed)
            .await;
        assert!(matches!(
            result,
            Err(WorkflowError::DefinitionChanged { .. })
        ));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);

        let mut input_changed = request();
        input_changed.recovery = Some(RecoverySnapshot {
            run_id: "run-1".into(),
            definition_hash: workflow.definition_hash().into(),
            input_hash: "sha256:changed-input".into(),
            completed: Vec::new(),
            started: Vec::new(),
        });
        let result = WorkflowExecutor::new(workflow, driver.clone(), journal)
            .execute(input_changed)
            .await;
        assert!(matches!(result, Err(WorkflowError::InputChanged { .. })));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn recovery_rejects_duplicate_conflicts_unknown_nodes_and_contract_mismatches() {
        let workflow = compile(definition(vec![
            Node::Sequence {
                node_id: "group".into(),
                nodes: Vec::new(),
            },
            tool("read", literal(serde_json::json!(1)), EffectClass::ReadOnly),
            tool("write", literal(serde_json::json!(2)), EffectClass::Write),
            Node::Repeat {
                node_id: "loop".into(),
                body: vec![tool(
                    "nested",
                    literal(serde_json::json!(3)),
                    EffectClass::ReadOnly,
                )],
                max_iterations: 2,
            },
        ]))
        .unwrap();
        let input_hash = hash_inputs(&serde_json::json!({})).unwrap();
        let address = |node_id: &str| NodeAddress {
            node_id: node_id.into(),
            invocation: Vec::new(),
        };
        let address_at = |node_id: &str, invocation: Vec<u32>| NodeAddress {
            node_id: node_id.into(),
            invocation,
        };
        let snapshot =
            |completed: Vec<CompletedNode>, started: Vec<StartedNode>| RecoverySnapshot {
                run_id: "run-1".into(),
                definition_hash: workflow.definition_hash().into(),
                input_hash: input_hash.clone(),
                completed,
                started,
            };
        let cases = vec![
            snapshot(
                vec![
                    CompletedNode {
                        address: address("read"),
                        output: TypedOutput::new(
                            serde_json::json!(1),
                            "json",
                            EffectClass::ReadOnly,
                        ),
                    },
                    CompletedNode {
                        address: address("read"),
                        output: TypedOutput::new(
                            serde_json::json!(1),
                            "json",
                            EffectClass::ReadOnly,
                        ),
                    },
                ],
                Vec::new(),
            ),
            snapshot(
                vec![CompletedNode {
                    address: address("read"),
                    output: TypedOutput::new(serde_json::json!(1), "json", EffectClass::ReadOnly),
                }],
                vec![StartedNode {
                    address: address("read"),
                    effect: EffectClass::ReadOnly,
                }],
            ),
            snapshot(
                Vec::new(),
                vec![StartedNode {
                    address: address("write"),
                    effect: EffectClass::ReadOnly,
                }],
            ),
            snapshot(
                vec![CompletedNode {
                    address: address("write"),
                    output: TypedOutput::new(serde_json::json!(2), "json", EffectClass::ReadOnly),
                }],
                Vec::new(),
            ),
            snapshot(
                vec![CompletedNode {
                    address: address("group"),
                    output: TypedOutput::new(
                        serde_json::json!({"children": []}),
                        "json",
                        EffectClass::ReadOnly,
                    ),
                }],
                Vec::new(),
            ),
            snapshot(
                vec![CompletedNode {
                    address: address_at("nested", vec![2]),
                    output: TypedOutput::new(serde_json::json!(3), "json", EffectClass::ReadOnly),
                }],
                Vec::new(),
            ),
            snapshot(
                vec![CompletedNode {
                    address: address_at("nested", vec![0, 0]),
                    output: TypedOutput::new(serde_json::json!(3), "json", EffectClass::ReadOnly),
                }],
                Vec::new(),
            ),
            snapshot(
                vec![CompletedNode {
                    address: address("missing"),
                    output: TypedOutput::new(
                        serde_json::json!(null),
                        "json",
                        EffectClass::ReadOnly,
                    ),
                }],
                Vec::new(),
            ),
        ];

        for recovery in cases {
            let driver = Arc::new(TestDriver::default());
            let mut request = request();
            request.recovery = Some(recovery);
            let result = WorkflowExecutor::new(
                workflow.clone(),
                driver.clone(),
                Arc::new(TestJournal::default()),
            )
            .execute(request)
            .await;
            assert!(matches!(result, Err(WorkflowError::InvalidRecovery(_))));
            assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn write_nodes_are_serialized_inside_parallel() {
        let body = vec![Node::Parallel {
            node_id: "p".into(),
            nodes: vec![
                tool("write-a", literal(serde_json::json!(1)), EffectClass::Write),
                tool("write-b", literal(serde_json::json!(2)), EffectClass::Write),
            ],
            concurrency: None,
        }];
        let driver = Arc::new(TestDriver {
            delay_ms: 10,
            ..Default::default()
        });
        let journal = Arc::new(TestJournal::default());
        let result = executor(definition(body), driver.clone(), journal)
            .execute(request())
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Succeeded);
        assert_eq!(driver.max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn duration_and_output_limits_stop_execution() {
        let driver = Arc::new(TestDriver {
            delay_ms: 50,
            ..Default::default()
        });
        let journal = Arc::new(TestJournal::default());
        let mut duration_request = request();
        duration_request.options.limits.max_duration_ms = 5;
        let result = executor(
            definition(vec![tool(
                "slow",
                literal(Value::Null),
                EffectClass::ReadOnly,
            )]),
            driver.clone(),
            journal.clone(),
        )
        .execute(duration_request)
        .await;
        assert!(matches!(result, Err(WorkflowError::DurationExceeded)));

        let mut output_request = request();
        output_request.options.limits.max_output_bytes = 1;
        let result = executor(
            definition(vec![tool(
                "large",
                literal(serde_json::json!({"value": "too large"})),
                EffectClass::ReadOnly,
            )]),
            Arc::new(TestDriver::default()),
            journal,
        )
        .execute(output_request)
        .await;
        assert!(matches!(result, Err(WorkflowError::OutputLimitExceeded)));
    }

    #[tokio::test]
    async fn cancellation_reaches_driver() {
        let driver = Arc::new(TestDriver {
            delay_ms: 300,
            ..Default::default()
        });
        let journal = Arc::new(TestJournal::default());
        let token = CancellationToken::new();
        let mut req = request();
        req.options.cancellation_token = token.clone();
        let executor = Arc::new(executor(
            definition(vec![tool("a", literal(Value::Null), EffectClass::ReadOnly)]),
            driver.clone(),
            journal.clone(),
        ));
        let task = tokio::spawn({
            let executor = executor.clone();
            async move { executor.execute(req).await }
        });
        sleep(Duration::from_millis(20)).await;
        token.cancel();
        let result = task.await.unwrap().expect("取消应形成已结算结果");
        assert_eq!(result.status, RunStatus::Cancelled);
        assert!(
            driver.cancelled.load(Ordering::SeqCst) || driver.calls.load(Ordering::SeqCst) == 0
        );
        let events = journal.events.lock().await.clone();
        assert!(events.iter().any(|event| matches!(
            event,
            JournalEvent::NodeSettled(settled)
                if settled.status == NodeStatus::Cancelled
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            JournalEvent::RunFinished(finished)
                if finished.status == RunStatus::Cancelled
        )));
    }
}
