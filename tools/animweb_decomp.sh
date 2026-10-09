#!/bin/sh
# animweb_decomp.sh <name> <va>... : read-only headless decompile into gamedata/re/aw/<name>.c (anim web RE notes helper)
# AW_GHIDRA_DIR overrides the project folder (default gamedata/ghidra; use a private copy when another session holds the lock).
name=$1; shift
proj=${AW_GHIDRA_DIR:-gamedata\ghidra}
printf "%s\n" "$@" > gamedata/re/aw/$name.txt
cmd //c "C:/Tools\ghidra_12.1.4_PUBLIC\support\analyzeHeadless.bat $proj DOOM2016 -process DOOMx64.exe -noanalysis -readOnly -scriptPath tools\ghidra_scripts -postScript DecompileList.java gamedata/re/aw/$name.txt gamedata/re/aw/$name.c" > gamedata/re/aw/$name.log 2>&1
grep -h "decompiled\|SCRIPT ERROR\|LockException" gamedata/re/aw/$name.log | head -3
