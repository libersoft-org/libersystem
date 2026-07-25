#!/usr/bin/env bash

REPO="libersystem.git"
NAME="LiberSoft"
BRANCH="main"
EMAIL="info@libersoft.org"
USER="libersoft-org"
ROOT="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

cd "$ROOT"

PASS=$(<"$ROOT/.secret_git")

if [ ! -d "./.git/" ]; then
	git init
	git config --global --add safe.directory '*'
	git remote add origin https://$USER:$PASS@github.com/$USER/$REPO
else
	git remote set-url origin https://$USER:$PASS@github.com/$USER/$REPO
fi

bun i -g prettier

git config user.name "$NAME"
git config user.email "$EMAIL"

format_changed_files() {
	local -a changed_files=()
	local -a rust_files=()
	local -a shell_files=()
	local -a toml_files=()
	local file
	local format_justfile=0

	mapfile -d '' -t changed_files < <(
		{
			git diff --name-only -z --diff-filter=ACMR
			git diff --cached --name-only -z --diff-filter=ACMR
			git ls-files --others --exclude-standard -z
		} | LC_ALL=C sort -zu
	)

	for file in "${changed_files[@]}"; do
		[[ -f "$file" ]] || continue
		case "$file" in
		*.rs) rust_files+=("$file") ;;
		*.sh) shell_files+=("$file") ;;
		*.toml) toml_files+=("$file") ;;
		src/Justfile) format_justfile=1 ;;
		esac
	done

	if ((${#rust_files[@]} > 0)); then
		rustfmt +nightly --edition 2024 --config-path "$ROOT/rustfmt.toml" "${rust_files[@]}" || return 1
	fi
	if ((${#shell_files[@]} > 0)); then
		shfmt -w "${shell_files[@]}" || return 1
	fi
	if ((${#toml_files[@]} > 0)); then
		taplo fmt "${toml_files[@]}" || return 1
	fi
	if ((format_justfile)); then
		(cd "$ROOT/src" && just --fmt) || return 1
	fi
}

if ! format_changed_files; then
	echo "commit.sh: formatting changed files failed - committing without a fresh format pass"
fi

src/tools/check-source-hygiene.sh --current

git status
git add .

src/tools/check-source-hygiene.sh --current

git status

if [ "$#" -eq 0 ]; then
	echo "Generating commit message using GitHub Copilot..."
	COMMIT_MSG=$({
		echo "Write exactly one Git commit subject."
		echo "Max 250 characters."
		echo "One line only."
		echo "No prefix."
		echo "No markdown."
		echo "No bullets."
		echo "No explanation."
		echo "No status narration."
		echo "If there are no changes, write exactly: No changes"
		echo
		echo "GIT STATUS:"
		git status --short
		echo
		echo "STAGED DIFF STAT:"
		git diff --cached --stat
		echo
		echo "STAGED DIFF:"
		git diff --cached --unified=0
		echo
		echo "UNSTAGED DIFF STAT:"
		git diff --stat
		echo
		echo "UNSTAGED DIFF:"
		git diff --unified=0
	} | copilot -s --no-ask-user 2>/dev/null)
	if [ -z "$COMMIT_MSG" ] || [ "$COMMIT_MSG" = "No changes" ]; then
		echo "\033[31mERROR:\033[0m Failed to generate commit message. Please provide one manually:"
		echo "Usage: $0 \"[COMMIT MESSAGE]\""
		exit 1
	fi
	COMMIT_MSG=$(echo "$COMMIT_MSG" | sed 's/"//g' | sed "s/'//g")
	echo "\033[33mGENERATED COMMIT MESSAGE:\033[0m $COMMIT_MSG"
	COMMIT_MESSAGE="$COMMIT_MSG"
else
	COMMIT_MESSAGE=$(echo "$1" | sed 's/"//g' | sed "s/'//g")
fi

git commit -m "$COMMIT_MESSAGE"
git push
git status
