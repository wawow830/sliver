use std::process::Command;

#[test]
fn missing_config_path_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_sliver"))
        .output()
        .expect("failed to run sliver");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("stderr was not UTF-8");
    assert!(stderr.contains("usage: sliver FILE"), "{stderr}");
}
