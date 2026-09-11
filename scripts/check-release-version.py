#!/usr/bin/env python3
"""Validate the release version used by the CI version guard and the auto-tag workflow.

Reads `[workspace.package].version` from the root Cargo.toml at `--base-ref` and at HEAD.

- Unchanged version: prints a note and exits 0 in both modes (a plain feature change).
- Changed version: asserts a strict SemVer increase, that every workspace member's entry in
  Cargo.lock carries the new version (`cargo update --workspace` fixes a stale lock), that
  CHANGELOG.md has a `## [X.Y.Z]` section, and (guard mode on pull requests) that the tag does
  not already exist.

When $GITHUB_ENV is set (or --github-env is passed) it appends BASE_VERSION, HEAD_VERSION,
RELEASE_TAG and VERSION_CHANGED for later workflow steps. Standard library only (tomllib).
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any


def fail(message: str) -> None:
    print(f"::error::{message}", file=sys.stderr)
    raise SystemExit(1)


def parse_toml(text: str, source: str) -> dict[str, Any]:
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        fail(f"Could not parse {source}: {exc}")
    raise AssertionError("unreachable")


def load_workspace_version(text: str, source: str) -> str:
    manifest = parse_toml(text, source)
    try:
        version = manifest["workspace"]["package"]["version"]
    except (KeyError, TypeError):
        fail(f"{source} has no [workspace.package].version (members must use version.workspace = true).")
    if not isinstance(version, str):
        fail(f"[workspace.package].version in {source} must be a string.")
    return version


def load_head_version() -> str:
    return load_workspace_version(Path("Cargo.toml").read_text(encoding="utf-8"), "Cargo.toml")


def load_base_version(base_ref: str) -> str:
    try:
        text = subprocess.check_output(
            ["git", "show", f"{base_ref}:Cargo.toml"],
            text=True,
            stderr=subprocess.PIPE,
        )
    except subprocess.CalledProcessError as exc:
        fail(f"Could not read Cargo.toml at {base_ref}: {exc.stderr.strip()}")
    return load_workspace_version(text, f"{base_ref}:Cargo.toml")


def load_workspace_member_names() -> set[str]:
    manifest = parse_toml(Path("Cargo.toml").read_text(encoding="utf-8"), "Cargo.toml")
    names: set[str] = set()
    workspace = manifest.get("workspace")
    if not isinstance(workspace, dict):
        fail("Cargo.toml has no [workspace] table.")
    for member in workspace.get("members", []):
        member_manifest_path = Path(member) / "Cargo.toml"
        try:
            member_manifest = parse_toml(member_manifest_path.read_text(encoding="utf-8"), str(member_manifest_path))
            name = member_manifest["package"]["name"]
        except (OSError, KeyError, TypeError) as exc:
            fail(f"Could not read package name from {member_manifest_path}: {exc}")
        if not isinstance(name, str):
            fail(f"Package name in {member_manifest_path} must be a string.")
        names.add(name)
    if not names:
        fail("No workspace members found in Cargo.toml.")
    return names


SEMVER_RE = re.compile(
    r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?"
)


def parse_semver(version: str) -> tuple[int, int, int, str | None]:
    match = SEMVER_RE.fullmatch(version)
    if not match:
        fail(f"Invalid Cargo SemVer version: {version}")
    major, minor, patch, prerelease = match.groups()
    return int(major), int(minor), int(patch), prerelease


def compare_prerelease(left: str | None, right: str | None) -> int:
    if left == right:
        return 0
    if left is None:
        return 1  # a release ranks above any pre-release of the same core
    if right is None:
        return -1
    left_parts = left.split(".")
    right_parts = right.split(".")
    for left_part, right_part in zip(left_parts, right_parts):
        if left_part == right_part:
            continue
        left_numeric = left_part.isdigit()
        right_numeric = right_part.isdigit()
        if left_numeric and right_numeric:
            return 1 if int(left_part) > int(right_part) else -1
        if left_numeric:
            return -1
        if right_numeric:
            return 1
        return 1 if left_part > right_part else -1
    if len(left_parts) == len(right_parts):
        return 0
    return 1 if len(left_parts) > len(right_parts) else -1


def compare_semver(left: str, right: str) -> int:
    left_major, left_minor, left_patch, left_pre = parse_semver(left)
    right_major, right_minor, right_patch, right_pre = parse_semver(right)
    left_core = (left_major, left_minor, left_patch)
    right_core = (right_major, right_minor, right_patch)
    if left_core != right_core:
        return 1 if left_core > right_core else -1
    return compare_prerelease(left_pre, right_pre)


def verify_lockfile_version(version: str) -> None:
    try:
        lockfile = tomllib.loads(Path("Cargo.lock").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        fail(f"Could not read Cargo.lock: {exc}")
    member_names = load_workspace_member_names()
    seen: set[str] = set()
    mismatches: list[str] = []
    for package in lockfile.get("package", []):
        name = package.get("name")
        if name not in member_names:
            continue
        seen.add(name)
        if package.get("version") != version:
            mismatches.append(f"{name} is {package.get('version')}")
    missing = sorted(member_names - seen)
    if missing:
        mismatches.append(f"missing from Cargo.lock: {', '.join(missing)}")
    if mismatches:
        fail(
            f"Cargo.lock is not in sync with [workspace.package] version {version} "
            f"({'; '.join(mismatches)}). Run `cargo update --workspace` and commit Cargo.lock."
        )


def verify_changelog_version(version: str) -> None:
    try:
        changelog = Path("CHANGELOG.md").read_text(encoding="utf-8")
    except OSError as exc:
        fail(f"Could not read CHANGELOG.md: {exc}")
    heading = re.compile(rf"^## \[{re.escape(version)}\]", re.MULTILINE)
    if not heading.search(changelog):
        fail(f"CHANGELOG.md is missing a `## [{version}]` release section (it becomes the release body).")


def tag_exists(tag: str) -> bool:
    result = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def write_env(path: str | None, values: dict[str, str]) -> None:
    if not path:
        return
    with open(path, "a", encoding="utf-8") as env_file:
        for key, value in values.items():
            env_file.write(f"{key}={value}\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base-ref", required=True, help="git ref to compare against (e.g. main, HEAD~1, a SHA)")
    parser.add_argument(
        "--mode",
        choices=("guard", "tagger"),
        required=True,
        help="guard: CI check on PRs and pushes; tagger: auto-tag.yml deciding whether to push a tag",
    )
    parser.add_argument("--event-name", default=os.environ.get("GITHUB_EVENT_NAME", ""), help="GitHub event name (default: $GITHUB_EVENT_NAME)")
    parser.add_argument("--github-env", default=os.environ.get("GITHUB_ENV"), help="file to append BASE_VERSION/HEAD_VERSION/RELEASE_TAG/VERSION_CHANGED to (default: $GITHUB_ENV)")
    args = parser.parse_args()

    base_version = load_base_version(args.base_ref)
    head_version = load_head_version()
    version_changed = head_version != base_version
    tag = f"v{head_version}"

    write_env(
        args.github_env,
        {
            "BASE_VERSION": base_version,
            "HEAD_VERSION": head_version,
            "RELEASE_TAG": tag,
            "VERSION_CHANGED": "true" if version_changed else "false",
        },
    )

    if not version_changed:
        print(f"Version unchanged ({head_version}); nothing to release.")
        return

    if compare_semver(head_version, base_version) <= 0:
        fail(f"[workspace.package] version must increase (base: {base_version}, head: {head_version}).")

    verify_lockfile_version(head_version)
    verify_changelog_version(head_version)

    if args.mode == "guard" and args.event_name == "pull_request" and tag_exists(tag):
        fail(f"Release tag {tag} already exists. Bump [workspace.package] version to a fresh version.")

    print(f"Release version check passed: {base_version} -> {head_version} ({tag}).")


if __name__ == "__main__":
    main()
