//! Edits to the config file that keep the user's comments and layout (`toml_edit`),
//! plus schema-aware validation warnings.

use toml_edit::{Array, DocumentMut, Item, Table, Value};

use super::{ConfigFile, DEFAULT_PROFILE, ProfileConfig};
use crate::auth::GrantKind;
use crate::{Result, ZdkError};

/// Parse raw config text into an editable document.
pub fn parse_document(text: &str) -> Result<DocumentMut> {
    text.parse::<DocumentMut>()
        .map_err(|e| ZdkError::Config(format!("invalid config TOML: {e}")))
}

/// Split `a.b.c` into segments, honouring quoted segments (`profiles."my.profile".subdomain`).
#[must_use]
pub fn split_key(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in key.chars() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

/// Read the item at a dotted key.
#[must_use]
pub fn get_value<'a>(doc: &'a DocumentMut, key: &str) -> Option<&'a Item> {
    let segments = split_key(key);
    let (first, rest) = segments.split_first()?;
    let mut item = doc.as_table().get(first)?;
    for seg in rest {
        item = item.as_table_like()?.get(seg)?;
    }
    Some(item)
}

/// Set a dotted key to `value`, creating intermediate tables. An existing value keeps its
/// trailing comment / spacing; intermediate tables are implicit so no bare `[profiles]` appears.
pub fn set_value(doc: &mut DocumentMut, key: &str, value: Value) -> Result<()> {
    let segments = split_key(key);
    let Some((last, tables)) = segments.split_last() else {
        return Err(ZdkError::Usage("config key must not be empty".into()));
    };
    let table_count = tables.len();
    let mut table: &mut Table = doc.as_table_mut();
    for (i, seg) in tables.iter().enumerate() {
        let is_last_table = i + 1 == table_count;
        let entry = table.entry(seg).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(!is_last_table);
            Item::Table(t)
        });
        table = entry.as_table_mut().ok_or_else(|| {
            ZdkError::Usage(format!(
                "config key '{key}': '{seg}' is a value, not a table"
            ))
        })?;
    }
    if let Some(Item::Value(existing)) = table.get_mut(last) {
        let mut new_value = value;
        *new_value.decor_mut() = existing.decor().clone();
        *existing = new_value;
    } else if matches!(
        table.get(last),
        Some(Item::Table(_) | Item::ArrayOfTables(_))
    ) {
        return Err(ZdkError::Usage(format!(
            "config key '{key}' is a table; set one of its fields instead"
        )));
    } else {
        table.insert(last, Item::Value(value));
    }
    Ok(())
}

/// Remove a dotted key. Returns `false` if it was not present.
pub fn remove_value(doc: &mut DocumentMut, key: &str) -> Result<bool> {
    let segments = split_key(key);
    let Some((last, tables)) = segments.split_last() else {
        return Err(ZdkError::Usage("config key must not be empty".into()));
    };
    let mut table: &mut Table = doc.as_table_mut();
    for seg in tables {
        match table.get_mut(seg).and_then(Item::as_table_mut) {
            Some(t) => table = t,
            None => return Ok(false),
        }
    }
    Ok(table.remove(last).is_some())
}

/// Parse a `zdk config set` value the way TOML would, falling back to a plain string:
/// `true`/`50`/`1.5`/`["a", "b"]`/`"quoted"` are typed; `a,b,c` becomes an array; anything else is a string.
#[must_use]
pub fn parse_config_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    if let Some(v) = parse_scalar(trimmed) {
        return v;
    }
    if trimmed.contains(',') {
        let mut arr = Array::new();
        for part in trimmed.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            arr.push(parse_scalar(part).unwrap_or_else(|| Value::from(part)));
        }
        return Value::Array(arr);
    }
    Value::from(trimmed)
}

fn parse_scalar(s: &str) -> Option<Value> {
    if s.is_empty() {
        return None;
    }
    let doc = format!("v = {s}\n").parse::<DocumentMut>().ok()?;
    let Item::Value(v) = doc.as_table().get("v")?.clone() else {
        return None;
    };
    match v {
        // A bare word that TOML would reject already failed above; a quoted string is explicit.
        Value::String(_) if !s.starts_with('"') && !s.starts_with('\'') => None,
        _ => Some(v),
    }
}

/// `set_value` with typed parsing and schema validation: the typed guess is tried first and,
/// if the schema rejects it (e.g. a numeric-looking `subdomain`), the plain string is used.
pub fn set_value_typed(doc: &mut DocumentMut, key: &str, raw: &str) -> Result<Value> {
    let typed = parse_config_value(raw);
    let mut attempt = doc.clone();
    set_value(&mut attempt, key, typed.clone())?;
    if ConfigFile::from_toml(&attempt.to_string()).is_ok() {
        *doc = attempt;
        return Ok(typed);
    }
    let as_string = Value::from(raw.trim());
    let mut attempt = doc.clone();
    set_value(&mut attempt, key, as_string.clone())?;
    match ConfigFile::from_toml(&attempt.to_string()) {
        Ok(_) => {
            *doc = attempt;
            Ok(as_string)
        }
        Err(e) => Err(ZdkError::Config(format!("cannot set {key} = {raw:?}: {e}"))),
    }
}

/// Add (or replace) `[profiles.<name>]` from a typed profile.
pub fn add_profile(doc: &mut DocumentMut, name: &str, profile: &ProfileConfig) -> Result<()> {
    validate_profile_name(name)?;
    let mut table = Table::new();
    if let Some(v) = &profile.subdomain {
        table.insert("subdomain", toml_edit::value(v.as_str()));
    }
    if let Some(v) = &profile.client_id {
        table.insert("client_id", toml_edit::value(v.as_str()));
    }
    if let Some(g) = profile.grant_type {
        table.insert("grant_type", toml_edit::value(g.as_str()));
    }
    if !profile.scopes.is_empty() {
        let mut arr = Array::new();
        for s in &profile.scopes {
            arr.push(s.as_str());
        }
        table.insert("scopes", toml_edit::value(arr));
    }
    if let Some(v) = &profile.plan {
        table.insert("plan", toml_edit::value(v.as_str()));
    }
    if let Some(v) = &profile.email {
        table.insert("email", toml_edit::value(v.as_str()));
    }
    if let Some(v) = profile.callback_port {
        table.insert("callback_port", toml_edit::value(i64::from(v)));
    }
    if let Some(v) = profile.credential_store {
        table.insert("credential_store", toml_edit::value(v.as_str()));
    }
    let profiles = doc
        .as_table_mut()
        .entry("profiles")
        .or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        })
        .as_table_mut()
        .ok_or_else(|| ZdkError::Config("'profiles' is not a table".into()))?;
    profiles.insert(name, Item::Table(table));
    Ok(())
}

/// Remove `[profiles.<name>]`. Clears `default.active_profile` if it pointed at it.
pub fn remove_profile(doc: &mut DocumentMut, name: &str) -> Result<bool> {
    let removed = match doc
        .as_table_mut()
        .get_mut("profiles")
        .and_then(Item::as_table_mut)
    {
        Some(profiles) => profiles.remove(name).is_some(),
        None => false,
    };
    if removed && active_profile(doc).as_deref() == Some(name) {
        remove_value(doc, "default.active_profile")?;
    }
    Ok(removed)
}

/// Rename a profile, keeping its comments; follows `default.active_profile`.
pub fn rename_profile(doc: &mut DocumentMut, old: &str, new: &str) -> Result<()> {
    validate_profile_name(new)?;
    let profiles = doc
        .as_table_mut()
        .get_mut("profiles")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ZdkError::NotFound {
            resource: "profile".into(),
            id: old.into(),
            request_id: None,
        })?;
    if profiles.contains_key(new) {
        return Err(ZdkError::Usage(format!("profile '{new}' already exists")));
    }
    let item = profiles.remove(old).ok_or_else(|| ZdkError::NotFound {
        resource: "profile".into(),
        id: old.into(),
        request_id: None,
    })?;
    profiles.insert(new, item);
    if active_profile(doc).as_deref() == Some(old) {
        set_value(doc, "default.active_profile", Value::from(new))?;
    }
    Ok(())
}

/// Point `default.active_profile` at an existing profile.
pub fn switch_profile(doc: &mut DocumentMut, name: &str) -> Result<()> {
    let exists = get_value(doc, "profiles")
        .and_then(Item::as_table_like)
        .is_some_and(|t| t.contains_key(name));
    if !exists {
        return Err(ZdkError::NotFound {
            resource: "profile".into(),
            id: name.into(),
            request_id: None,
        });
    }
    set_value(doc, "default.active_profile", Value::from(name))
}

/// Names of the profiles in the document, in file order.
#[must_use]
pub fn profile_names(doc: &DocumentMut) -> Vec<String> {
    get_value(doc, "profiles")
        .and_then(Item::as_table_like)
        .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
        .unwrap_or_default()
}

fn active_profile(doc: &DocumentMut) -> Option<String> {
    get_value(doc, "default.active_profile")
        .and_then(Item::as_str)
        .map(str::to_string)
}

fn validate_profile_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if ok {
        Ok(())
    } else {
        Err(ZdkError::Usage(format!(
            "profile name '{name}' must be [A-Za-z0-9_-]+"
        )))
    }
}

/// Non-fatal problems with a parsed config. Empty means clean.
#[must_use]
pub fn validate(cfg: &ConfigFile) -> Vec<String> {
    let mut warnings: Vec<String> = cfg
        .unknown_keys()
        .into_iter()
        .map(|k| format!("unknown key `{k}` is ignored"))
        .collect();

    if let Some(active) = &cfg.default.active_profile
        && !cfg.profiles.contains_key(active)
    {
        warnings.push(format!(
            "default.active_profile = \"{active}\" but no [profiles.{active}] section exists"
        ));
    }
    if let Some(ps) = cfg.default.page_size
        && !(1..=100).contains(&ps)
    {
        warnings.push(format!(
            "default.page_size = {ps} is outside 1..=100 (Zendesk caps pages at 100)"
        ));
    }
    if !(1..=99).contains(&cfg.auth.refresh_at_percent) {
        warnings.push(format!(
            "auth.refresh_at_percent = {} must be within 1..=99",
            cfg.auth.refresh_at_percent
        ));
    }
    if cfg.auth.method == GrantKind::StaticToken {
        warnings.push("auth.method = \"static_token\" is not a login method; use authorization_code, client_credentials or api_token".into());
    }
    if cfg.rate_limit.max_concurrency == 0 {
        warnings.push("rate_limit.max_concurrency = 0 would block every request; 1 is used".into());
    }
    if cfg.rate_limit.reserve_percent > 50 {
        warnings.push(format!(
            "rate_limit.reserve_percent = {} leaves less than half the budget usable",
            cfg.rate_limit.reserve_percent
        ));
    }
    if cfg.rate_limit.warn_threshold > 100 {
        warnings.push(format!(
            "rate_limit.warn_threshold = {} is a percentage and must be <= 100",
            cfg.rate_limit.warn_threshold
        ));
    }
    if cfg.retry.max_attempts == 0 {
        warnings.push("retry.max_attempts = 0 disables retries entirely".into());
    }
    if cfg.retry.base_ms > cfg.retry.max_ms {
        warnings.push(format!(
            "retry.base_ms ({}) exceeds retry.max_ms ({})",
            cfg.retry.base_ms, cfg.retry.max_ms
        ));
    }
    for code in &cfg.retry.retry_on {
        if !(400..600).contains(code) {
            warnings.push(format!(
                "retry.retry_on contains {code}, which is not an HTTP error status"
            ));
        }
    }

    for (name, p) in &cfg.profiles {
        let grant = p.grant_type.unwrap_or(cfg.auth.method);
        match &p.subdomain {
            None => warnings.push(format!("profiles.{name}.subdomain is missing")),
            Some(s)
                if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') || s.is_empty() =>
            {
                warnings.push(format!("profiles.{name}.subdomain = \"{s}\" should be just the subdomain (e.g. \"acme\"), not a URL"));
            }
            Some(_) => {}
        }
        match grant {
            GrantKind::AuthorizationCode | GrantKind::ClientCredentials => {
                if p.client_id.as_deref().is_none_or(str::is_empty) {
                    warnings.push(format!(
                        "profiles.{name}.client_id is required for grant_type = \"{}\"",
                        grant.as_str()
                    ));
                }
                if p.scopes.is_empty() {
                    warnings.push(format!("profiles.{name}.scopes is empty; `zdk auth login` will request the client's default scopes"));
                }
            }
            GrantKind::ApiToken => {
                if p.email.as_deref().is_none_or(str::is_empty) {
                    warnings.push(format!(
                        "profiles.{name}.email is required for grant_type = \"api_token\""
                    ));
                }
                warnings.push(format!(
                    "profiles.{name} uses API-token auth, which Zendesk stops issuing on 27 Oct 2026 and disables on 30 Apr 2027"
                ));
            }
            GrantKind::StaticToken => {
                warnings.push(format!(
                    "profiles.{name}.grant_type = \"static_token\" is not a login method"
                ));
            }
        }
        if p.scopes
            .iter()
            .any(|s| s.trim().is_empty() || s.contains(' '))
        {
            warnings.push(format!("profiles.{name}.scopes contains an empty or space-separated entry; use one scope per array element"));
        }
    }
    if cfg.profiles.is_empty()
        && cfg
            .default
            .active_profile
            .as_deref()
            .is_some_and(|a| a != DEFAULT_PROFILE)
    {
        warnings.push(
            "no profiles are defined; run `zdk config init` or `zdk config profiles add`".into(),
        );
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoreSelector;

    const COMMENTED: &str = r#"# top comment
[default]
active_profile = "prod"   # trailing
page_size = 100 # keep me

# profile comment
[profiles.prod]
subdomain = "acme"
client_id = "zdk"  # inline
scopes = ["tickets:read"]
"#;

    #[test]
    fn set_value_preserves_comments_and_layout() {
        let mut doc = parse_document(COMMENTED).unwrap();
        set_value(&mut doc, "default.page_size", Value::from(50)).unwrap();
        set_value(&mut doc, "profiles.prod.client_id", Value::from("new")).unwrap();
        set_value(&mut doc, "rate_limit.strategy", Value::from("fail")).unwrap();
        let out = doc.to_string();
        assert!(out.contains("# top comment"), "{out}");
        assert!(out.contains("page_size = 50 # keep me"), "{out}");
        assert!(out.contains("client_id = \"new\"  # inline"), "{out}");
        assert!(out.contains("# profile comment"), "{out}");
        assert!(out.contains("[rate_limit]\nstrategy = \"fail\""), "{out}");
        assert_eq!(
            get_value(&doc, "default.page_size").unwrap().as_integer(),
            Some(50)
        );
        ConfigFile::from_toml(&out).unwrap();
    }

    #[test]
    fn set_value_on_a_new_profile_does_not_emit_a_bare_profiles_header() {
        let mut doc = parse_document("").unwrap();
        set_value(&mut doc, "profiles.sandbox.subdomain", Value::from("s")).unwrap();
        let out = doc.to_string();
        assert!(out.contains("[profiles.sandbox]"), "{out}");
        assert!(!out.contains("[profiles]\n"), "{out}");
    }

    #[test]
    fn set_value_refuses_to_clobber_a_table_or_descend_into_a_value() {
        let mut doc = parse_document(COMMENTED).unwrap();
        assert_eq!(
            set_value(&mut doc, "profiles.prod", Value::from(1))
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            set_value(&mut doc, "default.page_size.x", Value::from(1))
                .unwrap_err()
                .exit_code(),
            2
        );
    }

    #[test]
    fn typed_parsing_covers_bool_int_float_array_and_string() {
        assert_eq!(parse_config_value("true").as_bool(), Some(true));
        assert_eq!(
            parse_config_value("FALSE").as_str(),
            Some("FALSE"),
            "TOML booleans are lowercase"
        );
        assert_eq!(parse_config_value("50").as_integer(), Some(50));
        assert_eq!(parse_config_value("1.5").as_float(), Some(1.5));
        assert_eq!(parse_config_value("acme").as_str(), Some("acme"));
        assert_eq!(parse_config_value("\"quoted\"").as_str(), Some("quoted"));
        let arr = parse_config_value("tickets:read, users:read");
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr.get(1).unwrap().as_str(), Some("users:read"));
        let codes = parse_config_value("429,503");
        assert_eq!(
            codes.as_array().unwrap().get(0).unwrap().as_integer(),
            Some(429)
        );
        let explicit = parse_config_value("[\"a\", \"b\"]");
        assert_eq!(explicit.as_array().unwrap().len(), 2);
    }

    #[test]
    fn schema_aware_set_falls_back_to_string_for_numeric_looking_text() {
        let mut doc = parse_document("").unwrap();
        let v = set_value_typed(&mut doc, "profiles.p.subdomain", "12345").unwrap();
        assert_eq!(v.as_str(), Some("12345"));
        let v = set_value_typed(&mut doc, "default.page_size", "50").unwrap();
        assert_eq!(v.as_integer(), Some(50));
        let err = set_value_typed(&mut doc, "default.page_size", "lots").unwrap_err();
        assert_eq!(err.exit_code(), 10);
        let v = set_value_typed(&mut doc, "retry.retry_on", "429,503").unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn profile_lifecycle_add_switch_rename_remove() {
        let mut doc = parse_document(COMMENTED).unwrap();
        add_profile(
            &mut doc,
            "sandbox",
            &ProfileConfig {
                subdomain: Some("s".into()),
                client_id: Some("c".into()),
                grant_type: Some(GrantKind::ClientCredentials),
                scopes: vec!["read".into()],
                callback_port: Some(8400),
                credential_store: Some(StoreSelector::File),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(profile_names(&doc), vec!["prod", "sandbox"]);
        switch_profile(&mut doc, "sandbox").unwrap();
        assert_eq!(active_profile(&doc).as_deref(), Some("sandbox"));
        assert_eq!(switch_profile(&mut doc, "nope").unwrap_err().exit_code(), 5);

        rename_profile(&mut doc, "sandbox", "staging").unwrap();
        assert_eq!(profile_names(&doc), vec!["prod", "staging"]);
        assert_eq!(active_profile(&doc).as_deref(), Some("staging"));
        assert_eq!(
            rename_profile(&mut doc, "staging", "prod")
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            rename_profile(&mut doc, "ghost", "x")
                .unwrap_err()
                .exit_code(),
            5
        );
        assert_eq!(
            rename_profile(&mut doc, "prod", "bad name")
                .unwrap_err()
                .exit_code(),
            2
        );

        assert!(remove_profile(&mut doc, "staging").unwrap());
        assert!(!remove_profile(&mut doc, "staging").unwrap());
        assert!(
            active_profile(&doc).is_none(),
            "active pointer cleared when its profile goes"
        );
        assert!(doc.to_string().contains("# top comment"));

        let cfg = ConfigFile::from_toml(&doc.to_string()).unwrap();
        assert_eq!(cfg.profiles.len(), 1);
        assert!(cfg.profiles.contains_key("prod"));
    }

    #[test]
    fn validate_reports_the_documented_problems_and_is_quiet_on_the_prd_example() {
        let clean = ConfigFile::from_toml(super::super::tests::PRD_EXAMPLE).unwrap();
        assert!(validate(&clean).is_empty(), "{:?}", validate(&clean));

        let cfg = ConfigFile::from_toml(
            r#"
[default]
active_profile = "ghost"
page_size = 500
zz_unknown = 1
[auth]
refresh_at_percent = 100
[rate_limit]
max_concurrency = 0
[retry]
base_ms = 5000
max_ms = 10
retry_on = [200]
[profiles.oauth]
client_id = "c"
[profiles.legacy]
subdomain = "https://x.zendesk.com"
grant_type = "api_token"
"#,
        )
        .unwrap();
        let w = validate(&cfg).join("\n");
        for needle in [
            "unknown key `default.zz_unknown`",
            "active_profile = \"ghost\"",
            "page_size = 500",
            "refresh_at_percent = 100",
            "max_concurrency = 0",
            "base_ms (5000) exceeds",
            "retry_on contains 200",
            "profiles.oauth.subdomain is missing",
            "profiles.oauth.scopes is empty",
            "profiles.legacy.subdomain",
            "profiles.legacy.email is required",
            "API-token auth",
        ] {
            assert!(w.contains(needle), "missing {needle:?} in:\n{w}");
        }
    }

    #[test]
    fn quoted_segments_survive_splitting() {
        assert_eq!(
            split_key("profiles.\"my.profile\".subdomain"),
            vec!["profiles", "my.profile", "subdomain"]
        );
        assert_eq!(split_key("default.page_size"), vec!["default", "page_size"]);
    }
}
