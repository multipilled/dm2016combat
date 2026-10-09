#!/bin/sh
# map_decomp.sh <name> <va>... : read-only headless decompile into gamedata/re/map/<name>.c (map-loading RE notes helper)
# Uses a private copy of the Ghidra project (gamedata/ghidra_map) so other sessions' locks don't collide.
name=$1; shift
proj=${MAP_GHIDRA_DIR:-gamedata/ghidra_map}
printf "%s\n" "$@" > gamedata/re/map/$name.txt
cmd //c "C:/Tools\ghidra_12.1.4_PUBLIC\support\analyzeHeadless.bat $proj DOOM2016 -process DOOMx64.exe -noanalysis -readOnly -scriptPath tools\ghidra_scripts -postScript DecompileList.java gamedata/re/map/$name.txt gamedata/re/map/$name.c" > gamedata/re/map/$name.log 2>&1 </dev/null
grep -h "decompiled\|SCRIPT ERROR\|LockException" gamedata/re/map/$name.log | head -3
