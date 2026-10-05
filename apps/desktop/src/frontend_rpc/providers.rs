//! ZCode Provider Settings/Model Selection facade 的桌面实现。
//!
//! 该模块只做 RPC 参数、事件和 Runtime 边界适配；供应商事实保存在
//! `crate::providers` 的唯一 `providers.json` 中，不在这里缓存第二份配置。

use super::dispatch::{EventCallback, GatewayContext, RpcError, RpcFuture, Subscription};
use crate::providers;
use keencode_model::{Message, MessageRole, ModelProvider, ModelRequest};
use serde_json::{Value, json};
use tauri::{Emitter, Listener};

const PROVIDER_EVENT: &str = "keencode://provider-settings";
const MODEL_EVENT: &str = "keencode://model-selection";

/// Provider Settings 与 Model Selection 共用的业务调用入口。
pub(crate) fn call(ctx: GatewayContext, channel: String, method: String, args: Value) -> RpcFuture {
    Box::pin(async move {
        match channel.as_str() {
            "provider-settings" => provider_settings_call(&ctx, &method, args).await,
            "model-selection" => model_selection_call(&ctx, &method, args),
            _ => Err(RpcError::new(
                "rpc.unknownChannel",
                format!("未知 Provider channel：{channel}"),
            )),
        }
    })
}

/// 事件使用真实 Tauri listener；dispose/drop 会解除对应 listener。
pub(crate) fn listen(
    ctx: &GatewayContext,
    channel: &str,
    event: &str,
    _args: Value,
    callback: EventCallback,
) -> Result<Subscription, RpcError> {
    let event_name = match (channel, event) {
        ("provider-settings", "onDidChange") => PROVIDER_EVENT,
        ("model-selection", "onDidChange") => MODEL_EVENT,
        _ => {
            return Err(RpcError::new(
                "rpc.unknownMethod",
                format!("未知或未接通事件：{channel}.{event}"),
            ));
        }
    };
    let app = ctx.app.clone();
    let listener = app.listen(event_name, move |event| {
        let payload = serde_json::from_str::<Value>(event.payload())
            .unwrap_or_else(|_| Value::String(event.payload().to_owned()));
        let _ = callback(payload);
    });
    Ok(Subscription::new(move || app.unlisten(listener)))
}

async fn provider_settings_call(
    ctx: &GatewayContext,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    let app = &ctx.app;
    match method {
        "getView" => providers::facade_settings_view(app).map_err(provider_error),
        "refresh" => {
            let view = providers::facade_settings_view(app).map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "createPersonalProvider" => {
            let input = positional_object(args, 0, "createPersonalProvider")?;
            let response =
                providers::facade_create_provider(app, &input).map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(response)
        }
        "resolveModelConfig" => {
            let input = positional_object(args, 0, "resolveModelConfig")?;
            Ok(providers::facade_resolve_model_config(app, &input).map_err(provider_error)?)
        }
        "savePersonalProviderOverlay" => {
            let (provider_id, config, metadata) = provider_overlay_args(args)?;
            let view = providers::facade_save_provider_overlay(
                app,
                &provider_id,
                &config,
                metadata.as_ref(),
            )
            .map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "deletePersonalProvider" => {
            let provider_id = positional_string(args, 0, "deletePersonalProvider")?;
            let view =
                providers::facade_delete_provider(app, &provider_id).map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "reorderPersonalProviders" => {
            let ids = positional_value(args, 0, "reorderPersonalProviders")?;
            let view = providers::facade_reorder_providers(app, &ids).map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "reorderPersonalModels" => {
            let values = positional_values(args, 2, "reorderPersonalModels")?;
            let provider_id = value_string(&values[0], "providerId")?;
            let view = providers::facade_reorder_models(app, &provider_id, &values[1])
                .map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "addPersonalModel" => {
            let values = positional_values(args, 3, "addPersonalModel")?;
            let provider_id = value_string(&values[0], "providerId")?;
            let model_id = value_string(&values[1], "modelId")?;
            let view = providers::facade_add_model(
                app,
                &provider_id,
                &model_id,
                &values[2],
                values.get(3).and_then(Value::as_bool),
            )
            .map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "renamePersonalModel" => {
            let values = positional_values(args, 3, "renamePersonalModel")?;
            let provider_id = value_string(&values[0], "providerId")?;
            let current = value_string(&values[1], "currentModelId")?;
            let next = value_string(&values[2], "nextModelId")?;
            let view = providers::facade_rename_model(app, &provider_id, &current, &next)
                .map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "deletePersonalModel" => {
            let values = positional_values(args, 2, "deletePersonalModel")?;
            let provider_id = value_string(&values[0], "providerId")?;
            let model_id = value_string(&values[1], "modelId")?;
            let view = providers::facade_delete_model(app, &provider_id, &model_id)
                .map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "savePersonalModelDraft" => {
            let input = positional_object(args, 0, "savePersonalModelDraft")?;
            let view = providers::facade_save_model_draft(app, &input).map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "setPersonalModelEnabled" => {
            let values = positional_values(args, 3, "setPersonalModelEnabled")?;
            let provider_id = value_string(&values[0], "providerId")?;
            let model_id = value_string(&values[1], "modelId")?;
            let enabled = values[2]
                .as_bool()
                .ok_or_else(|| RpcError::new("rpc.invalidArguments", "enabled 必须是布尔值"))?;
            let view = providers::facade_set_model_enabled(app, &provider_id, &model_id, enabled)
                .map_err(provider_error)?;
            sync_runtime(app)?;
            Ok(view)
        }
        "testModelConnectivity" => {
            let input = positional_object(args, 0, "testModelConnectivity")?;
            test_model_connectivity(ctx, input).await
        }
        _ => Err(RpcError::new(
            "rpc.unknownMethod",
            format!("未知 provider-settings 方法：{method}"),
        )),
    }
}

fn model_selection_call(
    ctx: &GatewayContext,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    if method != "getView" {
        return Err(RpcError::new(
            "rpc.unknownMethod",
            format!("未知 model-selection 方法：{method}"),
        ));
    }
    let input = optional_object(args, "model-selection.getView")?;
    providers::facade_model_selection_view(&ctx.app, input.as_ref())
        .map_err(|error| RpcError::new("model-selection.error", redact_error(&error.to_string())))
}

async fn test_model_connectivity(ctx: &GatewayContext, input: Value) -> Result<Value, RpcError> {
    if input
        .get("workspacePath")
        .and_then(Value::as_str)
        .is_none_or(|path| path.trim().is_empty())
    {
        return Err(RpcError::new(
            "rpc.invalidArguments",
            "workspacePath 必须是非空字符串",
        ));
    }
    let provider_id = input
        .get("providerId")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new("rpc.invalidArguments", "providerId 必须是字符串"))?;
    let model_id = input
        .get("modelId")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new("rpc.invalidArguments", "modelId 必须是字符串"))?;
    let settings = providers::facade_settings_view(&ctx.app)
        .map_err(|error| RpcError::new("provider-unavailable", redact_error(&error.to_string())))?;
    let provider = settings
        .get("providers")
        .and_then(Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("providerId").and_then(Value::as_str) == Some(provider_id)
            })
        });
    let Some(provider) = provider else {
        return Ok(json!({
            "success": false,
            "error": {
                "code": "provider-unavailable",
                "message": "This provider is currently unavailable for connectivity testing."
            }
        }));
    };
    if provider.get("enabled") != Some(&Value::Bool(true))
        || provider.get("executable") != Some(&Value::Bool(true))
    {
        return Ok(json!({
            "success": false,
            "error": {
                "code": "provider-unavailable",
                "message": "This provider is currently unavailable for connectivity testing."
            }
        }));
    }
    let model_ready = provider
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| {
            models
                .iter()
                .find(|model| model.get("modelId").and_then(Value::as_str) == Some(model_id))
        });
    if model_ready.is_none_or(|model| {
        model.get("enabled") != Some(&Value::Bool(true))
            || model.get("executable") != Some(&Value::Bool(true))
    }) {
        return Ok(json!({
            "success": false,
            "error": {
                "code": "model-unavailable",
                "message": "This model is currently unavailable for connectivity testing."
            }
        }));
    }
    let runtime = crate::require_owned_runtime(&ctx.app)
        .map_err(|error| RpcError::new("provider-unavailable", redact_error(&error)))?;
    let resolved = match runtime.provider_registry().resolve(provider_id, model_id) {
        Ok(provider) => provider,
        Err(error) => {
            return Ok(json!({
                "success": false,
                "error": {
                    "code": "model-unavailable",
                    "message": redact_error(&error.to_string())
                }
            }));
        }
    };
    let request = ModelRequest::new(
        model_id.to_owned(),
        vec![Message::text(MessageRole::User, "Reply with KC_OK only.")],
    );
    match resolved.complete(request).await {
        Ok(_) => Ok(json!({"success": true})),
        Err(error) => Ok(json!({
            "success": false,
            "error": {
                "code": connectivity_error_code(&error),
                "message": redact_error(&error.to_string())
            }
        })),
    }
}

fn sync_runtime(app: &tauri::AppHandle) -> Result<(), RpcError> {
    let runtime = crate::require_owned_runtime(app)
        .map_err(|error| RpcError::new("provider-unavailable", redact_error(&error)))?;
    runtime
        .reload_providers(app)
        .map(|_| ())
        .map_err(|error| RpcError::new("provider-unavailable", redact_error(&error.to_string())))?;
    let view = providers::facade_settings_view(app).map_err(provider_error)?;
    // 事件是广播面，不能把 settings getter 中用于预填的明文 Key 扩散给所有窗口。
    let mut event_view = view;
    scrub_provider_credentials(&mut event_view);
    app.emit(PROVIDER_EVENT, event_view)
        .map_err(|error| RpcError::new("provider-settings.error", error.to_string()))?;
    let mut selection =
        providers::facade_model_selection_view(app, None).map_err(provider_error)?;
    scrub_provider_credentials(&mut selection);
    app.emit(MODEL_EVENT, selection)
        .map_err(|error| RpcError::new("model-selection.error", error.to_string()))
}

fn connectivity_error_code(error: &keencode_model::ModelError) -> &'static str {
    match error {
        keencode_model::ModelError::ModelNotFound { .. } => "model-unavailable",
        _ => "provider-unavailable",
    }
}

fn redact_error(error: &str) -> String {
    keencode_model::redact_error_secrets(error)
}

fn provider_error(error: anyhow::Error) -> RpcError {
    RpcError::new("provider-settings.error", redact_error(&error.to_string()))
}

/// 广播事件不得携带 getter 用于表单预填的明文 API Key。
pub(crate) fn scrub_provider_credentials(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("apiKey");
            for child in object.values_mut() {
                scrub_provider_credentials(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                scrub_provider_credentials(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn positional_object(args: Value, index: usize, method: &str) -> Result<Value, RpcError> {
    match args {
        Value::Object(_) => Ok(args),
        Value::Array(values) if values.is_empty() && index == 0 => Ok(json!({})),
        Value::Array(values) if values.get(index).is_some_and(Value::is_null) => Ok(json!({})),
        Value::Array(values) => values
            .get(index)
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| {
                RpcError::new("rpc.invalidArguments", format!("{method} 参数必须是对象"))
            }),
        _ => Err(RpcError::new(
            "rpc.invalidArguments",
            format!("{method} 参数必须是对象"),
        )),
    }
}

fn optional_object(args: Value, method: &str) -> Result<Option<Value>, RpcError> {
    match args {
        Value::Object(_) => Ok(Some(args)),
        Value::Array(values) if values.is_empty() => Ok(None),
        Value::Array(values) if values.len() == 1 && values[0].is_null() => Ok(None),
        Value::Array(values) if values.len() == 1 && values[0].is_object() => {
            Ok(Some(values[0].clone()))
        }
        _ => Err(RpcError::new(
            "rpc.invalidArguments",
            format!("{method} 参数必须是对象"),
        )),
    }
}

fn provider_overlay_args(args: Value) -> Result<(String, Value, Option<Value>), RpcError> {
    let values = positional_values(args, 2, "savePersonalProviderOverlay")?;
    Ok((
        value_string(&values[0], "providerId")?,
        values[1].clone(),
        values.get(2).cloned(),
    ))
}

fn positional_values(args: Value, minimum: usize, method: &str) -> Result<Vec<Value>, RpcError> {
    match args {
        Value::Array(values) if values.len() >= minimum => Ok(values),
        Value::Object(object) if minimum == 1 => Ok(vec![Value::Object(object)]),
        _ => Err(RpcError::new(
            "rpc.invalidArguments",
            format!("{method} 参数数量不足"),
        )),
    }
}

fn positional_value(args: Value, index: usize, method: &str) -> Result<Value, RpcError> {
    positional_values(args, index + 1, method).map(|values| values[index].clone())
}

fn positional_string(args: Value, index: usize, method: &str) -> Result<String, RpcError> {
    let value = positional_value(args, index, method)?;
    value_string(&value, "providerId")
}

fn value_string(value: &Value, field: &str) -> Result<String, RpcError> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| RpcError::new("rpc.invalidArguments", format!("{field} 必须是非空字符串")))
}
