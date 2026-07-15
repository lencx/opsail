use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::tempdir;

fn sample_html() -> String {
    let words = (0..140)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<!doctype html><html dir=\"ltr\"><head><title>Example document</title></head><body><main><article><p>{words}</p><p><a href=\"/guide\">Read the guide</a></p></article></main></body></html>"
    )
}

#[test]
fn read_defaults_to_stdin_and_markdown() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--base-url", "https://example.test/articles/"])
        .write_stdin(sample_html())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("# Example document")
                .and(predicate::str::contains("https://example.test/guide")),
        )
        .stderr("");
}

#[test]
fn extract_alias_reads_a_file_and_writes_html_to_a_file() {
    let directory = tempdir().unwrap();
    let input = directory.path().join("input.html");
    let output = directory.path().join("output.html");
    fs::write(&input, sample_html()).unwrap();

    let mut command = cargo_bin_cmd!("opsail");
    command
        .arg("extract")
        .arg(&input)
        .args(["--format", "html", "--output"])
        .arg(&output)
        .assert()
        .success()
        .stdout("")
        .stderr("");

    let written = fs::read_to_string(output).unwrap();
    assert!(written.contains("<p>"));
    assert!(written.contains("Read the guide"));
}

#[test]
fn json_property_is_valid_json_on_stdout() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "-", "--format", "json", "--property", "title"])
        .write_stdin(sample_html())
        .assert()
        .success()
        .stdout("\"Example document\"\n")
        .stderr("");
}

#[test]
fn direction_property_is_available() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "-", "--format", "json", "--property", "direction"])
        .write_stdin(sample_html())
        .assert()
        .success()
        .stdout("\"ltr\"\n")
        .stderr("");
}

#[test]
fn unknown_property_is_a_clap_usage_error() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--property", "missing"])
        .write_stdin(sample_html())
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains("invalid value 'missing'"));
}

#[test]
fn invalid_format_is_a_clap_usage_error() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--format", "yaml"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains("invalid value 'yaml'"));
}

#[test]
fn complete_json_output_has_a_versioned_schema() {
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "-", "--format", "json"])
        .write_stdin(sample_html())
        .assert()
        .success()
        .stderr("");
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["metadata"]["title"], "Example document");
    assert!(
        value["content"]
            .as_str()
            .unwrap()
            .starts_with("# Example document")
    );
    assert!(value["quality"]["wordCount"].as_u64().unwrap() >= 140);
}

#[test]
fn missing_files_are_runtime_errors_with_empty_stdout() {
    let directory = tempdir().unwrap();
    let missing = directory.path().join("missing.html");
    let mut command = cargo_bin_cmd!("opsail");

    command
        .arg("read")
        .arg(missing)
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("failed to read source"));
}

#[test]
fn zero_byte_limit_is_a_clap_usage_error() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--max-bytes", "0"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains("value must be greater than zero"));
}

#[test]
fn non_http_base_urls_are_rejected_before_acquisition() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--base-url", "file:///tmp/article.html"])
        .write_stdin(sample_html())
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("base URL must use HTTP or HTTPS"));
}

#[test]
fn embedded_url_credentials_are_rejected_without_echoing_them() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args([
            "read",
            "--base-url",
            "https://reader:super-secret@example.test/article",
        ])
        .write_stdin(sample_html())
        .assert()
        .code(1)
        .stdout("")
        .stderr(
            predicate::str::contains("base URL must not contain embedded credentials")
                .and(predicate::str::contains("super-secret").not()),
        );
}

#[test]
fn url_source_credentials_are_rejected_without_echoing_them() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "https://reader:source-secret@example.test/article"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(
            predicate::str::contains("source URL must not contain embedded credentials")
                .and(predicate::str::contains("source-secret").not()),
        );
}

#[test]
fn read_help_is_successful_and_stays_on_stdout() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--format"))
        .stderr("");
}

#[test]
fn read_without_arguments_shows_help_instead_of_reading_stdin() {
    let mut command = cargo_bin_cmd!("opsail");
    command.arg("read").assert().code(2).stdout("").stderr(
        predicate::str::contains("Usage: opsail read [OPTIONS] [SOURCE]")
            .and(predicate::str::contains("--format")),
    );
}
