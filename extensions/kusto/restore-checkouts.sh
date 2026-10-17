#!/bin/sh
# Restores the NuGet packages of C# checkouts whose feeds are on Azure DevOps, signing in to the
# feeds with a token from the Azure CLI. Each argument is a checkout or a folder of checkouts, as in
# `kusto_results.code.roots`. A C# language server loads no project of a checkout that has not been
# restored, so going to definitions and finding references stays inside the open file.
set -u

if [ $# -eq 0 ]; then
    echo "usage: $0 <checkout or folder of checkouts>..." >&2
    exit 2
fi

# `dotnet` installed in the home folder runs only when it knows where it is.
if [ -z "${DOTNET_ROOT:-}" ] && [ -x "$HOME/.dotnet/dotnet" ]; then
    export DOTNET_ROOT="$HOME/.dotnet"
fi
if [ -n "${DOTNET_ROOT:-}" ] && [ -x "$DOTNET_ROOT/dotnet" ]; then
    dotnet="$DOTNET_ROOT/dotnet"
else
    dotnet=dotnet
fi

checkouts=""
for argument in "$@"; do
    root=$(CDPATH= cd -- "$argument" 2>/dev/null && pwd) || {
        echo "skipping $argument: not a folder" >&2
        continue
    }
    if [ -e "$root/.git" ]; then
        checkouts="$checkouts$root
"
    else
        for child in "$root"/*/; do
            if [ -e "$child.git" ]; then
                checkouts="$checkouts${child%/}
"
            fi
        done
    fi
done
if [ -z "$checkouts" ]; then
    echo "no git checkouts found in $*" >&2
    exit 1
fi

# Every Azure DevOps feed named by a NuGet.config of the checkouts gets the same token.
feeds=$(printf '%s' "$checkouts" | while IFS= read -r checkout; do
    git -C "$checkout" ls-files | grep -i '\(^\|/\)nuget\.config$' | while IFS= read -r config; do
        grep -oE 'https://[^"]*(pkgs\.dev\.azure\.com|\.pkgs\.visualstudio\.com)/[^"]*' "$checkout/$config"
    done
done | sort -u)

if [ -n "$feeds" ]; then
    # 499b84ac-... is the resource ID of Azure DevOps.
    token=$(az account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798 \
        --query accessToken --output tsv) || {
        echo "could not get an Azure DevOps token from the Azure CLI; run \`az login\` first" >&2
        exit 1
    }
    endpoints=$(printf '%s\n' "$feeds" | while IFS= read -r feed; do
        printf '{"endpoint":"%s","password":"%s"},' "$feed" "$token"
    done)
    # The Azure Artifacts credential provider reads the feeds' credentials from here.
    export VSS_NUGET_EXTERNAL_FEED_ENDPOINTS="{\"endpointCredentials\":[${endpoints%,}]}"
fi

failed=""
while IFS= read -r checkout; do
    [ -n "$checkout" ] || continue
    solutions=$(git -C "$checkout" ls-files -- '*.sln' '*.slnx' | grep -v /)
    if [ -z "$solutions" ]; then
        echo "== $checkout: no solution at the top, skipped"
        continue
    fi
    while IFS= read -r solution; do
        echo "== $checkout/$solution"
        if ! "$dotnet" restore "$checkout/$solution" --nologo </dev/null; then
            failed="$failed  $checkout/$solution
"
        fi
    done <<SOLUTIONS
$solutions
SOLUTIONS
done <<EOF
$checkouts
EOF

if [ -n "$failed" ]; then
    printf 'restore failed for:\n%s' "$failed" >&2
    exit 1
fi
echo "restored every solution"
