//! Black-box tests for the `zdk` binary (no network in P1).

mod common;

use common::{Harness, json};
use predicates::prelude::*;

#[test]
fn help_lists_the_commands_and_global_flags() {
    let h = Harness::new();
    h.zdk().arg("--help").assert().success().stdout(
        predicate::str::contains("Zendesk CLI")
            .and(predicate::str::contains("config"))
            .and(predicate::str::contains("completions"))
            .and(predicate::str::contains("--output"))
            .and(predicate::str::contains("--profile"))
            .and(predicate::str::contains("--rate-limit-strategy")),
    );
}

#[test]
fn help_snapshots() {
    let h = Harness::new();
    let root = h.zdk().arg("--help").output().expect("run");
    assert!(root.status.success());
    insta::assert_snapshot!("help", String::from_utf8_lossy(&root.stdout));

    let config = h.zdk().args(["config", "--help"]).output().expect("run");
    assert!(config.status.success());
    insta::assert_snapshot!("config_help", String::from_utf8_lossy(&config.stdout));
}

#[test]
fn version_flag_and_command() {
    let h = Harness::new();
    h.zdk()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^zdk 0\.0\.0\n$").expect("re"));
    h.zdk()
        .args(["version", "-o", "table"])
        .assert()
        .success()
        .stdout("zdk 0.0.0\n");
    // Piped (the default here) means JSON.
    let out = h
        .zdk()
        .arg("version")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["version"], "0.0.0");
}

#[test]
fn version_json_has_target_and_spec_versions() {
    let h = Harness::new();
    let out = h
        .zdk()
        .args(["version", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["version"], "0.0.0");
    assert!(v["target"].as_str().is_some_and(|t| t.contains('-')), "{v}");
    assert!(v["spec_versions"].is_array(), "{v}");
}

#[test]
fn help_json_is_a_walkable_tree() {
    let h = Harness::new();
    let out = h
        .zdk()
        .arg("--help-json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["binary"], "zdk");
    assert_eq!(v["version"], "0.0.0");
    let subs = v["subcommands"].as_array().expect("subcommands");
    let config = subs
        .iter()
        .find(|c| c["name"] == "config")
        .expect("config command");
    assert!(
        config["subcommands"]
            .as_array()
            .is_some_and(|s| s.iter().any(|c| c["name"] == "profiles")),
        "{config}"
    );
    assert!(
        v["global_flags"]
            .as_array()
            .is_some_and(|g| g.iter().any(|f| f["name"] == "--output"))
    );
    assert!(
        v["exit_codes"]
            .as_array()
            .is_some_and(|e| e.iter().any(|c| c["code"] == 130))
    );
}

#[test]
fn unknown_subcommand_and_unimplemented_flags_exit_2() {
    let h = Harness::new();
    h.zdk().arg("frobnicate").assert().code(2);
    h.zdk().args(["--all-profiles", "version"]).assert().code(2);
    h.zdk()
        .args(["--page-size", "0", "version"])
        .assert()
        .code(2);
}

#[test]
fn config_init_then_show_json_has_the_profile() {
    let h = Harness::new();
    h.zdk()
        .args([
            "config",
            "init",
            "--non-interactive",
            "--subdomain",
            "acme",
            "--client-id",
            "zdk",
        ])
        .assert()
        .success();
    assert!(h.config_path().exists());
    let text = h.read_config();
    assert!(
        text.contains("# zendesk-cli (zdk) configuration"),
        "starter file is commented:\n{text}"
    );
    assert!(!text.contains("secret"), "no secret fields in config");

    let out = h
        .zdk()
        .args(["config", "show", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["default"]["active_profile"], "default");
    assert_eq!(v["profiles"]["default"]["subdomain"], "acme");
    assert_eq!(v["profiles"]["default"]["client_id"], "zdk");
    assert_eq!(v["profiles"]["default"]["grant_type"], "authorization_code");

    // Refuses to clobber without --yes; overwrites with it (and honours -p for the profile name).
    h.zdk()
        .args([
            "config",
            "init",
            "--non-interactive",
            "--subdomain",
            "other",
            "--client-id",
            "c",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already exists"));
    h.zdk()
        .args([
            "-p",
            "sandbox",
            "config",
            "init",
            "--non-interactive",
            "--subdomain",
            "other",
            "--client-id",
            "c",
            "--yes",
            "--grant-type",
            "client_credentials",
        ])
        .assert()
        .success();
    let out = h
        .zdk()
        .args(["config", "show", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["default"]["active_profile"], "sandbox");
    assert_eq!(v["profiles"]["sandbox"]["grant_type"], "client_credentials");
}

#[test]
fn config_init_non_interactive_requires_subdomain() {
    let h = Harness::new();
    h.zdk()
        .args(["config", "init", "--non-interactive", "--client-id", "zdk"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--subdomain is required"));
}

#[test]
fn config_set_then_get_round_trips_typed_values_and_keeps_comments() {
    let h = Harness::new();
    h.write_config("# my notes\n[default]\npage_size = 100 # keep\n");
    h.zdk()
        .args(["config", "set", "default.page_size", "50"])
        .assert()
        .success();
    h.zdk()
        .args(["config", "get", "default.page_size"])
        .assert()
        .success()
        .stdout("50\n");
    h.zdk()
        .args([
            "config",
            "set",
            "profiles.prod.scopes",
            "tickets:read,users:read",
        ])
        .assert()
        .success();
    h.zdk()
        .args(["config", "set", "profiles.prod.subdomain", "12345"])
        .assert()
        .success();

    let text = h.read_config();
    assert!(text.contains("# my notes"), "{text}");
    assert!(text.contains("page_size = 50 # keep"), "{text}");
    assert!(
        text.contains("subdomain = \"12345\""),
        "numeric-looking strings stay strings:\n{text}"
    );

    let out = h
        .zdk()
        .args(["config", "get", "profiles.prod.scopes", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!(["tickets:read", "users:read"])
    );
    let out = h
        .zdk()
        .args(["config", "get", "profiles.prod", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["subdomain"], "12345");

    h.zdk()
        .args(["config", "get", "default.nope"])
        .assert()
        .code(5);
    h.zdk()
        .args(["config", "set", "default.page_size", "lots"])
        .assert()
        .code(10);
}

#[test]
fn config_validate_is_quiet_on_a_clean_file_and_warns_on_problems() {
    let h = Harness::new();
    h.write_config("[default]\nactive_profile = \"p\"\n[profiles.p]\nsubdomain = \"acme\"\nclient_id = \"c\"\nscopes = [\"read\"]\n");
    h.zdk()
        .args(["config", "validate"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());

    h.write_config("[default]\nactive_profile = \"ghost\"\nzz_unknown = 1\n");
    let out = h
        .zdk()
        .args(["config", "validate", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let v = json(&out.stdout);
    assert_eq!(v["ok"], true);
    let warnings = v["warnings"].as_array().expect("warnings");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("ghost"))),
        "{v}"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("zz_unknown"));

    // Unparsable TOML is a hard error: exit 10 with the machine-readable error line on stderr.
    h.write_config("[default\n");
    let out = h
        .zdk()
        .args(["config", "validate"])
        .assert()
        .code(10)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty(), "errors never reach stdout");
    let err_line = String::from_utf8_lossy(&out.stderr);
    let err: serde_json::Value = serde_json::from_str(err_line.trim())
        .unwrap_or_else(|e| panic!("stderr not JSON ({e}): {err_line}"));
    assert_eq!(err["error"]["code"], "CONFIG");
    assert_eq!(err["error"]["exit_code"], 10);
    assert!(
        err["error"]["help"]
            .as_str()
            .is_some_and(|h| h.contains("zdk config")),
        "{err}"
    );
}

#[test]
fn table_mode_errors_are_miette_reports_on_stderr() {
    let h = Harness::new();
    h.write_config("[default\n");
    let out = h
        .zdk()
        .args(["config", "validate", "-o", "table"])
        .assert()
        .code(10)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("zdk::config"), "{stderr}");
    assert!(stderr.contains("help:"), "{stderr}");
    assert!(out.stdout.is_empty());
}

#[test]
fn config_path_and_show_defaults_without_a_file() {
    let h = Harness::new();
    let expected = h.config_path();
    h.zdk()
        .args(["config", "path", "-o", "table"])
        .assert()
        .success()
        .stdout(format!("{}\n", expected.display()));
    let out = h
        .zdk()
        .args(["config", "path", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["exists"], false);
    assert!(
        v["state_dir"]
            .as_str()
            .is_some_and(|s| s.ends_with("zendesk-cli"))
    );

    let out = h
        .zdk()
        .args(["config", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(
        v["auth"]["method"], "authorization_code",
        "piped `config show` is JSON: {v}"
    );
    assert_eq!(v["rate_limit"]["strategy"], "wait");

    h.zdk()
        .args(["config", "show", "-o", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# no config file at"));
}

#[test]
fn config_show_effective_masks_env_secrets_unless_revealed() {
    let h = Harness::new();
    let out = h
        .zdk()
        .env("ZENDESK_ACCESS_TOKEN", "hunter2")
        .env("ZENDESK_OUTPUT", "yaml")
        .args([
            "config",
            "show",
            "--effective",
            "-o",
            "json",
            "--page-size",
            "7",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["settings"]["subdomain"], "test");
    assert_eq!(v["settings"]["base_url"], "https://test.zendesk.com/");
    assert_eq!(v["settings"]["page_size"], 7);
    assert_eq!(
        v["settings"]["output"]["format"], "json",
        "flag beats ZENDESK_OUTPUT"
    );
    assert_eq!(v["settings"]["credential_store"], "none");
    assert_eq!(v["env_secrets"]["ZENDESK_ACCESS_TOKEN"], "***");
    assert!(!String::from_utf8_lossy(&out).contains("hunter2"));

    let out = h
        .zdk()
        .env("ZENDESK_ACCESS_TOKEN", "hunter2")
        .args([
            "config",
            "show",
            "--effective",
            "--reveal-secrets",
            "-o",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["env_secrets"]["ZENDESK_ACCESS_TOKEN"], "hunter2");
}

#[test]
fn projection_and_jq_apply_to_command_output() {
    let h = Harness::new();
    let out = h
        .zdk()
        .args([
            "config",
            "show",
            "-o",
            "json",
            "--fields",
            "auth.method,retry.max_attempts",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!({"auth": {"method": "authorization_code"}, "retry": {"max_attempts": 6}})
    );

    h.zdk()
        .args(["config", "show", "-o", "json", "--jq", ".retry.retry_on[]"])
        .assert()
        .success()
        .stdout("429\n500\n502\n503\n504\n");
    h.zdk()
        .args(["config", "show", "--jq", ".retry.jitter"])
        .assert()
        .success()
        .stdout("\"full\"\n");

    let out = h
        .zdk()
        .args(["config", "show", "-o", "json", "--jq", ".["])
        .assert()
        .code(2)
        .get_output()
        .clone();
    let err: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stderr).trim()).expect("json error line");
    assert_eq!(err["error"]["code"], "USAGE");
    assert!(
        err["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("jq:")),
        "{err}"
    );
}

#[test]
fn profiles_lifecycle_through_the_cli() {
    let h = Harness::new();
    h.zdk()
        .args([
            "config",
            "profiles",
            "add",
            "sandbox",
            "--subdomain",
            "s",
            "--client-id",
            "c",
        ])
        .assert()
        .success();
    h.zdk()
        .args([
            "config",
            "profiles",
            "add",
            "prod",
            "--subdomain",
            "acme",
            "--client-id",
            "zdk_prod",
            "--grant-type",
            "client_credentials",
            "--scopes",
            "tickets:read,users:read",
            "--plan",
            "enterprise",
        ])
        .assert()
        .success();

    let out = h
        .zdk()
        .args(["config", "profiles", "list", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows = json(&out);
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 2);
    let sandbox = rows
        .iter()
        .find(|r| r["profile"] == "sandbox")
        .expect("sandbox");
    assert_eq!(sandbox["subdomain"], "s");
    assert_eq!(sandbox["active"], true, "first profile becomes active");
    let prod = rows.iter().find(|r| r["profile"] == "prod").expect("prod");
    assert_eq!(prod["auth"], "client_credentials");
    assert_eq!(prod["plan"], "enterprise");
    assert_eq!(
        prod["scopes"],
        serde_json::json!(["tickets:read", "users:read"])
    );

    let table = h
        .zdk()
        .args(["config", "profiles", "list", "-o", "table"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let table = String::from_utf8_lossy(&table);
    for header in ["PROFILE", "SUBDOMAIN", "AUTH", "PLAN"] {
        assert!(table.contains(header), "{table}");
    }

    h.zdk()
        .args(["config", "profiles", "switch", "prod"])
        .assert()
        .success();
    h.zdk()
        .args(["config", "get", "default.active_profile", "-o", "table"])
        .assert()
        .success()
        .stdout("prod\n");
    h.zdk()
        .args(["config", "get", "default.active_profile"])
        .assert()
        .success()
        .stdout("\"prod\"\n");
    h.zdk()
        .args(["config", "profiles", "switch", "ghost"])
        .assert()
        .code(5);

    h.zdk()
        .args(["config", "profiles", "rename", "prod", "production"])
        .assert()
        .success();
    h.zdk()
        .args(["config", "get", "default.active_profile", "-o", "table"])
        .assert()
        .success()
        .stdout("production\n");
    h.zdk()
        .args(["config", "profiles", "rename", "sandbox", "production"])
        .assert()
        .code(2);

    h.zdk()
        .args(["config", "profiles", "remove", "sandbox"])
        .assert()
        .success();
    h.zdk()
        .args(["config", "profiles", "remove", "sandbox"])
        .assert()
        .code(5);
    let out = h
        .zdk()
        .args(["config", "profiles", "list", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out).as_array().expect("array").len(), 1);

    // The active profile drives the effective settings.
    let out = h
        .zdk()
        .env_remove("ZENDESK_SUBDOMAIN")
        .args(["config", "show", "--effective", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["settings"]["profile_name"], "production");
    assert_eq!(v["settings"]["subdomain"], "acme");
    assert_eq!(v["settings"]["auth"]["method"], "client_credentials");
}

#[test]
fn config_edit_runs_the_editor_and_validates() {
    let h = Harness::new();
    h.zdk()
        .args(["config", "edit"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("EDITOR"));
    // `true` is an editor that changes nothing; the starter file gets written first.
    h.zdk()
        .env("EDITOR", "true")
        .args(["config", "edit", "-o", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"ok\": true"));
    assert!(h.config_path().exists());
    h.zdk()
        .env("EDITOR", "false")
        .args(["config", "edit"])
        .assert()
        .code(1);
}

#[test]
fn completions_are_generated_for_every_shell() {
    let h = Harness::new();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        h.zdk()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("zdk"));
    }
    h.zdk()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef zdk"));
}

#[test]
fn man_pages_are_written_for_root_and_subcommands() {
    let h = Harness::new();
    let dir = h.home.join("man");
    h.zdk()
        .args(["man", "-o", "table", "--to"])
        .arg(&dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("man page(s)"));
    assert!(dir.join("zdk.1").exists());
    assert!(dir.join("zdk-config.1").exists());
    assert!(dir.join("zdk-config-profiles.1").exists());
    let root = std::fs::read_to_string(dir.join("zdk.1")).expect("read");
    assert!(
        root.contains(".TH zdk 1"),
        "{}",
        &root[..root.len().min(200)]
    );
}

#[test]
fn bad_environment_values_are_config_errors() {
    let h = Harness::new();
    let out = h
        .zdk()
        .env("ZENDESK_PAGE_SIZE", "lots")
        .args(["version", "-o", "json"])
        .assert()
        .code(10)
        .get_output()
        .clone();
    let err: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stderr).trim()).expect("json error line");
    assert_eq!(err["error"]["code"], "CONFIG");
    assert!(
        err["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("ZENDESK_PAGE_SIZE")),
        "{err}"
    );
}

#[test]
fn quiet_silences_warnings_but_not_errors() {
    let h = Harness::new();
    h.write_config("[default]\nzz_unknown = 1\n");
    h.zdk()
        .args(["version"])
        .assert()
        .success()
        .stderr(predicate::str::contains("zz_unknown"));
    h.zdk()
        .args(["-q", "version"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
    h.write_config("[default\n");
    h.zdk()
        .args(["-q", "config", "validate"])
        .assert()
        .code(10)
        .stderr(predicate::str::contains("CONFIG"));
}

// ---------------------------------------------------------------------------------------------
// `zdk api` (P2): the escape hatch and the registry, against a wiremock Zendesk
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn api_get_prints_the_body_as_json_when_piped_and_raw_untouched() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"user":{"id":1,"name":"Ada"}}"#)
                .insert_header("content-type", "application/json"),
        )
        .expect(3)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["api", "GET", "/api/v2/users/me"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["user"]["name"], "Ada");
    // Bare path defaults to GET; -o raw is the body untouched.
    h.zdk_api(&server.uri())
        .args(["api", "/api/v2/users/me", "-o", "raw"])
        .assert()
        .success()
        .stdout("{\"user\":{\"id\":1,\"name\":\"Ada\"}}\n");
    // --fields / --jq apply to the escape hatch too.
    h.zdk_api(&server.uri())
        .args(["api", "api/v2/users/me", "--jq", ".user.name"])
        .assert()
        .success()
        .stdout("\"Ada\"\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn api_paginate_with_limit_streams_ndjson_across_pages() {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .and(query_param("page[after]", "c2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tickets": [{"id": 3}, {"id": 4}],
            "meta": {"has_more": false},
            "links": {"next": null}
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .and(query_param("page[size]", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tickets": [{"id": 1}, {"id": 2}],
            "meta": {"has_more": true, "after_cursor": "c2"},
            "links": {"next": format!("{}/api/v2/tickets?page%5Bsize%5D=100&page%5Bafter%5D=c2", server.uri())}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "api",
            "GET",
            "/api/v2/tickets",
            "--paginate",
            "--limit",
            "3",
            "-o",
            "ndjson",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    for (line, id) in lines.iter().zip(1..) {
        assert_eq!(json(line.as_bytes())["id"], id, "{text}");
    }
}

#[test]
fn api_ops_and_describe_use_the_registry() {
    let h = Harness::new();
    let out = h
        .zdk()
        .args(["api", "ops", "--grep", "ListTickets", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows = json(&out);
    let rows = rows.as_array().expect("array");
    assert!(
        rows.iter()
            .any(|r| r["id"] == "ListTickets" && r["scope"] == "tickets:read"),
        "{rows:?}"
    );
    h.zdk()
        .args(["api", "ops", "--grep", "^ListTickets$", "-o", "table"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("ID")
                .and(predicate::str::contains("METHOD"))
                .and(predicate::str::contains("PATH"))
                .and(predicate::str::contains("PAGINATION"))
                .and(predicate::str::contains("SCOPE"))
                .and(predicate::str::contains("/api/v2/tickets")),
        );
    let out = h
        .zdk()
        .args(["api", "ops", "--spec", "voice", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let voice = json(&out);
    let voice = voice.as_array().expect("array");
    assert!(!voice.is_empty() && voice.iter().all(|r| r["spec"] == "voice"));
    let out = h
        .zdk()
        .args(["api", "ops", "--deprecated", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out).as_array().expect("array").len(), 4);
    h.zdk()
        .args(["api", "ops", "--spec", "nope"])
        .assert()
        .code(2);
    h.zdk().args(["api", "ops", "--grep", "["]).assert().code(2);

    let out = h
        .zdk()
        .args(["api", "describe", "ListTickets", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["scope"], "tickets:read");
    assert_eq!(v["path"], "/api/v2/tickets");
    assert_eq!(v["method"], "GET");
    assert!(
        v["parameters"]
            .as_array()
            .is_some_and(|p| p.iter().any(|x| x["name"] == "page")),
        "{v}"
    );
    assert!(v.get("responses").is_none(), "schemas only with --schema");
    let out = h
        .zdk()
        .args(["api", "describe", "CreateTicket", "--schema", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["body"], "required");
    assert!(v["request_body"].is_object(), "{v}");
    let out = h
        .zdk()
        .args(["api", "describe", "GET", "/api/v2/tickets/1", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["id"], "ShowTicket");
    h.zdk()
        .args(["api", "describe", "ListLocales"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("support.ListLocales"));
    h.zdk()
        .args(["api", "describe", "NoSuchOperation"])
        .assert()
        .code(5);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_dry_run_prints_the_request_and_sends_nothing() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "api",
            "POST",
            "/api/v2/tickets",
            "--field",
            "ticket.subject=x",
            "--field",
            "ticket.priority:=\"high\"",
            "--dry-run",
            "-o",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(
        out.stderr.is_empty(),
        "dry run is not an error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out.stdout);
    assert_eq!(v["method"], "POST");
    assert!(
        v["url"]
            .as_str()
            .is_some_and(|u| u.ends_with("/api/v2/tickets")),
        "{v}"
    );
    assert_eq!(v["body"]["ticket"]["subject"], "x");
    assert_eq!(v["body"]["ticket"]["priority"], "high");
    assert_eq!(v["headers"]["authorization"], "Bearer [redacted]");
    assert_eq!(v["headers"]["content-type"], "application/json");
    assert!(v["headers"]["idempotency-key"].is_string(), "{v}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("test-token"));

    h.zdk_api(&server.uri())
        .args([
            "api",
            "POST",
            "/api/v2/tickets",
            "--data",
            r#"{"ticket":{"subject":"y"}}"#,
            "--dry-run",
            "-o",
            "table",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::starts_with("curl -X POST")
                .and(predicate::str::contains("Bearer [redacted]")),
        );
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_errors_map_to_exit_codes_with_json_on_stderr() {
    use common::stderr_error;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/999"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(
                    serde_json::json!({"error": "RecordNotFound", "description": "Not found"}),
                )
                .insert_header("x-request-id", "rid-404"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/1"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "30")
                .insert_header("x-rate-limit", "200")
                .insert_header("x-rate-limit-remaining", "0"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .and(query_param("page", "101"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "InvalidPaginationParameter",
            "description": "Offset pagination is limited to 10,000 records (page 100)"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["api", "GET", "/api/v2/tickets/999"])
        .assert()
        .code(5)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    let err = stderr_error(&out.stderr);
    assert_eq!(err["error"]["code"], "NOT_FOUND");
    assert_eq!(err["error"]["exit_code"], 5);
    assert_eq!(err["error"]["request_id"], "rid-404");
    assert!(
        err["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("ticket '999' not found")),
        "{err}"
    );

    let out = h
        .zdk_api(&server.uri())
        .args([
            "--rate-limit-strategy",
            "fail",
            "api",
            "GET",
            "/api/v2/tickets/1",
        ])
        .assert()
        .code(7)
        .get_output()
        .clone();
    let err = stderr_error(&out.stderr);
    assert_eq!(err["error"]["code"], "RATE_LIMITED");
    assert!(
        err["error"]["help"]
            .as_str()
            .is_some_and(|h| h.contains("30s")),
        "{err}"
    );

    let out = h
        .zdk_api(&server.uri())
        .args([
            "api",
            "GET",
            "/api/v2/search",
            "--query",
            "query=type:ticket",
            "--query",
            "page=101",
        ])
        .assert()
        .code(12)
        .get_output()
        .clone();
    let err = stderr_error(&out.stderr);
    assert_eq!(err["error"]["code"], "PAGINATION_LIMIT");
    assert!(
        err["error"]["help"]
            .as_str()
            .is_some_and(|h| h.contains("search export")),
        "{err}"
    );
}

#[test]
fn api_usage_errors_happen_before_any_request() {
    let h = Harness::new();
    let unreachable = "http://127.0.0.1:1";
    h.zdk_api(unreachable).args(["api"]).assert().code(2);
    h.zdk_api(unreachable)
        .args(["api", "FROB", "/api/v2/tickets"])
        .assert()
        .code(2);
    h.zdk_api(unreachable)
        .args([
            "api",
            "POST",
            "/api/v2/tickets",
            "--data",
            "{}",
            "--field",
            "a=b",
        ])
        .assert()
        .code(2);
    h.zdk_api(unreachable)
        .args(["api", "POST", "/api/v2/tickets", "--field", "novalue"])
        .assert()
        .code(2);
    h.zdk_api(unreachable)
        .args(["api", "GET", "https://evil.example/api/v2/users/me"])
        .assert()
        .code(2);
    h.zdk_api(unreachable)
        .args(["api", "GET", "/api/v2/tickets/1", "--paginate"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not paginated"));
    // No token and no dry-run: exit 3 before any network access.
    h.zdk()
        .env("ZENDESK_BASE_URL", unreachable)
        .args(["api", "GET", "/api/v2/users/me"])
        .assert()
        .code(3);
}
