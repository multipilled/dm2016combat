#!/bin/sh
# decomp.sh <name> <va> [<va>...] : headless-decompile functions of the user's DOOM exe into gamedata/re/<name>.ann.c
name=$1; shift
printf "%s\n" "$@" > gamedata/re/$name.txt
cmd //c "C:/Tools\ghidra_12.1.4_PUBLIC\support\analyzeHeadless.bat gamedata\ghidra DOOM2016 -process DOOMx64.exe -noanalysis -scriptPath tools\ghidra_scripts -postScript DecompileList.java gamedata/re/$name.txt gamedata/re/$name.c" > gamedata/re/$name.log 2>&1
grep -h "decompiled\|SCRIPT ERROR" gamedata/re/$name.log
~/AppData/Local/Programs/Python/Python312/python.exe tools/annotate.py gamedata/re/$name.c gamedata/re/$name.ann.c gamedata/re/fields_physics_player.tsv > /dev/null
