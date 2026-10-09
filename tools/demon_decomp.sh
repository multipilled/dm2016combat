#!/bin/sh
# demon_decomp.sh <name> <va>... : read-only headless decompile into gamedata/re/demon/<name>.c (demon RE notes helper).
# Uses the private project copy gamedata/ghidra_demon (DEMON_GHIDRA_DIR overrides) so it never waits on other sessions' locks.
name=$1; shift
proj=${DEMON_GHIDRA_DIR:-gamedata/ghidra_demon}
mkdir -p gamedata/re/demon
printf "%s\n" "$@" > gamedata/re/demon/$name.txt
cmd //c "C:/Tools\ghidra_12.1.4_PUBLIC\support\analyzeHeadless.bat $proj DOOM2016 -process DOOMx64.exe -noanalysis -readOnly -scriptPath tools\ghidra_scripts -postScript DecompileList.java gamedata/re/demon/$name.txt gamedata/re/demon/$name.c" > gamedata/re/demon/$name.log 2>&1
grep -h "decompiled\|SCRIPT ERROR\|LockException" gamedata/re/demon/$name.log | head -3
