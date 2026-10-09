#!/usr/bin/env bash
# Thin wrapper: the GPU lane lives in the shared ~/.claude/bin/with_gpu.sh (user, Oct 9 2026: "always use GPU when
# you can", "GPU is favored over CPU"). Game/engine runs, Blender renders, emulators and AI tools go here;
# tools/with_cpu.sh also routes such jobs here automatically.
#   tools/with_gpu.sh [--exclusive] <session-name> <command...>
exec bash "$HOME/.claude/bin/with_gpu.sh" "$@"
