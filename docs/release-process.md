# Release process

`zendesk-cli` releases are **version-bump driven**. Nobody creates tags or GitHub Releases by hand:

1. A PR bumps `[workspace.package] version` in the root `Cargo.toml`, runs `cargo update --workspace`, and adds a `## [X.Y.Z]` section to `CHANGELOG.md`. CI's *Release version guard* job validates all three.
2. When that PR merges to `main`, `.github/workflows/auto-tag.yml` pushes the annotated tag `vX.Y.Z` using the `HOMEBREW_TAP_TOKEN` PAT (a tag pushed with `GITHUB_TOKEN` would not trigger any workflow).
3. The tag triggers `.github/workflows/release.yml`: pre-release gate (fmt, clippy, tests, audit, tag/CHANGELOG consistency, man pages and completions) → 7-target build matrix → GitHub Release with `checksums-sha256.txt` and build-provenance attestations → `repository_dispatch` to `osodevops/homebrew-tap` and `osodevops/scoop-bucket`, each followed by a 5-minute poll that verifies the downstream repo actually published the new checksums.

Release archives are `zdk-<tag>-<target>.tar.gz` (macOS, Linux) and `.zip` (Windows), each containing `bin/zdk[.exe]`, `share/man/man1/*.1`, `share/completions/*` and `share/doc/zendesk-cli/`.

| Target | Runner |
|---|---|
| aarch64-apple-darwin | `macos-15` |
| x86_64-apple-darwin | `macos-15-intel` |
| x86_64-unknown-linux-musl (static) | `ubuntu-24.04` |
| aarch64-unknown-linux-musl (static) | `ubuntu-24.04-arm` |
| x86_64-unknown-linux-gnu (glibc 2.35) | `ubuntu-22.04` |
| x86_64-pc-windows-msvc | `windows-2025` |
| aarch64-pc-windows-msvc | `windows-11-arm` (fallback: cross-compile on `windows-2025`, see the commented matrix entry) |

Runner labels are explicit, never `-latest`. Check GitHub's runner deprecation notices quarterly (`macos-14` retires 2 Nov 2026).

## Secrets

| Secret | Used by | Must be able to |
|---|---|---|
| `HOMEBREW_TAP_TOKEN` (PAT) | `auto-tag.yml`, `release.yml` (`homebrew` job), `spec-refresh.yml` | push tags to `osodevops/zendesk-cli`; `repository_dispatch` + read `osodevops/homebrew-tap`; open PRs on `osodevops/zendesk-cli` |
| `SCOOP_BUCKET_TOKEN` (PAT) | `release.yml` (`scoop` job) | `repository_dispatch` + read `osodevops/scoop-bucket` |
| `ZDK_E2E_SUBDOMAIN` / `ZDK_E2E_CLIENT_ID` / `ZDK_E2E_CLIENT_SECRET` | `e2e.yml` | client-credentials login on a test Zendesk instance (optional; the workflow skips itself when absent) |

Both PATs are org-level secrets. If `gh secret list -R osodevops/zendesk-cli` shows nothing, an org admin must either widen the org secrets to this repository or set repo-level copies:

```bash
REPO_ID=$(gh api repos/osodevops/zendesk-cli --jq .id)
gh api -X PUT orgs/osodevops/actions/secrets/HOMEBREW_TAP_TOKEN/repositories/$REPO_ID
gh api -X PUT orgs/osodevops/actions/secrets/SCOOP_BUCKET_TOKEN/repositories/$REPO_ID
# or: gh secret set HOMEBREW_TAP_TOKEN -R osodevops/zendesk-cli ; gh secret set SCOOP_BUCKET_TOKEN -R osodevops/zendesk-cli
```

`gh workflow run release-preflight.yml -R osodevops/zendesk-cli` proves both PATs are visible, can write where they must, and that the tap and bucket are wired for `zdk`. Run it before every release; it publishes nothing.

## First release (v0.1.0) runbook

### (a) GitHub pre-flight (once)

```bash
gh repo edit osodevops/zendesk-cli \
  --description "Zendesk CLI for AI agents and support operations — OAuth-first, full API coverage, structured output (binary: zdk)" \
  --homepage "https://github.com/osodevops/zendesk-cli" \
  --add-topic zendesk --add-topic cli --add-topic rust --add-topic ai-agents --add-topic oauth \
  --enable-issues --enable-wiki=false --delete-branch-on-merge --allow-squash-merge --enable-merge-commit=false
gh api -X PUT repos/osodevops/zendesk-cli/vulnerability-alerts
gh api -X PUT repos/osodevops/zendesk-cli/automated-security-fixes
```

Then make the secrets visible (see above).

### (b) Downstream PRs — merge **before** the release PR

- **Homebrew tap** (`osodevops/homebrew-tap`), branch `feat/zendesk-cli-formula`: `Formula/zendesk-cli.rb` (placeholder `v0.0.0` URLs and all-zero SHA-256s — the updater needs exactly one `url`/`sha256` pair per target to rewrite), `.github/workflows/update-zendesk-cli-formula.yml`, `scripts/update-zendesk-cli-formula.rb`, README row. `brew install osodevops/tap/zendesk-cli` 404s until v0.1.0 lands; that is expected. The tap workflow refuses to commit a formula that still contains placeholder checksums.
- **Scoop bucket** (`osodevops/scoop-bucket`), branch `feat/zdk-manifest`: `update-manifest.yml` gains the `update-zdk-manifest` dispatch type, the `zdk` product option and the `update-zdk` job (writes `bucket/zdk.json` with `64bit` and `arm64` blocks on first run; no seed file), README row.

Done when `gh api repos/osodevops/homebrew-tap/contents/Formula/zendesk-cli.rb --jq .sha` returns a SHA and the bucket workflow on `main` contains `update-zdk-manifest`.

### (c) Initial commits

The v0.1.0 code landed on `main` as five phase commits at version `0.0.0` (scaffold/config/output/registry codegen; credential stores and OAuth engine; HTTP core, governor, pagination and `zdk api`; `zdk auth` and `zdk doctor`; the curated commands). The release PR is therefore a real `0.0.0 -> 0.1.0` bump that exercises guard → auto-tag → release end to end. Before opening it, prove the static musl build locally — with rustup's cargo first on `PATH`, because Homebrew's cargo shadows it on macOS:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
rustup run 1.88.0 cargo check --workspace --all-targets --locked     # MSRV (not `cargo +1.88.0`)
cross build --release --target x86_64-unknown-linux-musl -p zendesk-cli
file target/x86_64-unknown-linux-musl/release/zdk                     # "statically linked"
```

### (d) Watch CI and preflight

```bash
gh run watch -R osodevops/zendesk-cli --exit-status <ci-run-id>
gh workflow run release-preflight.yml -R osodevops/zendesk-cli && sleep 5 && gh run watch -R osodevops/zendesk-cli --exit-status
```

Done when `CI Complete` is green on `main` and preflight is green.

### (e) Release

```bash
git checkout -b release/v0.1.0
# Cargo.toml: [workspace.package] version = "0.1.0"
cargo update --workspace
# CHANGELOG.md: move [Unreleased] items under "## [0.1.0] - YYYY-MM-DD"
python3 scripts/check-release-version.py --base-ref main --mode guard --event-name pull_request   # "0.0.0 -> 0.1.0 (v0.1.0)"
git commit -am "chore(release): v0.1.0"
git push -u origin HEAD
gh pr create --title "chore(release): v0.1.0" --body "Release v0.1.0. See CHANGELOG."
gh pr merge --squash --auto

# after merge
gh run watch --exit-status "$(gh run list --workflow auto-tag.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
git fetch --tags && git tag -l v0.1.0
gh run watch --exit-status "$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"   # ~15–25 min
```

Verify:

```bash
gh release view v0.1.0 -R osodevops/zendesk-cli --json assets --jq '.assets[].name'   # 7 archives + checksums-sha256.txt
mkdir -p /tmp/rel && cd /tmp/rel
gh release download v0.1.0 -R osodevops/zendesk-cli -p 'zdk-v0.1.0-aarch64-apple-darwin.tar.gz' -p checksums-sha256.txt
sha256sum -c --ignore-missing checksums-sha256.txt
gh attestation verify zdk-v0.1.0-aarch64-apple-darwin.tar.gz --owner osodevops
brew update && brew install osodevops/tap/zendesk-cli && zdk --version && brew test zendesk-cli   # "zdk 0.1.0"
gh api "repos/osodevops/homebrew-tap/commits?path=Formula/zendesk-cli.rb" --jq '.[0].commit.message'   # "Update zendesk-cli to v0.1.0"
gh api repos/osodevops/scoop-bucket/contents/bucket/zdk.json -H "Accept: application/vnd.github.raw+json" | jq '{version, hash: .architecture["64bit"].hash}'
```

Subsequent releases follow the same (e) steps with the new version.

## Rollback and recovery

| Situation | What exists | Action |
|---|---|---|
| Version guard fails on the release PR | nothing | Fix the PR (usually `cargo update --workspace` or the missing `## [X.Y.Z]` CHANGELOG section). |
| `auto-tag` fails (empty PAT, push rejected) | merged bump, no tag | Fix the secret, then push the tag by hand: `git tag -a vX.Y.Z -m "Release vX.Y.Z" <merge-sha> && git push origin vX.Y.Z`. A user push triggers `release.yml`. |
| Pre-release gate or a build job fails | tag only | Fix via PR (not a new version). `gh release delete vX.Y.Z --cleanup-tag --yes` if a draft exists, then re-tag at the fixed commit as above. |
| `release` job fails mid-upload | partial release | Same as above: delete the release with `--cleanup-tag`, re-tag at the fixed commit. |
| Tap or bucket poll times out, release is live | full release, stale formula/manifest | `gh workflow run update-zendesk-cli-formula.yml -R osodevops/homebrew-tap -f tag=vX.Y.Z` / `gh workflow run update-manifest.yml -R osodevops/scoop-bucket -f tag=vX.Y.Z -f product=zdk`, then `gh run rerun <release-run-id> --failed`. |
| Re-publish everything for an existing tag | full release | `gh workflow run release.yml -R osodevops/zendesk-cli -f tag=vX.Y.Z` (re-uploads assets to the same release; never use it to *create* a release). |
| A bad binary is already distributed | full release, users installed it | **Never delete or overwrite.** Ship `X.Y.Z+1` with the fix and note the bad version in CHANGELOG.md. |

## Pipeline risks and mitigations

| Risk | Mitigation |
|---|---|
| PAT expiry silently stops tagging | `release-preflight.yml`; `auto-tag` fails loudly on an empty secret; manual tag push recovery above |
| Runner label retirement | explicit labels; quarterly check (noted in CLAUDE.md) |
| `windows-11-arm` lacks rustup/VS components | commented fallback matrix entry cross-compiles on `windows-2025` with `no_smoke: true` |
| Attestation permission/visibility | explicit job permissions (`id-token`, `attestations`); self-verify step before publishing; requires a public repo |
| Tap/bucket slow → red release after publish | 5-minute polls print the exact re-run commands; preflight proves both repos are wired |
| Stale `Cargo.lock` blocks the release PR | guard error names `cargo update --workspace`; PR template lists it |
| musl binary accidentally dynamic | `ring` TLS provider, pure-Rust keyring backends; `file` check fails the build job |
| `spec-refresh` PRs do not trigger CI | `create-pull-request` uses the PAT, not `GITHUB_TOKEN` |

## Branch protection (optional)

Ruleset on `main`: no deletion, no force-push, PRs required (0 approvals, squash only), required status check `CI Complete` (strict), repository-admin bypass. Tags are unaffected, so `auto-tag` keeps working.
