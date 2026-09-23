set positional-arguments
set no-exit-message

just := quote(just_executable()) + " --justfile " + quote(justfile())

[private]
default:
    @{{ just }} --list

# Print a version's CHANGELOG.md section without its heading (also takes Unreleased)
changelog-notes version:
    #!/usr/bin/env bash
    set -euo pipefail
    notes=$({{ just }} _changelog "$1")
    if ! grep -q '^- ' <<<"$notes"; then
        echo "error: CHANGELOG.md: ## [$1] has no entries" >&2
        exit 1
    fi
    printf '%s\n' "$notes"

# Check CHANGELOG.md's structure and that it has a section for the Cargo.toml version
changelog-check:
    #!/usr/bin/env bash
    set -euo pipefail
    version=$({{ just }} _cargo-version)
    toc=$({{ just }} _changelog)
    errors=() names=()
    declare -A dates=() links=()
    while IFS=$'\t' read -r kind name date; do
        case $kind in
            heading)
                if [[ -v dates[$name] ]]; then
                    errors+=("duplicate heading ## [$name]")
                fi
                if [[ $name == Unreleased && -n $date ]]; then
                    errors+=("## [Unreleased] takes no date")
                elif [[ $name != Unreleased && -z $date ]]; then
                    errors+=("## [$name] has no date")
                fi
                names+=("$name")
                dates[$name]=$date
                ;;
            link)
                if [[ -v links[$name] ]]; then
                    errors+=("duplicate link definition [$name]:")
                fi
                links[$name]=1
                ;;
        esac
    done <<<"$toc"
    if [[ ${names[0]:-} != Unreleased ]]; then
        errors+=("## [Unreleased] must be the first heading")
    fi
    if [[ ! -v dates[$version] ]]; then
        errors+=("no ## [$version] section for the Cargo.toml version")
    elif ! {{ just }} changelog-notes "$version" >/dev/null 2>&1; then
        errors+=("## [$version] has no entries")
    fi
    for name in "${names[@]}"; do
        if [[ ! -v links[$name] ]]; then
            errors+=("## [$name] has no [$name]: link definition")
        fi
    done
    if (( ${#errors[@]} )); then
        printf 'error: CHANGELOG.md: %s\n' "${errors[@]}" >&2
        exit 1
    fi
    echo "CHANGELOG.md OK (current version $version)"

# Roll Unreleased into a new version, bump Cargo.toml/Cargo.lock and commit (never pushes)
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    fail() { echo "error: $*" >&2; exit 1; }
    repo=https://github.com/nickolaj-jepsen/niri-dynamic-workspaces
    semver='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'

    current=$({{ just }} _cargo-version)
    [[ $current =~ $semver ]] || fail "Cargo.toml version $current isn't X.Y.Z"
    major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}
    case $1 in
        patch) new=$major.$minor.$((patch + 1)) ;;
        minor) new=$major.$((minor + 1)).0 ;;
        major) new=$((major + 1)).0.0 ;;
        *) new=$1 ;;
    esac

    [[ $new =~ $semver ]] || fail "$new isn't X.Y.Z, patch, minor or major"
    if [[ $new == "$current" ]] || ! printf '%s\n' "$current" "$new" | sort -C -V; then
        fail "$new isn't greater than the current version $current"
    fi
    if git rev-parse -q --verify "refs/tags/v$new" >/dev/null; then
        fail "tag v$new already exists"
    fi
    if [[ -n $(git status --porcelain --untracked-files=no -- ':(exclude)CHANGELOG.md') ]]; then
        fail "tracked files other than CHANGELOG.md have changes; commit or stash them first"
    fi
    command -v cargo >/dev/null || fail "cargo not found; run through nix develop"
    # Without a cached registry index the real update fails after the edits.
    cargo update --workspace --offline --dry-run --quiet || fail "cargo can't update Cargo.lock offline; run cargo fetch first"
    {{ just }} changelog-check >/dev/null
    if ! {{ just }} changelog-notes Unreleased >/dev/null 2>&1; then
        fail "nothing to release: ## [Unreleased] has no entries"
    fi
    # grep without -q reads all input, so pipefail sees no SIGPIPE.
    if {{ just }} _changelog | cut -f2 | grep -xF -- "$new" >/dev/null; then
        fail "CHANGELOG.md already has a [$new] heading or link definition"
    fi

    awk -v new="$new" -v prev="$current" -v date="$(date +%F)" -v repo="$repo" '
        $0 == "## [Unreleased]" { print; print ""; print "## [" new "] - " date; next }
        /^\[Unreleased\]: / {
            print "[Unreleased]: " repo "/compare/v" new "...HEAD"
            print "[" new "]: " repo "/compare/v" prev "...v" new
            next
        }
        { print }
    ' CHANGELOG.md >CHANGELOG.md.new
    mv CHANGELOG.md.new CHANGELOG.md

    # The first match is the [package] version, as in _cargo-version.
    awk -v new="$new" '!done && /^version = "/ { $0 = "version = \"" new "\""; done = 1 } { print }' \
        Cargo.toml >Cargo.toml.new
    mv Cargo.toml.new Cargo.toml
    cargo update --workspace --offline --quiet
    if [[ $(git diff --numstat -- Cargo.lock) != $'1\t1\tCargo.lock' ]]; then
        fail "cargo changed more of Cargo.lock than its own version; see git diff"
    fi

    {{ just }} changelog-check
    # A CHANGELOG.md that was never committed must be added before a pathspec commit.
    git add -- Cargo.toml Cargo.lock CHANGELOG.md
    git commit -m "chore: bump version to $new" -- Cargo.toml Cargo.lock CHANGELOG.md
    echo "Pushing to main releases v$new once CI passes."

# The only CHANGELOG.md parser: prints a section's trimmed body, or with no name lists headings and link definitions
_changelog name="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f CHANGELOG.md ]]; then
        echo "error: CHANGELOG.md is missing" >&2
        exit 1
    fi
    heading='^## \[([^]]+)\]( - ([0-9]{4}-[0-9]{2}-[0-9]{2}))?$'
    link='^\[([^]]+)\]: '
    section="" found=false body=()
    while IFS= read -r line || [[ -n $line ]]; do
        if [[ $line == '## '* ]]; then
            if [[ ! $line =~ $heading ]]; then
                echo "error: CHANGELOG.md: malformed heading: $line" >&2
                exit 1
            fi
            section=${BASH_REMATCH[1]}
            if [[ -z $1 ]]; then
                printf 'heading\t%s\t%s\n' "$section" "${BASH_REMATCH[3]}"
            elif [[ $section == "$1" ]]; then
                found=true
            fi
        elif [[ $line =~ $link ]]; then
            # Link definitions end the last section.
            section=""
            if [[ -z $1 ]]; then
                printf 'link\t%s\n' "${BASH_REMATCH[1]}"
            fi
        elif [[ -n $1 && $section == "$1" ]]; then
            body+=("$line")
        fi
    done <CHANGELOG.md
    [[ -n $1 ]] || exit 0

    if ! $found; then
        echo "error: CHANGELOG.md: no ## [$1] section" >&2
        exit 1
    fi
    start=0 end=${#body[@]}
    while (( start < end )) && [[ -z ${body[start]//[[:space:]]/} ]]; do start=$((start + 1)); done
    while (( end > start )) && [[ -z ${body[end - 1]//[[:space:]]/} ]]; do end=$((end - 1)); done
    printf '%s\n' "${body[@]:start:end-start}"

# The [package] version: the first top-level version line, read as release.yml does
_cargo-version:
    #!/usr/bin/env bash
    set -euo pipefail
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
    if [[ -z $version ]]; then
        echo "error: Cargo.toml has no version line" >&2
        exit 1
    fi
    echo "$version"
