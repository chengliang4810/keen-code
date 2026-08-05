use crate::error_suggest::matcher::{fuzzy_filter, fuzzy_top_n};

#[test]
fn test_fuzzy_top_n_returns_sorted_matches() {
    let candidates: Vec<String> = vec![
        "peri-agent".into(),
        "peri-tui".into(),
        "peri-middlewares".into(),
        "langfuse-client".into(),
    ];
    let result = fuzzy_top_n(&candidates, "peri", 3);
    assert_eq!(result.len(), 3);
    let names: Vec<&str> = result.iter().map(|s| s.as_str()).collect();
    assert!(names.contains(&"peri-agent"));
    assert!(names.contains(&"peri-tui"));
    assert!(names.contains(&"peri-middlewares"));
}

#[test]
fn test_fuzzy_top_n_handles_no_matches() {
    let candidates: Vec<String> = vec!["foo".into(), "bar".into()];
    let result = fuzzy_top_n(&candidates, "zzz", 3);
    assert!(result.is_empty());
}

#[test]
fn test_fuzzy_top_n_respects_limit() {
    let candidates: Vec<String> = (0..10).map(|i| format!("candidate-{i}")).collect();
    let result = fuzzy_top_n(&candidates, "candidate", 3);
    assert_eq!(result.len(), 3);
}

#[test]
fn test_fuzzy_filter_returns_owned_strings_sorted() {
    let candidates: Vec<String> = vec![
        "src/main.rs".into(),
        "src/lib.rs".into(),
        "README.md".into(),
    ];
    let result = fuzzy_filter(&candidates, "main");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], "src/main.rs");
}
