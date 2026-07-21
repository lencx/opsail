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
        .stdout(
            predicate::str::contains("--format")
                .and(predicate::str::contains("--user-agent"))
                .and(predicate::str::contains("--cdp <ENDPOINT>"))
                .and(predicate::str::contains("--launch"))
                .and(predicate::str::contains("--chrome-path <PATH>"))
                .and(predicate::str::contains("--wait-until <STATE>"))
                .and(predicate::str::contains("--machine")),
        )
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

#[test]
fn refit_codex_help_exposes_the_usage_lifecycle_and_default_port() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["refit", "codex", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("enable")
                .and(predicate::str::contains("disable"))
                .and(predicate::str::contains("status"))
                .and(predicate::str::contains("doctor"))
                .and(predicate::str::contains("--port"))
                .and(predicate::str::contains("127.0.0.1"))
                .and(predicate::str::contains("55321")),
        )
        .stderr("");
}

#[test]
fn refit_codex_enable_accepts_only_the_usage_feature() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["refit", "codex", "enable", "theme"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(
            predicate::str::contains("invalid value 'theme'")
                .and(predicate::str::contains("usage")),
        );
}

#[test]
fn refit_codex_enable_help_documents_launch_once_and_persistent_default() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["refit", "codex", "enable", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--once")
                .and(predicate::str::contains("--launch"))
                .and(predicate::str::contains("current document"))
                .and(predicate::str::contains("close CDP"))
                .and(predicate::str::contains("attach-only"))
                .and(predicate::str::contains("stopped ChatGPT app once"))
                .and(predicate::str::contains(
                    "persistent managed mode is the default",
                )),
        )
        .stderr("");

    let mut once = cargo_bin_cmd!("opsail");
    once.args([
        "refit", "codex", "enable", "usage", "--launch", "--once", "--port", "80",
    ])
    .assert()
    .code(1)
    .stdout("")
    .stderr(predicate::str::contains(
        "[opsail-refit-codex:target-validation-failed]",
    ));
}

#[test]
fn refit_codex_rejects_privileged_ports_before_target_discovery() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["refit", "codex", "status", "--port", "80"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(
            predicate::str::contains("[opsail-refit-codex:target-validation-failed]")
                .and(predicate::str::contains("between 1024 and 65535")),
        );
}

#[test]
fn machine_mode_reads_html_and_emits_a_versioned_envelope() {
    let request = serde_json::json!({
        "protocolVersion": 1,
        "source": {
            "kind": "html",
            "html": sample_html(),
            "baseUrl": "https://example.test/articles/"
        }
    });
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .success()
        .stderr("");
    let response: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(response["protocolVersion"], 1);
    assert_eq!(response["ok"], true);
    assert_eq!(response["engine"]["name"], "opsail");
    assert!(
        response["engine"]["version"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(response["result"]["schemaVersion"], 1);
    assert_eq!(response["result"]["source"]["kind"], "html");
    assert_eq!(response["result"]["metadata"]["title"], "Example document");
    assert!(assert.get_output().stdout.ends_with(b"\n"));
    assert_eq!(
        assert
            .get_output()
            .stdout
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
        1
    );
}

#[test]
fn machine_mode_returns_invalid_json_as_a_structured_failure() {
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "--machine"])
        .write_stdin("{")
        .assert()
        .code(1)
        .stderr("");
    let response: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(response["protocolVersion"], 1);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "invalid-request");
    assert_eq!(response["error"]["stage"], "input");
    assert_eq!(response["error"]["retryable"], false);
}

#[test]
fn machine_mode_rejects_unknown_protocol_versions_structurally() {
    let request = serde_json::json!({
        "protocolVersion": 2,
        "source": {
            "kind": "html",
            "html": sample_html()
        }
    });
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .code(1)
        .stderr("");
    let response: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(response["protocolVersion"], 1);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "unsupported-protocol");
}

#[test]
fn machine_mode_cannot_be_combined_with_human_output_options() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--machine", "--format", "json"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains(
            "the argument '--machine' cannot be used with '--format <FORMAT>'",
        ));
}

#[test]
fn machine_mode_maps_read_failures_without_human_diagnostics() {
    let request = serde_json::json!({
        "protocolVersion": 1,
        "source": {
            "kind": "html",
            "html": "not html"
        }
    });
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .code(1)
        .stderr("");
    let response: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "not-html");
    assert_eq!(response["error"]["stage"], "input");
    assert_eq!(response["error"]["retryable"], false);
}

#[test]
fn machine_mode_reports_verification_without_leaking_tokens() {
    let request = serde_json::json!({
        "protocolVersion": 1,
        "source": {
            "kind": "html",
            "html": concat!(
                "<!doctype html><html><head><title>Just a moment...</title></head><body>",
                "<form id=\"challenge-form\" action=\"/cdn-cgi/challenge-platform/",
                "h/g/orchestrate/chl_page/v1?__cf_chl_tk=challenge-form-secret\"></form>",
                "<script>window._cf_chl_opt = {};</script>",
                "</body></html>"
            ),
            "finalUrl": "https://example.test/protected?token=final-url-secret#private-fragment"
        }
    });
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .code(1)
        .stderr("");
    let output = assert.get_output();
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "verification-required");
    assert_eq!(response["error"]["stage"], "acquire");
    assert_eq!(response["error"]["retryable"], false);
    assert_eq!(response["error"]["recovery"], "rendered-html");

    let stdout = String::from_utf8_lossy(&output.stdout);
    for secret in [
        "challenge-form-secret",
        "final-url-secret",
        "private-fragment",
    ] {
        assert!(!stdout.contains(secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
}

#[test]
fn machine_mode_validates_options_structurally() {
    let request = serde_json::json!({
        "protocolVersion": 1,
        "source": {
            "kind": "html",
            "html": sample_html()
        },
        "options": {
            "maxBytes": 0
        }
    });
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .code(1)
        .stderr("");
    let response: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(response["error"]["code"], "invalid-option");
}

#[test]
fn machine_mode_does_not_echo_url_credentials() {
    let request = serde_json::json!({
        "protocolVersion": 1,
        "source": {
            "kind": "url",
            "url": "https://reader:machine-secret@example.test/article"
        }
    });
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .code(1)
        .stdout(
            predicate::str::contains("url-contains-credentials")
                .and(predicate::str::contains("machine-secret").not()),
        )
        .stderr(predicate::str::contains("machine-secret").not());
}

#[test]
fn machine_mode_rejects_unsafe_html_base_urls() {
    for (base_url, expected_code) in [
        ("file:///tmp/article.html", "unsupported-scheme"),
        (
            "https://reader:base-secret@example.test/article",
            "url-contains-credentials",
        ),
    ] {
        let request = serde_json::json!({
            "protocolVersion": 1,
            "source": {
                "kind": "html",
                "html": sample_html(),
                "baseUrl": base_url
            }
        });
        let mut command = cargo_bin_cmd!("opsail");
        let assert = command
            .args(["read", "--machine"])
            .write_stdin(request.to_string())
            .assert()
            .code(1)
            .stderr(predicate::str::contains("base-secret").not());
        let response: serde_json::Value =
            serde_json::from_slice(&assert.get_output().stdout).unwrap();

        assert_eq!(response["error"]["code"], expected_code);
        assert_eq!(response["error"]["stage"], "input");
        assert!(!String::from_utf8_lossy(&assert.get_output().stdout).contains("base-secret"));
    }
}

#[test]
fn machine_mode_validates_cdp_endpoints_structurally() {
    let request = serde_json::json!({
        "protocolVersion": 1,
        "source": {
            "kind": "cdp",
            "endpoint": "ftp://example.test/devtools/browser/id",
            "url": "https://example.test/article",
            "waitUntil": "load"
        }
    });
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .code(1)
        .stderr("");
    let response: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(response["error"]["code"], "invalid-cdp-endpoint");
    assert_eq!(response["error"]["stage"], "input");
    assert_eq!(response["error"]["retryable"], false);
}

#[test]
fn machine_mode_validates_empty_chrome_paths_structurally() {
    let request = serde_json::json!({
        "protocolVersion": 1,
        "source": {
            "kind": "chrome",
            "url": "https://example.test/article",
            "chromePath": "",
            "waitUntil": "load"
        }
    });
    let mut command = cargo_bin_cmd!("opsail");
    let assert = command
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .code(1)
        .stderr("");
    let response: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(response["error"]["code"], "invalid-chrome-path");
    assert_eq!(response["error"]["stage"], "input");
    assert_eq!(response["error"]["retryable"], false);
}

#[test]
fn cdp_mode_requires_a_url_when_a_source_is_present() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "article.html", "--cdp", "9222"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains(
            "SOURCE must be an HTTP(S) URL or omitted",
        ));
}

#[test]
fn launch_mode_requires_a_source_url() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--launch"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains(
            "--launch requires an HTTP(S) SOURCE URL",
        ));
}

#[test]
fn chrome_path_requires_launch_mode() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args([
            "read",
            "https://example.test/article",
            "--chrome-path",
            "/path/to/chrome",
        ])
        .assert()
        .code(2)
        .stdout("")
        .stderr(
            predicate::str::contains("--chrome-path <PATH>")
                .and(predicate::str::contains("--launch")),
        );
}

#[test]
fn launch_mode_conflicts_with_caller_managed_cdp() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "--launch", "--cdp", "9222"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(
            predicate::str::contains("cannot be used with")
                .and(predicate::str::contains("--launch"))
                .and(predicate::str::contains("--cdp <ENDPOINT>")),
        );
}

#[test]
fn wait_until_requires_a_browser_mode() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args(["read", "-", "--wait-until", "load"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains(
            "--wait-until requires --cdp or --launch",
        ));
}

#[test]
fn cdp_direct_conflicts_with_target_selection() {
    let mut command = cargo_bin_cmd!("opsail");
    command
        .args([
            "read",
            "--cdp",
            "9222",
            "--cdp-direct",
            "--target-id",
            "page-1",
        ])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains(
            "the argument '--cdp-direct' cannot be used with '--target-id <ID>'",
        ));
}
