#!/usr/bin/env bash
# Prepare a dir-shuffle release: bump the version, run the same checks as CI, commit,
# tag and push. Pushing the tag starts .github/workflows/release.yml, which builds the
# wheels and publishes them to PyPI.
#
#   ./prepare-release.sh <version> [options]
#
#   <version>   the new version, e.g. 0.2.0 (a leading v is fine: v0.2.0). Pre-releases:
#               0.2.0-alpha.1, 0.2.0-beta.1, 0.2.0-rc.1.
#               Shortcuts: patch, minor, major bump that part of the current version
#               (0.1.0 -> 0.1.1 / 0.2.0 / 1.0.0).
#
# Examples:
#   ./prepare-release.sh 0.2.0 --dry-run   # try it: all checks, then everything is put back
#   ./prepare-release.sh 0.2.0             # release 0.2.0 (asks before committing and pushing)
#
# Options:
#   -n, --dry-run   bump and run all checks, then put everything back (no commit, no tag)
#   -y, --yes       don't ask before committing, tagging and pushing
#       --no-push   commit and tag locally, but don't push
#   -h, --help      show this help
#
# What it does, in order (it stops at the first problem, and restores Cargo.toml and
# Cargo.lock if it stops before committing):
#   1. Checks that you are on `main`, the working tree is clean, `main` is not behind
#      origin, the tag doesn't exist yet (locally or on origin), and the four PDFium pins
#      agree.
#   2. Writes the new version into Cargo.toml and lets `cargo check` update Cargo.lock.
#   3. Runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
#      `cargo test`, the same checks as CI.
#   4. Commits "Release X.Y.Z" and creates an annotated tag vX.Y.Z whose message lists
#      the commits since the previous tag.
#   5. Pushes `main` and the tag together (`git push --atomic`), so either both arrive or
#      neither does.
#
# This works with macOS's /bin/bash 3.2 and BSD userland tools.
set -euo pipefail

REMOTE=origin
BRANCH=main

usage() { sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0"; }
die() { printf '\nerror: %s\n' "$*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }

# ask "question": succeeds on yes. --yes answers yes, and no terminal means no.
ask() {
  if [ "$YES" = 1 ]; then return 0; fi
  if [ ! -t 0 ]; then die "stdin is not a terminal; pass --yes to confirm non-interactively"; fi
  local reply
  read -r -p "$1 [y/N] " reply
  case "$reply" in y | Y | yes | Yes | YES) return 0 ;; *) return 1 ;; esac
}

# ----------------------------------------------------------------------------------------
# Versions

SEMVER_RE='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-(alpha|beta|rc)\.(0|[1-9][0-9]*))?$'

# A string that sorts like the version: major.minor.patch, then pre-release
# (alpha < beta < rc < final release), then the pre-release number.
version_key() {
  [[ $1 =~ $SEMVER_RE ]] || return 1
  local rank=4 num=${BASH_REMATCH[6]:-0}
  case "${BASH_REMATCH[5]}" in alpha) rank=1 ;; beta) rank=2 ;; rc) rank=3 ;; esac
  printf '%09d.%09d.%09d.%d.%09d' \
    "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}" "$rank" "$num"
}

version_gt() { # version_gt A B: succeeds if A > B
  local a b
  a=$(version_key "$1") && b=$(version_key "$2") || return 1
  [[ $a > $b ]]
}

# bump LEVEL VERSION. From a pre-release, the next release of that level is the version
# itself (0.2.0-rc.1 -> minor -> 0.2.0), as with npm and cargo-release.
bump() {
  [[ $2 =~ $SEMVER_RE ]] || die "current version '$2' is not of the form X.Y.Z[-rc.N]"
  local ma=${BASH_REMATCH[1]} mi=${BASH_REMATCH[2]} pa=${BASH_REMATCH[3]} pre=${BASH_REMATCH[4]}
  case "$1" in
    patch)
      if [ -n "$pre" ]; then echo "$ma.$mi.$pa"; else echo "$ma.$mi.$((pa + 1))"; fi ;;
    minor)
      if [ -n "$pre" ] && [ "$pa" = 0 ]; then echo "$ma.$mi.0"; else echo "$ma.$((mi + 1)).0"; fi ;;
    major)
      if [ -n "$pre" ] && [ "$pa" = 0 ] && [ "$mi" = 0 ]; then echo "$ma.0.0"; else echo "$((ma + 1)).0.0"; fi ;;
  esac
}

# The version PyPI shows (PEP 440): 0.2.0-rc.1 -> 0.2.0rc1, 0.2.0-alpha.1 -> 0.2.0a1.
pep440() {
  [[ $1 =~ $SEMVER_RE ]] || return 1
  local core="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.${BASH_REMATCH[3]}"
  case "${BASH_REMATCH[5]}" in
    alpha) echo "${core}a${BASH_REMATCH[6]}" ;;
    beta) echo "${core}b${BASH_REMATCH[6]}" ;;
    rc) echo "${core}rc${BASH_REMATCH[6]}" ;;
    *) echo "$core" ;;
  esac
}

# The `version` of the [package] table in Cargo.toml.
cargo_version() {
  awk '/^\[/ { pkg = ($0 == "[package]") }
       pkg && /^version[ \t]*=[ \t]*"/ { sub(/^version[ \t]*=[ \t]*"/, ""); sub(/".*/, ""); print; exit }' Cargo.toml
}

set_cargo_version() {
  local tmp
  tmp=$(mktemp "${TMPDIR:-/tmp}/dir-shuffle-release.XXXXXX")
  awk -v new="$1" '/^\[/ { pkg = ($0 == "[package]") }
       pkg && !done && /^version[ \t]*=/ { print "version = \"" new "\""; done = 1; next }
       { print }' Cargo.toml >"$tmp"
  cat "$tmp" >Cargo.toml # keeps the file mode, unlike mv
  rm -f "$tmp"
}

# The version recorded for our own package in Cargo.lock.
lock_version() {
  awk '/^name = "dir-shuffle"$/ { found = 1; next }
       found && /^version = "/ { sub(/^version = "/, ""); sub(/".*/, ""); print; exit }' Cargo.lock
}

# ----------------------------------------------------------------------------------------
# Arguments

LEVEL=""
DRY_RUN=0
YES=0
PUSH=1
while [ $# -gt 0 ]; do
  case "$1" in
    -n | --dry-run) DRY_RUN=1 ;;
    -y | --yes) YES=1 ;;
    --no-push) PUSH=0 ;;
    -h | --help) usage; exit 0 ;;
    -*) die "unknown option '$1' (see --help)" ;;
    *)
      [ -z "$LEVEL" ] || die "more than one version given ('$LEVEL' and '$1')"
      LEVEL=$1 ;;
  esac
  shift
done
[ -n "$LEVEL" ] || { usage; exit 2; }

command -v git >/dev/null || die "git not found"
command -v cargo >/dev/null || die "cargo not found (install Rust: https://rustup.rs)"
ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || die "not inside a git repository"
cd "$ROOT"
if ! grep -qs '^name = "dir-shuffle"$' Cargo.toml; then
  die "$ROOT doesn't look like the dir-shuffle repository"
fi

CURRENT=$(cargo_version)
[ -n "$CURRENT" ] || die "could not read the version from Cargo.toml"
case "$LEVEL" in
  patch | minor | major) NEW=$(bump "$LEVEL" "$CURRENT") ;;
  *)
    NEW=${LEVEL#[vV]}
    [[ $NEW =~ $SEMVER_RE ]] ||
      die "'$LEVEL' is not a version like 1.2.3 or 1.2.3-rc.1 (or patch, minor, major)" ;;
esac
version_gt "$NEW" "$CURRENT" || die "new version $NEW is not greater than the current $CURRENT"
TAG="v$NEW"

printf 'dir-shuffle: %s -> %s (tag %s)%s\n' "$CURRENT" "$NEW" "$TAG" \
  "$([ "$DRY_RUN" = 1 ] && echo ', dry run' || true)"

# ----------------------------------------------------------------------------------------
# 1. Preconditions

step "Checking the repository"
branch=$(git symbolic-ref --short -q HEAD || true)
[ "$branch" = "$BRANCH" ] || die "releases are made from '$BRANCH', but you are on '${branch:-a detached HEAD}'"
if [ -n "$(git status --porcelain)" ]; then
  git status --short >&2
  die "the working tree is not clean; commit or stash the changes above first"
fi
note "on $BRANCH, working tree clean"

online=1
if git fetch --quiet "$REMOTE" "$BRANCH"; then
  behind=$(git rev-list --count "HEAD..$REMOTE/$BRANCH")
  [ "$behind" = 0 ] || die "$BRANCH is $behind commit(s) behind $REMOTE/$BRANCH; run 'git pull' first"
  ahead=$(git rev-list --count "$REMOTE/$BRANCH..HEAD")
  if [ "$ahead" = 0 ]; then
    note "up to date with $REMOTE/$BRANCH"
  else
    note "$ahead unpushed commit(s) will go out with the release:"
    git log --format='      %h %s' "$REMOTE/$BRANCH..HEAD"
  fi
else
  [ "$DRY_RUN" = 1 ] || die "could not fetch from '$REMOTE' (offline?)"
  online=0
  note "WARNING: could not fetch from '$REMOTE'; skipping the remote checks (dry run)"
fi

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  die "tag $TAG already exists locally (delete it with 'git tag -d $TAG' if it is a leftover)"
fi
if [ "$online" = 1 ]; then
  rc=0
  git ls-remote --exit-code --tags "$REMOTE" "refs/tags/$TAG" >/dev/null || rc=$?
  case $rc in
    0) die "tag $TAG already exists on $REMOTE; that version was already released" ;;
    2) note "tag $TAG is free" ;;
    *) die "could not list the tags on $REMOTE" ;;
  esac
fi

# The PDFium build must agree everywhere it is pinned (see AGENTS.md).
pin_cargo=$(grep -oE 'pdfium_[0-9]+' Cargo.toml | tr -cd '0-9\n' | sort -u || true)
pin_container=$(grep -oE '^ARG PDFIUM_BUILD=[0-9]+' PiContainerfile | tr -cd '0-9\n' | sort -u || true)
pin_fetch=$(grep -oE 'PDFIUM_BUILD:-[0-9]+' packaging/fetch-pdfium.sh | tr -cd '0-9\n' | sort -u || true)
pin_ci=$(grep -oE 'chromium%2F[0-9]+' .github/workflows/release.yml | sed 's/.*%2F//' | sort -u || true)
for pin in "$pin_cargo" "$pin_container" "$pin_fetch" "$pin_ci"; do
  if [ -z "$pin" ] || [ "$pin" != "$pin_cargo" ]; then
    die "the PDFium pins disagree: Cargo.toml '${pin_cargo//$'\n'/,}', PiContainerfile" \
      "'${pin_container//$'\n'/,}', packaging/fetch-pdfium.sh '${pin_fetch//$'\n'/,}'," \
      ".github/workflows/release.yml '${pin_ci//$'\n'/,}'"
  fi
done
note "PDFium pins agree (chromium/$pin_cargo)"

# ----------------------------------------------------------------------------------------
# 2. Bump. From here until the commit, any failure restores Cargo.toml and Cargo.lock.

MODIFIED=0
restore() {
  if [ "$MODIFIED" = 1 ]; then
    git checkout -q -- Cargo.toml Cargo.lock
    printf '    restored Cargo.toml and Cargo.lock\n' >&2
  fi
}
trap restore EXIT
trap 'exit 130' INT TERM

step "Setting the version to $NEW"
MODIFIED=1
set_cargo_version "$NEW"
[ "$(cargo_version)" = "$NEW" ] || die "failed to write the version into Cargo.toml"
# The exact expression CI uses to check that the tag matches the version.
ci_version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | sed -n 1p)
[ "$ci_version" = "$NEW" ] || die "CI would read the version as '$ci_version', not '$NEW'"

cargo check --quiet --all-targets
[ "$(lock_version)" = "$NEW" ] || die "cargo did not update Cargo.lock to $NEW"
changed=$(git diff --name-only | sort | tr '\n' ' ')
[ "$changed" = "Cargo.lock Cargo.toml " ] || die "unexpected files changed: $changed"
lock_stat=$(git diff --numstat -- Cargo.lock | awk '{ print $1 " " $2 }')
if [ "$lock_stat" = "1 1" ]; then
  note "Cargo.toml and Cargo.lock updated"
else
  note "WARNING: Cargo.lock changed more than the version line ($lock_stat lines added/removed):"
  git diff --stat -- Cargo.lock | sed 's/^/      /'
fi

# ----------------------------------------------------------------------------------------
# 3. The CI checks

step "cargo fmt --check"
cargo fmt --check || die "code is not formatted; run 'cargo fmt', commit, and try again"
step "cargo clippy --all-targets --locked -- -D warnings"
cargo clippy --quiet --all-targets --locked -- -D warnings
step "cargo test --locked"
cargo test --quiet --locked

# ----------------------------------------------------------------------------------------
# 4. Commit and tag

prev_tag=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
if [ -n "$prev_tag" ]; then
  changes=$(git log --no-merges --format='- %s' "$prev_tag..HEAD")
  since="Changes since $prev_tag:"
else
  changes=$(git log --no-merges --format='- %s' HEAD)
  since="Changes:"
fi
[ -n "$changes" ] || changes="- (no changes besides the version)"
tag_message=$(printf 'dir-shuffle %s\n\n%s\n%s\n' "$NEW" "$since" "$changes")

step "Release notes (tag message)"
printf '%s\n' "$tag_message" | sed 's/^/    /'

if [ "$DRY_RUN" = 1 ]; then
  step "Dry run finished: all checks passed. Nothing was committed, tagged or pushed."
  exit 0 # the EXIT trap restores Cargo.toml and Cargo.lock
fi

printf '\n'
ask "Commit 'Release $NEW' and create tag $TAG?" || die "aborted; nothing was committed"
git commit --quiet -m "Release $NEW" -- Cargo.toml Cargo.lock
MODIFIED=0 # committed: nothing to restore any more
git tag -a "$TAG" -F - <<<"$tag_message"
note "committed $(git rev-parse --short HEAD) and tagged $TAG"

# ----------------------------------------------------------------------------------------
# 5. Push

slug=""
url=$(git remote get-url "$REMOTE" 2>/dev/null || true)
case "$url" in
  git@github.com:*) slug=${url#git@github.com:} ;;
  ssh://git@github.com/*) slug=${url#ssh://git@github.com/} ;;
  https://github.com/*) slug=${url#https://github.com/} ;;
esac
slug=${slug%.git}
actions_url=${slug:+https://github.com/$slug/actions}

undo_hint() {
  note "to undo the local release instead:  git tag -d $TAG && git reset --hard HEAD~1"
}

if [ "$PUSH" = 0 ]; then
  step "Not pushing (--no-push). When you are ready:"
  note "git push --atomic $REMOTE $BRANCH $TAG"
  undo_hint
  exit 0
fi
printf '\n'
if ! ask "Push $BRANCH and $TAG to $REMOTE? This publishes $NEW to PyPI."; then
  step "Not pushed. When you are ready:"
  note "git push --atomic $REMOTE $BRANCH $TAG"
  undo_hint
  exit 0
fi
step "Pushing"
if ! git push --atomic "$REMOTE" "$BRANCH" "$TAG"; then
  printf '\nerror: the push failed; nothing was pushed (atomic). Fix the problem and run:\n' >&2
  note "git push --atomic $REMOTE $BRANCH $TAG" >&2
  undo_hint >&2
  exit 1
fi

step "Released $TAG"
note "CI is now building the wheels and publishing to PyPI${actions_url:+: $actions_url}"
note "PyPI (after CI succeeds): https://pypi.org/project/dir-shuffle/$(pep440 "$NEW")/"
if [[ $NEW == *-* ]]; then
  note "pre-release: install it with  uv tool install --prerelease allow dir-shuffle==$(pep440 "$NEW")"
else
  note "then check:  uv tool install --force dir-shuffle && dir-shuffle <dir> --summary"
fi
