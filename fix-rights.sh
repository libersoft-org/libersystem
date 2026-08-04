#!/bin/sh

find . -type f -executable \
	-not -path "*/.build/*" \
	-not -path "*/build/*" \
	-not -path "*/target/*" \
	-not -path "*/.git/*" \
	-not -path "*/.vscode/*" \
	-not -path "*/.venv/*" \
	-not -name "*.sh" \
	-exec echo "chmod -x {}" \;
