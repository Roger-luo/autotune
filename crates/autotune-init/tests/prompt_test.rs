use autotune_init::build_init_prompt;
use std::path::Path;

#[test]
fn prompt_contains_repo_root() {
    let prompt = build_init_prompt(Path::new("/home/user/myproject"));
    assert!(prompt.contains("/home/user/myproject"));
}

#[test]
fn prompt_contains_protocol_schema() {
    let prompt = build_init_prompt(Path::new("/tmp"));
    assert!(prompt.contains(r#""type": "message""#));
    assert!(prompt.contains(r#""type": "question""#));
    assert!(prompt.contains(r#""type": "config""#));
}

#[test]
fn prompt_contains_section_descriptions() {
    let prompt = build_init_prompt(Path::new("/tmp"));
    assert!(prompt.contains("experiment (required)"));
    assert!(prompt.contains("paths (required)"));
    assert!(prompt.contains("benchmark (required"));
    assert!(prompt.contains("score (required)"));
    assert!(prompt.contains("test (optional"));
    assert!(prompt.contains("agent (optional)"));
}
