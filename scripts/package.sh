#!/usr/bin/env bash
# Package a zdk release archive with the layout the Homebrew formula and Scoop manifest expect.
#
#   scripts/package.sh --tag v0.1.0 --target aarch64-apple-darwin --bin target/release/zdk \
#                      --docs dist/docs [--out dist] [--format tar.gz|zip]
#
# Produces <out>/zdk-<tag>-<target>.<format> containing:
#   zdk-<tag>-<target>/
#     bin/zdk[.exe]                       (0755)
#     share/man/man1/*.1                  (from <docs>/man1)
#     share/completions/*                 (from <docs>/completions)
#     share/doc/zendesk-cli/{README.md,CHANGELOG.md,LICENSE,SECURITY.md}
#
# Used by release.yml on every matrix runner (bash on Windows too) and for local dry-runs.
# Prints the archive path on stdout; everything else goes to stderr.
set -euo pipefail

BIN_NAME="zdk"
PACKAGE_NAME="zendesk-cli"
DOC_FILES=(README.md CHANGELOG.md LICENSE SECURITY.md)

usage() {
  sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

die() {
  echo "package.sh: $*" >&2
  exit 1
}

tag=""
target=""
bin=""
docs=""
out="dist"
format=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)    tag="${2:-}"; shift 2 ;;
    --target) target="${2:-}"; shift 2 ;;
    --bin)    bin="${2:-}"; shift 2 ;;
    --docs)   docs="${2:-}"; shift 2 ;;
    --out)    out="${2:-}"; shift 2 ;;
    --format) format="${2:-}"; shift 2 ;;
    -h|--help) usage ;;
    *) echo "package.sh: unknown argument: $1" >&2; usage ;;
  esac
done

[[ -n "$tag" ]]    || die "--tag is required"
[[ -n "$target" ]] || die "--target is required"
[[ -n "$bin" ]]    || die "--bin is required"
[[ -n "$docs" ]]   || die "--docs is required"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || die "--tag must look like v0.1.0 (got: $tag)"
[[ "$target" =~ ^[A-Za-z0-9_.-]+$ ]] || die "--target contains unexpected characters: $target"

case "$target" in
  *windows*) bin_file="${BIN_NAME}.exe"; default_format="zip" ;;
  *)         bin_file="${BIN_NAME}";     default_format="tar.gz" ;;
esac
format="${format:-$default_format}"
case "$format" in
  tar.gz|zip) ;;
  *) die "--format must be tar.gz or zip (got: $format)" ;;
esac

[[ -f "$bin" ]] || die "binary not found: $bin"
[[ -d "$docs" ]] || die "docs directory not found: $docs"
[[ -s "$docs/man1/${BIN_NAME}.1" ]] || die "refusing to package without $docs/man1/${BIN_NAME}.1 (run: $BIN_NAME man --to $docs/man1)"
[[ -d "$docs/completions" ]] || die "missing $docs/completions (run: $BIN_NAME completions <shell>)"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
for f in "${DOC_FILES[@]}"; do
  [[ -f "$repo_root/$f" ]] || die "missing $repo_root/$f"
done

name="${BIN_NAME}-${tag}-${target}"
stage="$out/$name"
archive="$out/$name.$format"

rm -rf "$stage" "$archive"
mkdir -p "$stage/bin" "$stage/share/man/man1" "$stage/share/completions" "$stage/share/doc/$PACKAGE_NAME"

cp "$bin" "$stage/bin/$bin_file"
chmod 0755 "$stage/bin/$bin_file"

man_count=0
for page in "$docs"/man1/*.1; do
  [[ -s "$page" ]] || die "empty man page: $page"
  cp "$page" "$stage/share/man/man1/"
  man_count=$((man_count + 1))
done
[[ "$man_count" -ge 1 ]] || die "no man pages in $docs/man1"

completion_count=0
for comp in "$docs"/completions/*; do
  [[ -f "$comp" ]] || continue
  [[ -s "$comp" ]] || die "empty completion file: $comp"
  cp "$comp" "$stage/share/completions/"
  completion_count=$((completion_count + 1))
done
[[ "$completion_count" -ge 1 ]] || die "no completion files in $docs/completions"

for f in "${DOC_FILES[@]}"; do
  cp "$repo_root/$f" "$stage/share/doc/$PACKAGE_NAME/"
done

# macOS: no AppleDouble (._*) entries in the archive.
export COPYFILE_DISABLE=1

case "$format" in
  tar.gz)
    tar -C "$out" -czf "$archive" "$name"
    ;;
  zip)
    archive_base="$name.$format"
    if command -v 7z >/dev/null 2>&1; then
      (cd "$out" && 7z a -tzip -bd -y "$archive_base" "$name" >/dev/null)
    elif command -v zip >/dev/null 2>&1; then
      (cd "$out" && zip -r -q "$archive_base" "$name")
    elif command -v pwsh >/dev/null 2>&1 || command -v powershell >/dev/null 2>&1; then
      ps="$(command -v pwsh || command -v powershell)"
      (cd "$out" && "$ps" -NoProfile -NonInteractive -Command "Compress-Archive -Path '$name' -DestinationPath '$archive_base' -Force")
    else
      die "no zip tool found (need 7z, zip, or PowerShell)"
    fi
    ;;
esac

[[ -s "$archive" ]] || die "archive was not created: $archive"
rm -rf "$stage"

echo "packaged $name ($man_count man page(s), $completion_count completion file(s))" >&2
echo "$archive"
