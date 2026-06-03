#!/usr/bin/env bash
set -euo pipefail

WORKSPACE="${1:-/tmp/mini-codex-react-test}"

case "$WORKSPACE" in
  /tmp/mini-codex-*)
    ;;
  *)
    echo "Refusing to remove non-test workspace: $WORKSPACE" >&2
    echo "Use a path like /tmp/mini-codex-react-test." >&2
    exit 1
    ;;
esac

rm -rf "$WORKSPACE"
mkdir -p "$WORKSPACE"

echo "Workspace reset: $WORKSPACE"
echo
echo "Suggested prompt:"
echo "Crie um projeto React chamado meu-react-app com Vite. Escreva os arquivos necessarios, mas nao instale dependencias e nao rode build."
echo

cargo run -- --workspace "$WORKSPACE"
