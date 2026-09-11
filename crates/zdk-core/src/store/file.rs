//! Encrypted-file credential store: `~/.config/zendesk-cli/credentials.enc`.
//!
//! Chosen automatically when no OS keyring is usable (Docker, CI, SSH) or explicitly with
//! `--credential-store file`. Never prompts.
//!
//! ```text
//! offset  size  field
//! 0       4     magic "ZDKC"
//! 4       1     format version (0x01)
//! 5       1     key mode: 0x01 passphrase (Argon2id), 0x02 machine key file
//! 6       4     Argon2id m_cost (KiB, little-endian; 0 in key-file mode)
//! 10      4     Argon2id t_cost (little-endian; 0 in key-file mode)
//! 14      4     Argon2id p_cost (little-endian; 0 in key-file mode)
//! 18      16    salt (random per write; unused in key-file mode but always present)
//! 34      24    XChaCha20-Poly1305 nonce (random per write)
//! 58      …     ciphertext + tag of the JSON map `{ "<profile>": Credential }`
//! ```
//!
//! The 58 header bytes are the AEAD's additional authenticated data, so a tampered header
//! (or mode byte) fails authentication just like a tampered body.
//!
//! Key: `ZENDESK_CREDENTIALS_PASSPHRASE` → Argon2id (m = 64 MiB, t = 3, p = 1) → 32 bytes;
//! otherwise `credentials.key` next to the file (32 random bytes, `0600`, created on first
//! save) is used directly.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use super::{CredentialStore, StoreKind};
use crate::auth::token::Credential;
use crate::config::{EnvOverrides, Paths};
use crate::util::fs::atomic_write_0600;
use crate::{Result, ZdkError};

/// File name inside the config directory.
pub const CREDENTIALS_FILE: &str = "credentials.enc";
/// Machine key file name inside the config directory.
pub const KEY_FILE: &str = "credentials.key";

const MAGIC: &[u8; 4] = b"ZDKC";
const VERSION: u8 = 0x01;
const MODE_PASSPHRASE: u8 = 0x01;
const MODE_KEYFILE: u8 = 0x02;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = 4 + 1 + 1 + 12 + SALT_LEN + NONCE_LEN;

/// The cipher for 32 raw key bytes (no intermediate copy of the key is made).
fn cipher_for(key: &[u8; KEY_LEN]) -> Option<XChaCha20Poly1305> {
    XChaCha20Poly1305::new_from_slice(key).ok()
}

/// The nonce for a slice of exactly `NONCE_LEN` bytes.
fn as_nonce(bytes: &[u8]) -> Option<XNonce> {
    <[u8; NONCE_LEN]>::try_from(bytes).ok().map(XNonce::from)
}

/// Upper bounds accepted when reading a header, so a corrupt file cannot make us allocate
/// gigabytes or spin for minutes in the KDF.
const MAX_M_COST_KIB: u32 = 1 << 20; // 1 GiB
const MAX_T_COST: u32 = 64;
const MAX_P_COST: u32 = 16;

/// Argon2id cost parameters written into the header (passphrase mode only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl KdfParams {
    /// Production strength: 64 MiB, 3 passes, 1 lane (OWASP 2024 guidance for Argon2id).
    pub const DEFAULT: Self = Self {
        m_cost: 65_536,
        t_cost: 3,
        p_cost: 1,
    };
}

/// Where the encryption key comes from.
#[derive(Clone)]
pub enum KeySource {
    /// `ZENDESK_CREDENTIALS_PASSPHRASE`, stretched with Argon2id.
    Passphrase(SecretString),
    /// 32 raw bytes in this file (auto-created on first save).
    KeyFile(PathBuf),
}

impl KeySource {
    fn mode(&self) -> u8 {
        match self {
            Self::Passphrase(_) => MODE_PASSPHRASE,
            Self::KeyFile(_) => MODE_KEYFILE,
        }
    }

    /// Human name for messages / `doctor`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Passphrase(_) => "passphrase",
            Self::KeyFile(_) => "machine key file",
        }
    }
}

impl fmt::Debug for KeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Passphrase(_) => f.write_str("Passphrase([redacted])"),
            Self::KeyFile(p) => f.debug_tuple("KeyFile").field(p).finish(),
        }
    }
}

/// The encrypted-file [`CredentialStore`].
#[derive(Debug)]
pub struct FileStore {
    path: PathBuf,
    key: KeySource,
    kdf: KdfParams,
}

impl FileStore {
    /// `credentials.enc` in the config directory; passphrase from the environment when set,
    /// otherwise the machine key file next to it.
    #[must_use]
    pub fn new(paths: &Paths, env: &EnvOverrides) -> Self {
        let key = match &env.credentials_passphrase {
            Some(p) => KeySource::Passphrase(p.clone()),
            None => KeySource::KeyFile(paths.config_dir.join(KEY_FILE)),
        };
        Self::at(paths.config_dir.join(CREDENTIALS_FILE), key)
    }

    /// An explicit file and key source (tests, `doctor`).
    #[must_use]
    pub fn at(path: PathBuf, key: KeySource) -> Self {
        Self {
            path,
            key,
            kdf: KdfParams::DEFAULT,
        }
    }

    /// Override the Argon2id parameters used for *new* writes (tests use small values;
    /// reads always honour the header).
    #[must_use]
    pub fn with_kdf_params(mut self, kdf: KdfParams) -> Self {
        self.kdf = kdf;
        self
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn key_source(&self) -> &KeySource {
        &self.key
    }

    fn err(&self, msg: impl fmt::Display) -> ZdkError {
        ZdkError::CredentialStore(format!("{}: {msg}", self.path.display()))
    }

    fn undecryptable(&self, required_mode: u8) -> ZdkError {
        let required = match required_mode {
            MODE_PASSPHRASE => {
                "the passphrase it was encrypted with (ZENDESK_CREDENTIALS_PASSPHRASE)"
            }
            _ => "the machine key file it was encrypted with (credentials.key next to it)",
        };
        let have = match (&self.key, required_mode) {
            (KeySource::Passphrase(_), MODE_PASSPHRASE) => {
                "the passphrase is wrong or the file was modified".to_string()
            }
            (KeySource::KeyFile(p), MODE_KEYFILE) => format!(
                "{} does not match or the file was modified",
                p.display()
            ),
            (KeySource::Passphrase(_), _) => {
                "ZENDESK_CREDENTIALS_PASSPHRASE is set but the file was not encrypted with a passphrase — unset it"
                    .to_string()
            }
            (KeySource::KeyFile(_), _) => {
                "it needs ZENDESK_CREDENTIALS_PASSPHRASE, which is not set".to_string()
            }
        };
        self.err(format!(
            "cannot decrypt credentials.enc: it requires {required}; {have}. \
             Fix the key, or delete the file and run `zdk auth login` again"
        ))
    }

    // -- key material ---------------------------------------------------------------------

    fn derive_passphrase_key(
        &self,
        passphrase: &SecretString,
        params: KdfParams,
        salt: &[u8],
    ) -> Result<Zeroizing<[u8; KEY_LEN]>> {
        let p = argon2::Params::new(params.m_cost, params.t_cost, params.p_cost, Some(KEY_LEN))
            .map_err(|e| self.err(format!("invalid Argon2id parameters: {e}")))?;
        let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, p);
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        argon
            .hash_password_into(passphrase.expose_secret().as_bytes(), salt, key.as_mut())
            .map_err(|e| self.err(format!("key derivation failed: {e}")))?;
        Ok(key)
    }

    fn read_key_file(&self, path: &Path) -> Result<Option<Zeroizing<[u8; KEY_LEN]>>> {
        let bytes = match std::fs::read(path) {
            Ok(b) => Zeroizing::new(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(self.err(format!("cannot read key file {}: {e}", path.display())));
            }
        };
        if bytes.len() != KEY_LEN {
            return Err(self.err(format!(
                "key file {} must be exactly {KEY_LEN} bytes (found {})",
                path.display(),
                bytes.len()
            )));
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&bytes);
        Ok(Some(key))
    }

    fn load_or_create_key_file(&self, path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>> {
        if let Some(key) = self.read_key_file(path)? {
            return Ok(key);
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        getrandom::fill(key.as_mut())
            .map_err(|e| self.err(format!("cannot generate a machine key: {e}")))?;
        atomic_write_0600(path, key.as_ref())
            .map_err(|e| self.err(format!("cannot write key file {}: {e}", path.display())))?;
        Ok(key)
    }

    /// Key for decrypting a file whose header says `mode`/`params`/`salt`.
    fn key_for_read(
        &self,
        mode: u8,
        params: KdfParams,
        salt: &[u8],
    ) -> Result<Zeroizing<[u8; KEY_LEN]>> {
        match (&self.key, mode) {
            (KeySource::Passphrase(p), MODE_PASSPHRASE) => {
                self.derive_passphrase_key(p, params, salt)
            }
            (KeySource::KeyFile(path), MODE_KEYFILE) => self
                .read_key_file(path)?
                .ok_or_else(|| self.undecryptable(MODE_KEYFILE)),
            (_, required) => Err(self.undecryptable(required)),
        }
    }

    // -- container -------------------------------------------------------------------------

    fn read_map(&self) -> Result<BTreeMap<String, Credential>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(self.err(format!("cannot read: {e}"))),
        };
        if bytes.len() < HEADER_LEN || &bytes[0..4] != MAGIC {
            return Err(self.err(
                "not a zdk credentials file (bad magic); delete it and run `zdk auth login` again",
            ));
        }
        let (header, body) = bytes.split_at(HEADER_LEN);
        if header[4] != VERSION {
            return Err(self.err(format!(
                "unsupported credentials file version {} (this build writes {VERSION})",
                header[4]
            )));
        }
        let mode = header[5];
        if mode != MODE_PASSPHRASE && mode != MODE_KEYFILE {
            return Err(self.err(format!("unknown key mode 0x{mode:02x} in header")));
        }
        let u32_at =
            |i: usize| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
        let params = KdfParams {
            m_cost: u32_at(6),
            t_cost: u32_at(10),
            p_cost: u32_at(14),
        };
        if mode == MODE_PASSPHRASE
            && (params.m_cost > MAX_M_COST_KIB
                || params.t_cost > MAX_T_COST
                || params.p_cost > MAX_P_COST)
        {
            return Err(self.err(format!(
                "refusing Argon2id parameters from header (m={} KiB, t={}, p={}): above the supported bounds",
                params.m_cost, params.t_cost, params.p_cost
            )));
        }
        let salt = &header[18..18 + SALT_LEN];
        let nonce = &header[18 + SALT_LEN..HEADER_LEN];

        let key = self.key_for_read(mode, params, salt)?;
        let nonce = as_nonce(nonce).ok_or_else(|| self.err("malformed header nonce"))?;
        let cipher = cipher_for(&key).ok_or_else(|| self.err("invalid key length"))?;
        let plain = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: body,
                    aad: header,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| self.undecryptable(mode))?;
        serde_json::from_slice(&plain)
            .map_err(|e| self.err(format!("decrypted content is not a credential map: {e}")))
    }

    fn write_map(&self, map: &BTreeMap<String, Credential>) -> Result<()> {
        let plain = Zeroizing::new(serde_json::to_vec(map)?);

        let mut salt = [0u8; SALT_LEN];
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut salt).map_err(|e| self.err(format!("cannot draw a salt: {e}")))?;
        getrandom::fill(&mut nonce).map_err(|e| self.err(format!("cannot draw a nonce: {e}")))?;

        let (key, params) = match &self.key {
            KeySource::Passphrase(p) => (self.derive_passphrase_key(p, self.kdf, &salt)?, self.kdf),
            KeySource::KeyFile(path) => (
                self.load_or_create_key_file(path)?,
                KdfParams {
                    m_cost: 0,
                    t_cost: 0,
                    p_cost: 0,
                },
            ),
        };

        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(MAGIC);
        header.push(VERSION);
        header.push(self.key.mode());
        header.extend_from_slice(&params.m_cost.to_le_bytes());
        header.extend_from_slice(&params.t_cost.to_le_bytes());
        header.extend_from_slice(&params.p_cost.to_le_bytes());
        header.extend_from_slice(&salt);
        header.extend_from_slice(&nonce);
        debug_assert_eq!(header.len(), HEADER_LEN);

        let cipher = cipher_for(&key).ok_or_else(|| self.err("invalid key length"))?;
        let body = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plain,
                    aad: &header,
                },
            )
            .map_err(|_| self.err("encryption failed"))?;

        let mut out = header;
        out.extend_from_slice(&body);
        atomic_write_0600(&self.path, &out).map_err(|e| self.err(format!("cannot write: {e}")))
    }
}

impl CredentialStore for FileStore {
    fn kind(&self) -> StoreKind {
        StoreKind::File
    }

    fn load(&self, profile: &str) -> Result<Option<Credential>> {
        Ok(self.read_map()?.remove(profile))
    }

    fn save(&self, profile: &str, cred: &Credential) -> Result<()> {
        let mut map = self.read_map()?;
        map.insert(profile.to_string(), cred.clone());
        self.write_map(&map)
    }

    fn delete(&self, profile: &str) -> Result<bool> {
        let mut map = self.read_map()?;
        let existed = map.remove(profile).is_some();
        if existed {
            self.write_map(&map)?;
        }
        Ok(existed)
    }

    fn list_profiles(&self) -> Result<Vec<String>> {
        Ok(self.read_map()?.into_keys().collect())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::auth::GrantKind;
    use crate::auth::token::TokenSet;

    /// Small enough to keep the suite fast; the header records whatever was used.
    const FAST: KdfParams = KdfParams {
        m_cost: 64,
        t_cost: 1,
        p_cost: 1,
    };

    fn cred(sub: &str) -> Credential {
        Credential::OAuth(TokenSet {
            access_token: SecretString::from("access-secret".to_string()),
            token_type: "bearer".into(),
            scopes: vec!["tickets:read".into()],
            obtained_at: Utc::now(),
            expires_at: None,
            refresh_token: Some(SecretString::from("refresh-secret".to_string())),
            refresh_expires_at: None,
            grant: GrantKind::AuthorizationCode,
            subdomain: sub.into(),
            client_id: "zdk".into(),
            client_secret: None,
        })
    }

    fn passphrase_store(dir: &Path, pw: &str) -> FileStore {
        FileStore::at(
            dir.join(CREDENTIALS_FILE),
            KeySource::Passphrase(SecretString::from(pw.to_string())),
        )
        .with_kdf_params(FAST)
    }

    fn keyfile_store(dir: &Path) -> FileStore {
        FileStore::at(
            dir.join(CREDENTIALS_FILE),
            KeySource::KeyFile(dir.join(KEY_FILE)),
        )
    }

    #[test]
    fn passphrase_round_trip_list_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let s = passphrase_store(dir.path(), "correct horse");
        assert!(s.load("work").unwrap().is_none());
        assert!(s.list_profiles().unwrap().is_empty());

        s.save("work", &cred("acme")).unwrap();
        s.save("home", &cred("home")).unwrap();
        assert_eq!(s.list_profiles().unwrap(), vec!["home", "work"]);
        assert_eq!(s.load("work").unwrap().unwrap().subdomain(), "acme");

        // Ciphertext never contains the plaintext secrets.
        let raw = std::fs::read(s.path()).unwrap();
        assert_eq!(&raw[0..4], b"ZDKC");
        assert_eq!(raw[4], VERSION);
        assert_eq!(raw[5], MODE_PASSPHRASE);
        assert!(!raw.windows(13).any(|w| w == b"access-secret"));
        assert!(!raw.windows(4).any(|w| w == b"acme"));

        assert!(s.delete("work").unwrap());
        assert!(!s.delete("work").unwrap());
        assert_eq!(s.list_profiles().unwrap(), vec!["home"]);
    }

    #[test]
    fn keyfile_round_trip_creates_key_on_first_save() {
        let dir = tempfile::tempdir().unwrap();
        let s = keyfile_store(dir.path());
        assert!(!dir.path().join(KEY_FILE).exists());
        assert!(
            s.load("work").unwrap().is_none(),
            "no file → None, no key created"
        );
        assert!(!dir.path().join(KEY_FILE).exists());

        s.save("work", &cred("acme")).unwrap();
        let key = std::fs::read(dir.path().join(KEY_FILE)).unwrap();
        assert_eq!(key.len(), KEY_LEN);
        let raw = std::fs::read(s.path()).unwrap();
        assert_eq!(raw[5], MODE_KEYFILE);

        // A second store instance (new process) reads with the same key file.
        let again = keyfile_store(dir.path());
        assert_eq!(again.load("work").unwrap().unwrap().subdomain(), "acme");
        again.save("work", &cred("acme2")).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(KEY_FILE)).unwrap(),
            key,
            "key is stable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = keyfile_store(dir.path());
        s.save("work", &cred("acme")).unwrap();
        for name in [CREDENTIALS_FILE, KEY_FILE] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name}");
        }
    }

    #[test]
    fn wrong_passphrase_names_the_required_key_mode() {
        let dir = tempfile::tempdir().unwrap();
        passphrase_store(dir.path(), "right")
            .save("work", &cred("acme"))
            .unwrap();
        let err = passphrase_store(dir.path(), "wrong")
            .load("work")
            .unwrap_err();
        assert!(matches!(err, ZdkError::CredentialStore(_)));
        let msg = err.to_string();
        assert!(msg.contains("cannot decrypt credentials.enc"), "{msg}");
        assert!(msg.contains("ZENDESK_CREDENTIALS_PASSPHRASE"), "{msg}");
        assert_eq!(err.exit_code(), 10);
    }

    #[test]
    fn key_mode_mismatch_is_explained_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        passphrase_store(dir.path(), "pw")
            .save("work", &cred("acme"))
            .unwrap();
        let msg = keyfile_store(dir.path())
            .load("work")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("requires the passphrase"), "{msg}");
        assert!(msg.contains("not set"), "{msg}");

        let dir = tempfile::tempdir().unwrap();
        keyfile_store(dir.path())
            .save("work", &cred("acme"))
            .unwrap();
        let msg = passphrase_store(dir.path(), "pw")
            .load("work")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("requires the machine key file"), "{msg}");
        assert!(msg.contains("unset it"), "{msg}");
    }

    #[test]
    fn wrong_key_file_and_missing_key_file_fail_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let s = keyfile_store(dir.path());
        s.save("work", &cred("acme")).unwrap();
        std::fs::write(dir.path().join(KEY_FILE), [7u8; KEY_LEN]).unwrap();
        let msg = s.load("work").unwrap_err().to_string();
        assert!(msg.contains("machine key file"), "{msg}");
        std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        let msg = s.load("work").unwrap_err().to_string();
        assert!(msg.contains("machine key file"), "{msg}");
        std::fs::write(dir.path().join(KEY_FILE), [7u8; 5]).unwrap();
        let msg = s.load("work").unwrap_err().to_string();
        assert!(msg.contains("exactly 32 bytes"), "{msg}");
    }

    #[test]
    fn tampering_with_header_or_body_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let s = passphrase_store(dir.path(), "pw");
        s.save("work", &cred("acme")).unwrap();
        let original = std::fs::read(s.path()).unwrap();

        // Flip a ciphertext byte.
        let mut body = original.clone();
        let last = body.len() - 1;
        body[last] ^= 0x01;
        std::fs::write(s.path(), &body).unwrap();
        assert!(
            s.load("work")
                .unwrap_err()
                .to_string()
                .contains("cannot decrypt")
        );

        // Flip a header byte (inside the salt) — AAD covers it.
        let mut hdr = original.clone();
        hdr[20] ^= 0x01;
        std::fs::write(s.path(), &hdr).unwrap();
        assert!(
            s.load("work")
                .unwrap_err()
                .to_string()
                .contains("cannot decrypt")
        );

        // Bad magic / truncated.
        std::fs::write(s.path(), b"nope").unwrap();
        assert!(
            s.load("work")
                .unwrap_err()
                .to_string()
                .contains("bad magic")
        );

        // Absurd KDF parameters are refused before allocating.
        let mut huge = original;
        huge[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(s.path(), &huge).unwrap();
        assert!(
            s.load("work")
                .unwrap_err()
                .to_string()
                .contains("Argon2id parameters")
        );
    }

    #[test]
    fn new_picks_passphrase_when_env_has_one() {
        let paths = Paths {
            config_file: "/t/config.toml".into(),
            config_dir: "/t".into(),
            state_dir: "/t/state".into(),
            cache_dir: "/t/cache".into(),
        };
        let s = FileStore::new(
            &paths,
            &EnvOverrides::from_pairs([("ZENDESK_CREDENTIALS_PASSPHRASE", "pw")]),
        );
        assert!(matches!(s.key_source(), KeySource::Passphrase(_)));
        assert_eq!(s.path(), Path::new("/t/credentials.enc"));
        let s = FileStore::new(&paths, &EnvOverrides::default());
        assert!(
            matches!(s.key_source(), KeySource::KeyFile(p) if p == Path::new("/t/credentials.key"))
        );
        assert!(!format!("{s:?}").contains("pw"));
    }
}
