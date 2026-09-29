#!/bin/sh
set -eu

extension_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
work_dir="${KUSTO_EXTENSION_WORK_DIR:-$HOME/Library/Application Support/Zed/extensions/work/kusto}"
publish_dir="$work_dir/server"

dotnet publish "$extension_dir/server/KustoLanguageServer.csproj" \
    --configuration Release --output "$publish_dir" --nologo

if [ ! -x "$publish_dir/kusto-lsp" ]; then
    echo "dotnet publish did not produce an executable language server" >&2
    exit 1
fi

echo "Installed Kusto language server at $publish_dir/kusto-lsp"
