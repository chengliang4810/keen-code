use super::*;
use crate::extensions::PluginProvidesDto;

fn plugin(market: &str, enabled: bool) -> PluginDto {
    PluginDto {
        name: format!("proof@{market}"),
        description: None,
        version: Some("1".into()),
        marketplace: Some(market.into()),
        marketplace_path: format!("D:/fixture/cache/{market}"),
        path: format!("D:/fixture/cache/{market}/proof/hash"),
        enabled,
        provides: PluginProvidesDto {
            commands: 1,
            skills: 0,
            agents: 0,
            hooks: 0,
            mcp: 0,
            lsp: 0,
        },
        unsupported_hooks: Vec::new(),
    }
}
fn reference(market: &str) -> InputReference {
    InputReference {
        name: "proof".into(),
        path: format!("plugin://proof@{market}"),
    }
}

#[test]
fn exact_plugin_identity_and_current_enabled_state_are_required() {
    let catalog = vec![plugin("local", true), plugin("other", false)];
    assert_eq!(
        selected_references("@proof 原请求", &[reference("local")], &catalog).unwrap(),
        vec![reference("local")]
    );
    assert!(
        selected_references("@proof", &[reference("other")], &catalog)
            .unwrap_err()
            .contains("禁用")
    );
    assert!(
        selected_references("@proof", &[reference("missing")], &catalog)
            .unwrap_err()
            .contains("失效")
    );
    assert!(
        selected_references(
            "@proof",
            &[InputReference {
                name: "wrong".into(),
                ..reference("local")
            }],
            &catalog
        )
        .is_err()
    );
    assert!(
        selected_references(
            "@proof",
            &[InputReference {
                name: "proof".into(),
                path: "D:/file.txt".into()
            }],
            &catalog
        )
        .is_err()
    );
    assert!(
        selected_references(
            "@proof",
            &[reference("local"), reference("local")],
            &catalog
        )
        .is_err()
    );
}

#[test]
fn original_quoted_and_bare_tokens_require_exact_boundaries() {
    let refs = [reference("local")];
    let catalog = [plugin("local", true)];
    for text in [
        "@proof",
        "first\n@PROOF last",
        "@\"plugin://proof@local\" 原请求",
        "@\"proof@local\"",
    ] {
        assert!(selected_references(text, &refs, &catalog).is_ok(), "{text}");
    }
    for text in [
        "@proof-other",
        "mail@proof",
        "@proof@local",
        "@\"proof\"suffix",
        "@\"proof\"@local",
        "正文未引用",
    ] {
        assert!(
            selected_references(text, &refs, &catalog).is_err(),
            "{text}"
        );
    }
    assert_eq!(
        selected_references("正文", &[], &catalog).unwrap(),
        Vec::<InputReference>::new()
    );
}

#[test]
fn malformed_or_unbounded_acp_reference_metadata_is_rejected() {
    assert!(decode_references(None).unwrap().is_empty());
    for value in [
        serde_json::json!("proof"),
        serde_json::json!([{ "name":"proof", "path":"plugin://proof@local", "extra":true }]),
        serde_json::json!([{ "name":"proof", "path":"plugin://proof@local\n" }]),
    ] {
        let meta = serde_json::Map::from_iter([("keencode/messageReferences".into(), value)]);
        assert!(decode_references(Some(&meta)).is_err());
    }
}

/// 未接通的宿主入口必须拒绝任何引用扩展，连畸形数据或空数组也不能静默吞掉。
#[test]
fn unsupported_input_boundaries_reject_reference_extensions() {
    assert!(reject_unsupported_references(None).is_ok());
    let plain = serde_json::Map::from_iter([(
        "keencode/operationId".into(),
        serde_json::json!("operation"),
    )]);
    assert!(reject_unsupported_references(Some(&plain)).is_ok());
    for value in [
        serde_json::json!([]),
        serde_json::json!("bad"),
        serde_json::json!([reference("local")]),
    ] {
        let meta = serde_json::Map::from_iter([("keencode/messageReferences".into(), value)]);
        assert!(reject_unsupported_references(Some(&meta)).is_err());
    }
}
