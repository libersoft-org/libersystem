#!/bin/sh

find . -type f -executable \
	-not -path "*/.build/*" \
	-not -path "*/build/*" \
	-not -path "*/target/*" \
	-not -path "*/.git/*" \
	-not -name "*.sh" \
	-exec echo "chmod -x {}" \;
