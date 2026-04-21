use autotune_init::build_init_prompt;
use std::path::Path;

#[test]
fn prompt_contains_repo_root() {
    let prompt = build_init_prompt(Path::new("/home/user/myproject"));
    assert!(prompt.contains("/home/user/myproject"));
}

#[test]
fn prompt_describes_xml_wire_protocol() {
    let prompt = build_init_prompt(Path::new("/tmp"));
    assert!(prompt.contains("XML"));
    assert!(prompt.contains("<message>"));
    assert!(prompt.contains("<question>"));
    assert!(prompt.contains("<task>"));
    assert!(prompt.contains("CDATA"));
}

#[test]
fn prompt_contains_all_section_tags() {
    let prompt = build_init_prompt(Path::new("/tmp"));
    assert!(prompt.contains("<task>"));
    assert!(prompt.contains("<paths>"));
    assert!(prompt.contains("<measure>"));
    assert!(prompt.contains("<score>"));
    assert!(prompt.contains("<test>"));
    assert!(prompt.contains("<agent>"));
}

#[test]
fn prompt_mentions_multi_fragment_and_stop_criteria() {
    let prompt = build_init_prompt(Path::new("/tmp"));
    assert!(prompt.contains("multiple fragments"));
    assert!(prompt.contains("stop criteria"));
}

#[test]
fn prompt_documents_judge_adaptor() {
    let prompt = build_init_prompt(Path::new("/repo"));
    assert!(
        prompt.contains("judge"),
        "prompt should mention judge adaptor"
    );
    assert!(
        prompt.contains("<rubric>"),
        "prompt should document <rubric> fragment"
    );
    assert!(
        prompt.contains("rubrics-done"),
        "prompt should document rubrics-done"
    );
    assert!(
        prompt.contains("<persona>"),
        "prompt should document persona field"
    );
}
