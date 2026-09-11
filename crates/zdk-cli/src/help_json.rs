//! `zdk --help-json`: the whole clap command tree as JSON, for documentation generators and
//! agents that want to discover the surface without scraping `--help`.
//!
//! Shape: the root carries `global_flags` once; every command (root included) has
//! `subcommands`, so the tree can be walked uniformly. `examples` and `exit_codes` are emitted
//! empty on purpose — they are prose curated on the docs side and merged in by command path.

use clap::{Arg, Command};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HelpJson {
    pub version: String,
    pub binary: String,
    pub description: String,
    pub usage: String,
    pub global_flags: Vec<FlagJson>,
    pub exit_codes: Vec<ExitCodeJson>,
    pub subcommands: Vec<CommandJson>,
}

#[derive(Debug, Serialize)]
pub struct CommandJson {
    pub name: String,
    pub path: Vec<String>,
    pub summary: String,
    pub description: String,
    pub usage: String,
    pub flags: Vec<FlagJson>,
    pub examples: Vec<serde_json::Value>,
    pub exit_codes: Vec<serde_json::Value>,
    pub subcommands: Vec<CommandJson>,
}

#[derive(Debug, Serialize)]
pub struct FlagJson {
    pub name: String,
    pub description: String,
    pub alias: Option<String>,
    pub value_name: Option<String>,
    pub required: bool,
    pub env: Option<String>,
    pub default: Option<String>,
    pub possible_values: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExitCodeJson {
    pub code: i32,
    pub name: &'static str,
    pub meaning: &'static str,
}

/// PRD §14.3, the process exit-code contract.
pub const EXIT_CODES: &[ExitCodeJson] = &[
    ExitCodeJson {
        code: 0,
        name: "OK",
        meaning: "success (also --dry-run)",
    },
    ExitCodeJson {
        code: 1,
        name: "ERROR",
        meaning: "unexpected error",
    },
    ExitCodeJson {
        code: 2,
        name: "USAGE",
        meaning: "usage / argument error",
    },
    ExitCodeJson {
        code: 3,
        name: "AUTH",
        meaning: "authentication failed or missing",
    },
    ExitCodeJson {
        code: 4,
        name: "FORBIDDEN",
        meaning: "forbidden or missing scope",
    },
    ExitCodeJson {
        code: 5,
        name: "NOT_FOUND",
        meaning: "resource not found",
    },
    ExitCodeJson {
        code: 6,
        name: "VALIDATION",
        meaning: "Zendesk rejected the request (400/409/422)",
    },
    ExitCodeJson {
        code: 7,
        name: "RATE_LIMITED",
        meaning: "rate limit exhausted (or --rate-limit-strategy fail)",
    },
    ExitCodeJson {
        code: 8,
        name: "PARTIAL_FAILURE",
        meaning: "bulk operation completed with failures",
    },
    ExitCodeJson {
        code: 9,
        name: "SERVER_ERROR",
        meaning: "Zendesk 5xx after retries",
    },
    ExitCodeJson {
        code: 10,
        name: "CONFIG",
        meaning: "configuration or credential-store error",
    },
    ExitCodeJson {
        code: 11,
        name: "NETWORK",
        meaning: "network / TLS error",
    },
    ExitCodeJson {
        code: 12,
        name: "PAGINATION_LIMIT",
        meaning: "offset pagination cap reached",
    },
    ExitCodeJson {
        code: 13,
        name: "JOB_TIMEOUT",
        meaning: "job did not finish in time",
    },
    ExitCodeJson {
        code: 130,
        name: "INTERRUPTED",
        meaning: "interrupted (ctrl-c)",
    },
];

/// Render the tree rooted at `cmd`.
pub fn build(mut cmd: Command) -> HelpJson {
    // Populate propagated globals and generated help/version args so the filtering below sees
    // the same argument set a real parse would.
    cmd.build();

    let description = cmd.get_about().map(ToString::to_string).unwrap_or_default();
    let usage = usage_for(&cmd, &[]);
    let global_flags = cmd
        .get_arguments()
        .filter(|a| a.is_global_set())
        .filter_map(|a| flag_json(a, true))
        .collect();
    let subcommands = cmd
        .get_subcommands()
        .filter(|s| !s.is_hide_set() && s.get_name() != "help")
        .map(|s| command_json(s, &[]))
        .collect();

    HelpJson {
        version: env!("CARGO_PKG_VERSION").to_string(),
        binary: cmd.get_name().to_string(),
        description,
        usage,
        global_flags,
        exit_codes: EXIT_CODES
            .iter()
            .map(|e| ExitCodeJson {
                code: e.code,
                name: e.name,
                meaning: e.meaning,
            })
            .collect(),
        subcommands,
    }
}

fn command_json(cmd: &Command, parents: &[String]) -> CommandJson {
    let name = cmd.get_name().to_string();
    let mut path = parents.to_vec();
    path.push(name.clone());

    let summary = cmd.get_about().map(ToString::to_string).unwrap_or_default();
    let description = cmd
        .get_long_about()
        .map_or_else(|| summary.clone(), ToString::to_string);
    let subcommands = cmd
        .get_subcommands()
        .filter(|s| !s.is_hide_set() && s.get_name() != "help")
        .map(|s| command_json(s, &path))
        .collect();

    CommandJson {
        name,
        summary,
        description,
        usage: usage_for(cmd, &path),
        flags: cmd
            .get_arguments()
            .filter_map(|a| flag_json(a, false))
            .collect(),
        examples: Vec::new(),
        exit_codes: Vec::new(),
        subcommands,
        path,
    }
}

/// `zdk config set [OPTIONS] <KEY> <VALUE>` without clap's leading "Usage: ".
fn usage_for(cmd: &Command, path: &[String]) -> String {
    let rendered = cmd.clone().render_usage().to_string();
    let line = rendered
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string();
    let line = line
        .strip_prefix("Usage:")
        .unwrap_or(&line)
        .trim()
        .to_string();
    if line.is_empty() {
        format!("zdk {}", path.join(" ")).trim().to_string()
    } else {
        line
    }
}

/// Command-specific options only unless `include_globals`; `help`/`version` are clap's own.
fn flag_json(arg: &Arg, include_globals: bool) -> Option<FlagJson> {
    if arg.is_global_set() != include_globals || arg.is_hide_set() {
        return None;
    }
    let id = arg.get_id().as_str();
    if id == "help" || id == "version" {
        return None;
    }

    let long = arg.get_long().map(|l| format!("--{l}"));
    let short = arg.get_short().map(|s| format!("-{s}"));
    let name = long
        .clone()
        .or_else(|| short.clone())
        .unwrap_or_else(|| id.to_uppercase());
    let value_name = arg
        .get_value_names()
        .and_then(|names| names.first().map(ToString::to_string));
    let defaults = arg.get_default_values();
    let default = (!defaults.is_empty()).then(|| {
        defaults
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    });
    let possible_values = arg
        .get_possible_values()
        .iter()
        .filter(|p| !p.is_hide_set())
        .map(|p| p.get_name().to_string())
        .collect();

    Some(FlagJson {
        alias: if long.is_some() { short } else { None },
        name,
        description: arg
            .get_help()
            .map(ToString::to_string)
            .unwrap_or_default()
            .replace('\n', " "),
        value_name,
        required: arg.is_required_set(),
        env: arg
            .get_env()
            .map(|e| e.to_string_lossy().into_owned())
            .filter(|e| !e.is_empty()),
        default,
        possible_values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::CommandFactory;

    fn tree() -> HelpJson {
        build(Cli::command())
    }

    fn find<'a>(commands: &'a [CommandJson], path: &[&str]) -> &'a CommandJson {
        let (head, rest) = path.split_first().expect("non-empty path");
        let found = commands
            .iter()
            .find(|c| c.name == *head)
            .unwrap_or_else(|| panic!("no command {head}"));
        if rest.is_empty() {
            found
        } else {
            find(&found.subcommands, rest)
        }
    }

    #[test]
    fn root_carries_version_binary_globals_and_exit_codes() {
        let t = tree();
        assert_eq!(t.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(t.binary, "zdk");
        assert!(t.usage.starts_with("zdk"), "{}", t.usage);
        let names: Vec<&str> = t.global_flags.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"--output"), "{names:?}");
        assert!(names.contains(&"--profile"), "{names:?}");
        assert!(
            !names.contains(&"--base-url"),
            "hidden flags stay hidden: {names:?}"
        );
        assert_eq!(t.exit_codes.len(), 15);
        assert!(t.exit_codes.iter().any(|e| e.code == 130));
    }

    #[test]
    fn nested_commands_carry_paths_usage_and_no_repeated_globals() {
        let t = tree();
        let set = find(&t.subcommands, &["config", "set"]);
        assert_eq!(set.path, vec!["config", "set"]);
        assert!(set.usage.starts_with("zdk config set"), "{}", set.usage);
        assert_eq!(set.usage.matches("config set").count(), 1, "{}", set.usage);
        let names: Vec<&str> = set.flags.iter().map(|f| f.name.as_str()).collect();
        for global in ["--output", "--profile", "--quiet", "--help", "--version"] {
            assert!(!names.contains(&global), "{global} leaked into {names:?}");
        }
        assert!(names.contains(&"KEY"), "{names:?}");
        let init = find(&t.subcommands, &["config", "init"]);
        let grant = init
            .flags
            .iter()
            .find(|f| f.name == "--grant-type")
            .expect("--grant-type");
        assert!(
            grant
                .possible_values
                .contains(&"client_credentials".to_string()),
            "{:?}",
            grant.possible_values
        );
    }

    #[test]
    fn synthesised_help_subcommands_are_excluded() {
        fn walk(commands: &[CommandJson], found: &mut Vec<String>) {
            for c in commands {
                if c.name == "help" {
                    found.push(c.path.join(" "));
                }
                walk(&c.subcommands, found);
            }
        }
        let mut found = Vec::new();
        walk(&tree().subcommands, &mut found);
        assert!(found.is_empty(), "help subcommands leaked: {found:?}");
    }

    #[test]
    fn env_backed_flags_report_their_variable() {
        let t = tree();
        let profile = t
            .global_flags
            .iter()
            .find(|f| f.name == "--profile")
            .expect("--profile");
        assert_eq!(profile.env.as_deref(), Some("ZENDESK_PROFILE"));
        assert_eq!(profile.alias.as_deref(), Some("-p"));
    }

    #[test]
    fn serialises_to_the_documented_shape() {
        let value = serde_json::to_value(tree()).unwrap();
        for key in [
            "version",
            "binary",
            "description",
            "usage",
            "global_flags",
            "exit_codes",
            "subcommands",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        let first = &value["subcommands"][0];
        for key in [
            "name",
            "path",
            "summary",
            "description",
            "usage",
            "flags",
            "examples",
            "exit_codes",
            "subcommands",
        ] {
            assert!(first.get(key).is_some(), "command missing {key}");
        }
    }
}
