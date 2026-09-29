#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 /path/to/tree-sitter-kusto" >&2
    exit 2
fi

extension_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
grammar_dir=$1
fixture="$extension_dir/examples/highlighting.kql"
query="$extension_dir/languages/kusto/highlights.scm"

parse_output=$(cd "$grammar_dir" && tree-sitter parse "$fixture")
if printf '%s\n' "$parse_output" | grep -Eq '\(ERROR|\(MISSING'; then
    printf '%s\n' "$parse_output" >&2
    exit 1
fi

query_output=$(cd "$grammar_dir" && tree-sitter query "$query" "$fixture")
for expected in \
    'comment, start: (0, 0)' \
    'keyword, start: (1, 0)' \
    'type, start: (2, 0)' \
    'string, start: (3, 17)' \
    'function, start: (4, 29)' \
    'keyword, start: (6, 17)'; do
    if ! printf '%s\n' "$query_output" | grep -Fq "$expected"; then
        printf 'missing capture: %s\n' "$expected" >&2
        exit 1
    fi
done

echo "Kusto fixture parses and expected captures are present."
