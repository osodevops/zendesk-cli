//! `--jq`: an embedded jq-compatible filter (jaq), so agents get `.[] | select(...)` without a jq binary.

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Vars, data};
use jaq_json::Val;
use serde::Deserialize;
use serde_json::Value;

use crate::{Result, ZdkError};

/// Run `filter` over `input`, collecting every output value.
pub fn apply(filter: &str, input: Value) -> Result<Vec<Value>> {
    let program = File {
        code: filter,
        path: (),
    };
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());

    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader.load(&arena, program).map_err(|errs| {
        let detail = errs
            .into_iter()
            .map(|(_, e)| match e {
                jaq_core::load::Error::Io(v) => v
                    .into_iter()
                    .map(|(p, m)| format!("{p}: {m}"))
                    .collect::<Vec<_>>()
                    .join("; "),
                jaq_core::load::Error::Lex(v) => v
                    .into_iter()
                    .map(|(expect, at)| format!("expected {} at `{at}`", expect.as_str()))
                    .collect::<Vec<_>>()
                    .join("; "),
                jaq_core::load::Error::Parse(v) => v
                    .into_iter()
                    .map(|(expect, at)| format!("expected {} at `{at}`", expect.as_str()))
                    .collect::<Vec<_>>()
                    .join("; "),
            })
            .collect::<Vec<_>>()
            .join("; ");
        usage(&format!("cannot parse filter '{filter}': {detail}"))
    })?;

    let compiled = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errs| {
            let detail = errs
                .into_iter()
                .flat_map(|(_, es)| {
                    es.into_iter()
                        .map(|(name, kind)| format!("undefined {} `{name}`", kind.as_str()))
                })
                .collect::<Vec<_>>()
                .join("; ");
            usage(&format!("cannot compile filter '{filter}': {detail}"))
        })?;

    let input =
        Val::deserialize(input).map_err(|e| usage(&format!("cannot convert input: {e}")))?;
    let ctx = Ctx::<data::JustLut<Val>>::new(&compiled.lut, Vars::new([]));

    let mut out = Vec::new();
    for result in compiled.id.run((ctx, input)) {
        match result {
            Ok(val) => out.push(to_json(&val)?),
            Err(exn) => {
                return Err(match exn.get_err() {
                    Ok(err) => usage(&format!("{err}")),
                    Err(_) => usage("filter halted"),
                });
            }
        }
    }
    Ok(out)
}

fn usage(msg: &str) -> ZdkError {
    ZdkError::Usage(format!("jq: {msg}"))
}

/// `Val` renders as JSON text; parsing it back keeps key order and number fidelity.
fn to_json(val: &Val) -> Result<Value> {
    serde_json::from_str(&val.to_string())
        .map_err(|e| usage(&format!("cannot convert output: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn select_and_project_over_an_array() {
        let input = json!([{"a": 1, "b": "x"}, {"a": 2, "b": "y"}, {"a": 1, "b": "z"}]);
        let out = apply(".[] | select(.a==1) | .b", input).unwrap();
        assert_eq!(out, vec![json!("x"), json!("z")]);
    }

    #[test]
    fn std_library_functions_are_available_and_order_is_kept() {
        let input = json!({"z": 1, "a": [3, 1, 2], "s": "Hi"});
        assert_eq!(
            apply("keys_unsorted", input.clone()).unwrap(),
            vec![json!(["z", "a", "s"])]
        );
        assert_eq!(
            apply(".a | sort | map(. * 2)", input.clone()).unwrap(),
            vec![json!([2, 4, 6])]
        );
        assert_eq!(
            apply(".s | ascii_downcase", input.clone()).unwrap(),
            vec![json!("hi")]
        );
        assert_eq!(
            apply("[.a[] | tostring] | join(\",\")", input).unwrap(),
            vec![json!("3,1,2")]
        );
    }

    #[test]
    fn multiple_outputs_and_empty() {
        assert_eq!(
            apply(".[]", json!([1, 2])).unwrap(),
            vec![json!(1), json!(2)]
        );
        assert!(apply("empty", json!(1)).unwrap().is_empty());
    }

    #[test]
    fn syntax_and_runtime_errors_are_usage_errors() {
        let err = apply(".[", json!([])).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().starts_with("jq: "), "{err}");
        let err = apply("nosuchfn", json!(1)).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("undefined filter"), "{err}");
        let err = apply(".a.b", json!("string")).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }
}
