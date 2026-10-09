// Headless: decompile the given entry points of the user's own DOOM exe to one file.
// Before decompiling a function, its direct callees (two levels) are created if missing, decompiled,
// and their inferred parameters/returns committed, so float arguments passed in xmm registers survive.
// args: <addr list file (one hex VA per line)> <output .c file>
import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.pcode.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.nio.file.*;
import java.util.*;

public class DecompileList extends GhidraScript {
    DecompInterface d;
    Set<Address> committed = new HashSet<>();

    Function ensure(Address at) throws Exception {
        FunctionManager fm = currentProgram.getFunctionManager();
        Function f = fm.getFunctionAt(at);
        if (f == null) {
            new DisassembleCommand(at, null, true).applyTo(currentProgram, monitor);
            f = createFunction(at, null);
        }
        return f;
    }

    List<Function> callees(Function f) throws Exception {
        List<Function> out = new ArrayList<>();
        InstructionIterator it = currentProgram.getListing().getInstructions(f.getBody(), true);
        while (it.hasNext()) {
            Instruction ins = it.next();
            if (!ins.getFlowType().isCall()) continue;
            for (Address t : ins.getFlows()) {
                Function c = ensure(t);
                if (c != null && !out.contains(c)) out.add(c);
            }
        }
        return out;
    }

    void commit(Function f, int depth) throws Exception {
        if (f == null || committed.contains(f.getEntryPoint()) || f.isThunk()) return;
        committed.add(f.getEntryPoint());
        if (depth > 0) for (Function c : callees(f)) commit(c, depth - 1);
        DecompileResults r = d.decompileFunction(f, 60, monitor);
        if (r == null || !r.decompileCompleted()) return;
        HighFunction hf = r.getHighFunction();
        try {
            HighFunctionDBUtil.commitParamsToDatabase(hf, true, HighFunctionDBUtil.ReturnCommitOption.COMMIT, SourceType.ANALYSIS);
        } catch (Exception e) {
            println("commit failed for " + f.getName() + ": " + e.getMessage());
        }
    }

    @Override
    public void run() throws Exception {
        String[] a = getScriptArgs();
        List<String> lines = Files.readAllLines(Paths.get(a[0]));
        d = new DecompInterface();
        d.setOptions(new DecompileOptions());
        d.openProgram(currentProgram);
        List<Function> todo = new ArrayList<>();
        for (String l : lines) {
            l = l.trim();
            if (l.isEmpty()) continue;
            Function f = ensure(toAddr(Long.decode(l)));
            if (f != null) todo.add(f);
            else println("could not create function at " + l);
        }
        for (Function f : todo) for (Function c : callees(f)) commit(c, 1);
        try (PrintWriter out = new PrintWriter(new FileWriter(a[1]))) {
            for (Function f : todo) {
                DecompileResults r = d.decompileFunction(f, 120, monitor);
                out.println("// ==== " + f.getName() + " @ " + f.getEntryPoint());
                if (r != null && r.decompileCompleted()) out.println(r.getDecompiledFunction().getC());
                else out.println("// decompile failed: " + (r == null ? "null" : r.getErrorMessage()));
            }
        }
        println("decompiled " + todo.size() + " functions -> " + a[1]);
    }
}
