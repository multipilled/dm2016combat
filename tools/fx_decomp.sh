#!/bin/sh
# fx_decomp.sh <name> <va>... : read-only headless decompile into gamedata/re/fx/<name>.c (FX/particle RE notes helper)
# FX_GHIDRA_DIR overrides the project folder (default gamedata/ghidra_fx, a private copy of gamedata/ghidra so other sessions keep their lock).
name=$1; shift
proj=${FX_GHIDRA_DIR:-gamedata/ghidra_fx}
printf "%s\n" "$@" > gamedata/re/fx/$name.txt
cmd //c "C:/Tools\ghidra_12.1.4_PUBLIC\support\analyzeHeadless.bat $proj DOOM2016 -process DOOMx64.exe -noanalysis -readOnly -scriptPath tools\ghidra_scripts -postScript DecompileList.java gamedata/re/fx/$name.txt gamedata/re/fx/$name.c" > gamedata/re/fx/$name.log 2>&1
grep -h "decompiled\|SCRIPT ERROR\|LockException\|ERROR" gamedata/re/fx/$name.log | head -3
