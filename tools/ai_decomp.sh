#!/bin/sh
# ai_decomp.sh <name> <va>... : read-only headless decompile into gamedata/re/ai/<name>.c (demon AI RE helper).
# Uses the private project copy gamedata/ghidra_ai (AI_GHIDRA_DIR overrides) so it never waits on other sessions' locks.
name=$1; shift
proj=${AI_GHIDRA_DIR:-gamedata/ghidra_ai}
out=gamedata/re/ai
mkdir -p $out
printf "%s\n" "$@" > $out/$name.txt
cmd //c "C:/Tools\ghidra_12.1.4_PUBLIC\support\analyzeHeadless.bat $proj DOOM2016 -process DOOMx64.exe -noanalysis -readOnly -scriptPath tools\ghidra_scripts -postScript DecompileList.java $out/$name.txt $out/$name.c" > $out/$name.log 2>&1
grep -h "decompiled\|SCRIPT ERROR\|LockException" $out/$name.log | head -3
