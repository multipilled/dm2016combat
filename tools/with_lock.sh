#!/usr/bin/env bash
# Thin wrapper: the lock logic lives in the shared ~/.claude/bin/with_lock.sh
# (PID-based stale check: a holder waiting for a CPU slot keeps its lock). The lock root is this repo's main
# checkout, next to the common .git, so every worktree shares one lock.
#   tools/with_lock.sh <session-name> <command...>      LOCK_NAME=<name> picks a different lock (default game)
# Replace this file only by writing a new copy and mv-ing it over: bash reads running scripts lazily.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
common=$(git -C "$here" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || echo "$here/../.git")
WITH_LOCK_ROOT="$(dirname "$common")" exec bash "$HOME/.claude/bin/with_lock.sh" "$@"
