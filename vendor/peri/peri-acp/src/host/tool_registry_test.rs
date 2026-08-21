use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::{BaseTool, ToolContext};
use serde_json::{json, Value};

use super::SessionToolRegistry;

struct CwdTool {
    name: &'static str,
    cwd: String,
}

#[async_trait]
impl BaseTool for CwdTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "A cwd-bound deferred tool"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "x-cwd": self.cwd,
        })
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.cwd.clone())
    }
}

fn install(registry: &SessionToolRegistry, tool: Arc<dyn BaseTool>) {
    registry
        .shared_tools
        .write()
        .insert(tool.name().to_string(), Arc::clone(&tool));
    let index = match Arc::clone(&registry.tool_search_index)
        .downcast_arc::<peri_middlewares::tool_search::ToolSearchIndex>()
    {
        Ok(index) => index,
        Err(_) => panic!("production SessionToolRegistry must use ToolSearchIndex"),
    };
    index.build(vec![tool]);
}

#[test]
fn session_registries_isolate_search_execution_and_cwd() {
    let session_a = SessionToolRegistry::new();
    let session_b = SessionToolRegistry::new();
    let agent_a: Arc<dyn BaseTool> = Arc::new(CwdTool {
        name: "AgentTool",
        cwd: "/project-a".to_string(),
    });
    let read_b: Arc<dyn BaseTool> = Arc::new(CwdTool {
        name: "Read",
        cwd: "/project-b".to_string(),
    });
    install(&session_a, Arc::clone(&agent_a));
    install(&session_b, Arc::clone(&read_b));

    let index_a = match Arc::clone(&session_a.tool_search_index)
        .downcast_arc::<peri_middlewares::tool_search::ToolSearchIndex>()
    {
        Ok(index) => index,
        Err(_) => panic!("session A must use ToolSearchIndex"),
    };
    let index_b = match Arc::clone(&session_b.tool_search_index)
        .downcast_arc::<peri_middlewares::tool_search::ToolSearchIndex>()
    {
        Ok(index) => index,
        Err(_) => panic!("session B must use ToolSearchIndex"),
    };
    assert_eq!(index_a.search("select:AgentTool", 10).len(), 1);
    assert!(index_b.search("select:AgentTool", 10).is_empty());
    assert_eq!(
        index_b.search("select:Read", 10)[0].parameters["x-cwd"],
        "/project-b"
    );

    // Search and ExecuteExtraTool resolve from the same session snapshot.
    let indexed = index_a.get_tool("AgentTool").expect("agent tool indexed");
    let executable = session_a
        .shared_tools
        .read()
        .get("AgentTool")
        .cloned()
        .expect("agent tool executable");
    assert!(Arc::ptr_eq(&indexed, &executable));
}

#[test]
fn reset_drops_previous_tools_before_next_session_turn() {
    let registry = SessionToolRegistry::new();
    install(
        &registry,
        Arc::new(CwdTool {
            name: "AgentTool",
            cwd: "/project-a".to_string(),
        }),
    );
    let index = match Arc::clone(&registry.tool_search_index)
        .downcast_arc::<peri_middlewares::tool_search::ToolSearchIndex>()
    {
        Ok(index) => index,
        Err(_) => panic!("session registry must use ToolSearchIndex"),
    };
    assert_eq!(index.search("select:AgentTool", 10).len(), 1);

    // A new turn for the same session starts from an empty snapshot.
    registry.reset_for_turn();
    assert!(index.search("select:AgentTool", 10).is_empty());
    assert!(registry.shared_tools.read().is_empty());
}
