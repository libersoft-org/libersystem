#!/bin/sh

REPO="libersystem.git"
NAME="LiberSoft"
BRANCH="main"
EMAIL="info@libersoft.org"
USER="libersoft-org"
PASS=$(cat ./.secret_git)

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

if command -v just >/dev/null 2>&1; then
	(cd src && just fmt) || echo "commit.sh: 'just fmt' failed - committing without a fresh format pass"
fi

SOURCE_ARTIFACTS=$(find src -path 'src/boot/.build' -prune -o \( -type d \( -name target -o -name shared \) -o -type f \( -name '*.lslib' -o -name '*.lsexe' \) \) -print)
if [ -n "$SOURCE_ARTIFACTS" ]; then
	echo "ERROR: compiled artifacts are forbidden in the source tree:" >&2
	echo "$SOURCE_ARTIFACTS" >&2
	exit 1
fi

git status
git add .

TRACKED_ARTIFACTS=$(git ls-files | grep -E '\.(lslib|lsexe)$|(^|/)(target|shared)/' || true)
if [ -n "$TRACKED_ARTIFACTS" ]; then
	echo "ERROR: compiled artifacts are tracked by Git:" >&2
	echo "$TRACKED_ARTIFACTS" >&2
	exit 1
fi

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
