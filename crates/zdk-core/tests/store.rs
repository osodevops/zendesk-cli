//! Credential stores through the public `store::open` / `open_with_probe` API with a
//! temporary `Paths` tree. The OS keyring is never touched.

use std::path::Path;

use chrono::Utc;
use secrecy::{ExposeSecret, SecretString};
use zdk_core::ZdkError;
use zdk_core::auth::GrantKind;
use zdk_core::auth::token::{Credential, TokenSet};
use zdk_core::config::{EnvOverrides, Paths};
use zdk_core::store::file::{CREDENTIALS_FILE, KEY_FILE};
use zdk_core::store::{self, StoreDecision, StoreKind, StoreSelector};

fn paths(root: &Path) -> Paths {
    Paths {
        config_file: root.join("config").join("config.toml"),
        config_dir: root.join("config"),
        state_dir: root.join("state"),
        cache_dir: root.join("cache"),
    }
}

fn oauth(sub: &str) -> Credential {
    Credential::OAuth(TokenSet {
        access_token: SecretString::from("access-token-value".to_string()),
        token_type: "bearer".into(),
        scopes: vec!["tickets:read".into(), "users:read".into()],
        obtained_at: Utc::now(),
        expires_at: Some(Utc::now() + chrono::TimeDelta::minutes(30)),
        refresh_token: Some(SecretString::from("refresh-token-value".to_string())),
        refresh_expires_at: None,
        grant: GrantKind::AuthorizationCode,
        subdomain: sub.into(),
        client_id: "zdk".into(),
        client_secret: None,
    })
}

#[cfg(unix)]
fn assert_mode_0600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "{}", path.display());
}

#[cfg(not(unix))]
fn assert_mode_0600(_path: &Path) {}

#[test]
fn file_store_end_to_end_with_the_machine_key() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let env = EnvOverrides::default();

    let s = store::open(StoreSelector::File, &p, &env).unwrap();
    assert_eq!(s.kind(), StoreKind::File);
    assert!(s.load("default").unwrap().is_none());
    assert!(s.list_profiles().unwrap().is_empty());

    s.save("default", &oauth("acme")).unwrap();
    s.save("staging", &oauth("acme-staging")).unwrap();
    let enc = p.config_dir.join(CREDENTIALS_FILE);
    let key = p.config_dir.join(KEY_FILE);
    assert!(enc.exists() && key.exists());
    assert_mode_0600(&enc);
    assert_mode_0600(&key);
    let raw = std::fs::read(&enc).unwrap();
    assert_eq!(&raw[0..4], b"ZDKC");
    assert!(!raw.windows(18).any(|w| w == b"access-token-value"));

    // A fresh open (new process) reads the same file with the same key.
    let again = store::open(StoreSelector::File, &p, &env).unwrap();
    assert_eq!(again.list_profiles().unwrap(), vec!["default", "staging"]);
    match again.load("staging").unwrap().unwrap() {
        Credential::OAuth(t) => {
            assert_eq!(t.subdomain, "acme-staging");
            assert_eq!(t.access_token.expose_secret(), "access-token-value");
            assert_eq!(t.scopes, vec!["tickets:read", "users:read"]);
        }
        Credential::ApiToken { .. } => panic!("expected an OAuth credential"),
    }
    assert!(again.delete("default").unwrap());
    assert!(!again.delete("default").unwrap());
    assert_eq!(again.list_profiles().unwrap(), vec!["staging"]);
}

#[test]
fn file_store_with_passphrase_rejects_the_wrong_one() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let env = EnvOverrides::from_pairs([("ZENDESK_CREDENTIALS_PASSPHRASE", "correct horse")]);

    let s = store::open(StoreSelector::File, &p, &env).unwrap();
    s.save("default", &oauth("acme")).unwrap();
    assert!(
        !p.config_dir.join(KEY_FILE).exists(),
        "no key file in passphrase mode"
    );
    assert_eq!(s.load("default").unwrap().unwrap().subdomain(), "acme");

    let wrong = EnvOverrides::from_pairs([("ZENDESK_CREDENTIALS_PASSPHRASE", "battery staple")]);
    let err = store::open(StoreSelector::File, &p, &wrong)
        .unwrap()
        .load("default")
        .unwrap_err();
    assert!(matches!(err, ZdkError::CredentialStore(_)));
    assert_eq!(err.exit_code(), 10);
    let msg = err.to_string();
    assert!(msg.contains("cannot decrypt credentials.enc"), "{msg}");
    assert!(msg.contains("passphrase"), "{msg}");

    // No passphrase at all → the header says which key mode is needed.
    let msg = store::open(StoreSelector::File, &p, &EnvOverrides::default())
        .unwrap()
        .load("default")
        .unwrap_err()
        .to_string();
    assert!(msg.contains("ZENDESK_CREDENTIALS_PASSPHRASE"), "{msg}");
}

#[test]
fn env_store_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let env = EnvOverrides::from_pairs([
        ("ZENDESK_EMAIL", "me@acme.com"),
        ("ZENDESK_API_TOKEN", "tok"),
        ("ZENDESK_SUBDOMAIN", "acme"),
    ]);
    let s = store::open(StoreSelector::Env, &p, &env).unwrap();
    assert_eq!(s.kind(), StoreKind::Env);
    match s.load("whatever").unwrap().unwrap() {
        Credential::ApiToken {
            email,
            token,
            subdomain,
        } => {
            assert_eq!(email, "me@acme.com");
            assert_eq!(token.expose_secret(), "tok");
            assert_eq!(subdomain, "acme");
        }
        Credential::OAuth(_) => panic!("expected an API-token credential"),
    }
    let err = s.save("default", &oauth("acme")).unwrap_err();
    assert!(matches!(err, ZdkError::Config(_)));
    assert!(err.to_string().contains("read-only"));
    assert!(s.delete("default").is_err());
    assert!(
        dir.path().read_dir().unwrap().next().is_none(),
        "nothing written"
    );

    let empty = store::open(StoreSelector::Env, &p, &EnvOverrides::default()).unwrap();
    assert!(empty.load("default").unwrap().is_none());
}

#[test]
fn open_none_yields_a_memory_store() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let s = store::open(StoreSelector::None, &p, &EnvOverrides::default()).unwrap();
    assert_eq!(s.kind(), StoreKind::Memory);
    s.save("default", &oauth("acme")).unwrap();
    assert!(s.load("default").unwrap().is_some());
    assert!(
        dir.path().read_dir().unwrap().next().is_none(),
        "nothing written"
    );
}

#[test]
fn auto_falls_back_to_file_when_the_keyring_probe_fails() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let env = EnvOverrides::default();
    let no_bus = || {
        Err(ZdkError::CredentialStore(
            "no keyring is available on this system".into(),
        ))
    };
    let s = store::open_with_probe(StoreSelector::Auto, &p, &env, false, &no_bus).unwrap();
    assert_eq!(s.kind(), StoreKind::File);
    s.save("default", &oauth("acme")).unwrap();
    assert!(p.config_dir.join(CREDENTIALS_FILE).exists());

    let decision = StoreDecision::read(&p).expect("cached");
    assert_eq!(decision.backend, StoreKind::File);
    assert!(decision.reason.unwrap().contains("no keyring"));
    assert_mode_0600(&p.state_dir.join(store::DECISION_FILE));

    // The cached decision is reused without probing.
    let must_not_probe = || panic!("cached decision must skip the probe");
    let s = store::open_with_probe(StoreSelector::Auto, &p, &env, false, &must_not_probe).unwrap();
    assert_eq!(s.kind(), StoreKind::File);
    assert_eq!(s.load("default").unwrap().unwrap().subdomain(), "acme");
}
