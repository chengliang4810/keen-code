use super::*;

#[test]
fn test_session_config_cancel() {
    let cfg = SessionConfig::new();
    assert!(!cfg.is_cancelled());

    let child = cfg.cancel_token.child_token();
    cfg.cancel();
    assert!(cfg.is_cancelled());
    assert!(child.is_cancelled(), "子 token 应跟随父 token 取消");
}

#[test]
fn test_session_config_max_iterations() {
    let cfg = SessionConfig::new();
    assert_eq!(cfg.max_iterations(), 500);

    cfg.set_max_iterations(100);
    assert_eq!(cfg.max_iterations(), 100);
}
