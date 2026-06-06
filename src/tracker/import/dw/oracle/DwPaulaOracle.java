//Faithful Amiga Paula register-trace oracle for David Whittaker .dw replayers.
//
//Emulates the .dw replayer on the loaded Raw-Binary 68000 image:
//  1. runs init_main once (song setup + sample_loader),
//  2. runs play_tick N times (one per 50 Hz interrupt = one "tick"),
//capturing EVERY write to the Amiga custom-chip audio registers via a
//MemoryAccessFilter, and reconstructing the real per-channel Paula state
//(DMA gate, period, volume) at the end of each tick.
//
//Output: a CSV with one row per tick:
//  tick,dma,c0g,c0per,c0vol,c0s,c1g,c1per,c1vol,c1s,c2g..,c3g..,f0
//where cNg is channel N's DMA gate (1=audible), cNper the AUDxPER word,
//cNvol the AUDxVOL byte clamped to 0..64, cNs the per-tick "struck" flag
//(1 = this tick raised the channel's DMA bit 0->1 = sample reload /
//re-attack; distinguishes a re-trigger from a held/tied note, which the
//gate cannot), and f0 the replayer's "playing" flag (DAT_000005f0;
//0 => song hit an explicit Stop command).
//
//This is the Tier-2 fidelity oracle from PLAN.md: the player's per-tick
//observer trace is diffed frame-to-frame against this CSV to build
//BASELINE.md.
//
//=== Per-cluster entry points ===
//The default init/play addresses below are for the C1 cluster reference
//binary `xenon2 (title).dw` (init_main @0x66, play_tick @0x1b6). Other
//player clusters relocate these; pass them as script args:
//    DwPaulaOracle <initAddr> <playAddr> [ticks] [outCsv] [a3base] [f0addr]
//e.g.  DwPaulaOracle 0x66 0x1b6 1500 /tmp/xen_oracle.csv
//Addresses accept 0x.. hex (Long.decode).
//
//[a3base] — some clusters' per-frame routine is a body that runs with a
//register A3 = module base set by its (host) caller (e.g. bubble bobble's
//`do_track_cmd` uses A3 = -0x442 = 0xfffffbbe so that A3+offset reaches the
//low-address work tables and the command jump table `jmp (0,A3,A2)`). Pass
//that base and the oracle sets A3 before init and before every play call.
//Pass "-" (or omit) to leave A3 untouched (the xenon2 routine sets its own
//base internally). Decoded as a 32-bit two's-complement long.
//[f0addr] — address of the replayer's "playing"/active flag byte (default
//0x5f0, xenon2's DAT_000005f0). 0 in that byte marks an explicit Stop;
//for players whose flag is an always-0xff "active" byte this simply never
//trips, which is correct for forever-looping songs.
//
//=== Requirements ===
//* The .dw must be loaded as Raw Binary, language 68000:BE:32, base 0.
//* GhidraMCP: needs GHIDRA_MCP_ALLOW_SCRIPTS=1 in the Ghidra process env,
//  and ~/ghidra_scripts must be a real (enabled) source bundle — if it
//  was auto-created after Ghidra started you get a GhidraPlaceholderBundle
//  cast error; restart Ghidra (dir now exists) or enable it in Bundle
//  Manager.
//
//@category DW
//@keybinding
//@menupath
//@toolbar

import ghidra.app.script.GhidraScript;
import ghidra.app.emulator.EmulatorHelper;
import ghidra.app.emulator.MemoryAccessFilter;
import ghidra.program.model.address.AddressSpace;
import java.io.FileWriter;

public class DwPaulaOracle extends GhidraScript {

    // Scratch stack top and a sentinel return address used as the
    // "function returned" breakpoint. Both live in addressable but
    // otherwise-unused RAM, well clear of the module image.
    private static final long STACK = 0x300000L;
    private static final long RET   = 0x00f00000L;

    @Override
    public void run() throws Exception {
        String[] a = getScriptArgs();
        long initAddr = a.length > 0 ? Long.decode(a[0]) : 0x66L;
        long playAddr = a.length > 1 ? Long.decode(a[1]) : 0x1b6L;
        int  N        = a.length > 2 ? Integer.parseInt(a[2]) : 1500;
        String out    = a.length > 3 ? a[3] : "/tmp/dw_oracle.csv";
        // A3 base: "-" / absent => leave untouched. Decode as 32-bit.
        Long a3 = (a.length > 4 && !a[4].equals("-")) ? (Long.decode(a[4]) & 0xffffffffL) : null;
        // f0 flag: "-" / absent default => disabled (no read, no stop).
        // Stop-detection is only meaningful for xenon2-style players whose
        // playing flag (DAT_000005f0) goes 0 on an explicit Stop; for other
        // clusters that byte holds unrelated data and would falsely
        // truncate the trace. Pass an explicit address to re-enable it.
        long f0Addr = (a.length > 5 && !a[5].equals("-")) ? Long.decode(a[5]) : -1L;
        // maxStepsPerCall > 0 => bounded single-step mode (cannot hang;
        // use for probing unvalidated entry points). 0 => fast run() path.
        long maxSteps = a.length > 6 ? Long.decode(a[6]) : 0L;
        // d0: song index passed to the host-style init entry (the
        // replayer does `ext.w D0 ; mulu #10,D0` to index the sub-song
        // row table — see bubble bobble dw_init @0x88). "-"/absent =>
        // leave D0 untouched (= the emulator's default 0 = sub-song 0).
        // Set ONLY before init (not before play, which clobbers D0).
        Long d0 = (a.length > 7 && !a[7].equals("-")) ? (Long.decode(a[7]) & 0xffffffffL) : null;

        AddressSpace sp = currentProgram.getAddressFactory().getDefaultAddressSpace();
        EmulatorHelper emu = new EmulatorHelper(currentProgram);

        // Zero the custom-chip register window so reads never fault and
        // the initial DMA/period/volume state is well-defined.
        emu.writeMemory(sp.getAddress(0xdff000L), new byte[0x200]);
        emu.setBreakpoint(sp.getAddress(RET));

        // Continuous Paula state, mutated by the write filter in
        // emulation order (so multiple intra-tick DMACON writes compose
        // correctly — a single post-tick register read would lose them).
        final int[]  dma = new int[]{0};
        final int[]  per = new int[4];
        final int[]  vol = new int[4];
        final long[] lc  = new long[4];
        final int[]  len = new int[4];
        // Per-channel "struck this tick": set when a DMACON write raises a
        // channel's DMA bit 0->1. On Paula that edge reloads AUDxLC/AUDxLEN,
        // i.e. restarts the sample from its head = an audible re-attack. A
        // genuine note sets the bit after the replayer cleared it the frame
        // before (0->1 = strike); a TIE (cmd 0x83 on the suppression
        // families) sets an already-on bit (1->1, no edge, no strike). This
        // is the signal that disambiguates a held note from a re-trigger —
        // gate/period/volume cannot (a held decaying note and a re-struck
        // one share the same gate=1, continuous period, same volume curve).
        final int[]  struck = new int[4];

        MemoryAccessFilter filt = new MemoryAccessFilter() {
            protected void processRead(AddressSpace s, long off, int size, byte[] v) {}
            protected void processWrite(AddressSpace s, long off, int size, byte[] v) {
                if (off == 0xdff096L && size >= 2) {
                    // DMACON: bit15 set => OR the 1-bits, clear => AND-NOT them.
                    int w = ((v[0] & 0xff) << 8) | (v[1] & 0xff);
                    if ((w & 0x8000) != 0) {
                        // Rising channel bits (0->1) = sample-reload strikes.
                        int rising = (w & 0xf) & ~dma[0];
                        for (int ch = 0; ch < 4; ch++)
                            if ((rising & (1 << ch)) != 0) struck[ch] = 1;
                        dma[0] |= (w & 0x3ff);
                    } else {
                        dma[0] &= ~(w & 0x3ff);
                    }
                } else if (off >= 0xdff0a0L && off < 0xdff0e0L) {
                    int ch  = (int) ((off - 0xdff0a0L) / 0x10L);
                    int reg = (int) ((off - 0xdff0a0L) % 0x10L);
                    if      (reg == 0 && size >= 4) lc[ch]  = ((long)(v[0]&0xff)<<24)|((v[1]&0xff)<<16)|((v[2]&0xff)<<8)|(v[3]&0xff);
                    else if (reg == 4 && size >= 2) len[ch] = ((v[0]&0xff)<<8)|(v[1]&0xff);
                    else if (reg == 6 && size >= 2) per[ch] = ((v[0]&0xff)<<8)|(v[1]&0xff);
                    else if (reg == 8 && size >= 2) vol[ch] = ((v[0]&0xff)<<8)|(v[1]&0xff);
                }
            }
        };
        emu.getEmulator().addMemoryAccessFilter(filt);

        if (d0 != null) emu.writeRegister("D0", d0);  // sub-song select
        callSub(emu, sp, initAddr, a3, maxSteps);   // init_main

        StringBuilder csv = new StringBuilder();
        csv.append("tick,dma,c0g,c0per,c0vol,c0s,c1g,c1per,c1vol,c1s,"
                 + "c2g,c2per,c2vol,c2s,c3g,c3per,c3vol,c3s,f0\n");
        for (int t = 0; t < N; t++) {
            struck[0] = struck[1] = struck[2] = struck[3] = 0; // reset per tick
            callSub(emu, sp, playAddr, a3, maxSteps); // play_tick (fills struck via filter)
            int f0 = (f0Addr >= 0) ? (emu.readMemory(sp.getAddress(f0Addr), 1)[0] & 0xff) : -1;
            csv.append(t).append(',').append(Integer.toHexString(dma[0]));
            for (int ch = 0; ch < 4; ch++) {
                int g  = (dma[0] >> ch) & 1;
                int vc = vol[ch] > 64 ? 64 : vol[ch];
                csv.append(',').append(g).append(',').append(per[ch]).append(',').append(vc)
                   .append(',').append(struck[ch]);
            }
            csv.append(',').append(f0).append('\n');
            // Stop only when an explicit f0 flag is configured and reads 0.
            if (f0Addr >= 0 && f0 == 0) { csv.append("# stop@").append(t).append('\n'); break; }
        }

        try (FileWriter fw = new FileWriter(out)) { fw.write(csv.toString()); }
        println("DwPaulaOracle: wrote " + out + " (" + N + " ticks, init=0x"
                + Long.toHexString(initAddr) + " play=0x" + Long.toHexString(playAddr) + ")");
        emu.dispose();
    }

    // Per-call wall-clock guard: a "new player" tick returns in well
    // under a second, so a call still running after this many ms is a
    // runaway (bad entry point, or a routine that never pops the RET
    // sentinel). The watchdog cancels the monitor so emu.run() returns
    // and we throw instead of freezing Ghidra forever.
    private static final long CALL_TIMEOUT_MS = 10_000L;

    // Call a 68k subroutine: set up a fresh stack with the RET sentinel
    // as the return address, jump to `entry`, run until the RET
    // breakpoint pops (i.e. the routine returned) — or abort if a
    // watchdog trips first.
    private void callSub(EmulatorHelper emu, AddressSpace sp, long entry, Long a3, long maxSteps) throws Exception {
        emu.writeRegister("SP", STACK - 4);
        emu.writeMemory(sp.getAddress(STACK - 4),
                new byte[]{ (byte)(RET>>24), (byte)(RET>>16), (byte)(RET>>8), (byte)RET });
        if (a3 != null) emu.writeRegister("A3", a3);
        emu.writeRegister("PC", entry);

        if (maxSteps > 0) {
            // Bounded single-step: we own the loop, so it physically
            // cannot hang — at worst it throws after maxSteps. Used to
            // probe entry points whose return behaviour is unverified
            // (the p-code run() loop does not reliably poll the monitor,
            // so a bad entry can otherwise freeze Ghidra outright).
            for (long i = 0; i < maxSteps; i++) {
                if (emu.getExecutionAddress().getOffset() == RET) return;
                if (!emu.step(monitor)) {
                    throw new Exception("callSub(stepped): step failed at PC="
                            + emu.getExecutionAddress() + " : " + emu.getLastError());
                }
            }
            throw new Exception("callSub(stepped): 0x" + Long.toHexString(entry)
                    + " exceeded " + maxSteps + " instructions without returning "
                    + "(runaway / bad entry). PC=" + emu.getExecutionAddress());
        }

        final long deadline = System.currentTimeMillis() + CALL_TIMEOUT_MS;
        Thread wd = new Thread(() -> {
            while (System.currentTimeMillis() < deadline) {
                try { Thread.sleep(50); } catch (InterruptedException e) { return; }
            }
            monitor.cancel();
        });
        wd.setDaemon(true);
        wd.start();
        try {
            emu.run(monitor);
        } finally {
            wd.interrupt();
        }
        if (monitor.isCancelled()) {
            monitor.clearCanceled();
            throw new Exception("callSub: runaway emulation from 0x"
                    + Long.toHexString(entry) + " (no RET within " + CALL_TIMEOUT_MS
                    + "ms) — likely a bad entry address. Stopped at PC="
                    + emu.getExecutionAddress());
        }
        if (emu.getExecutionAddress().getOffset() != RET) {
            throw new Exception("callSub: 0x" + Long.toHexString(entry)
                    + " stopped at PC=" + emu.getExecutionAddress()
                    + " (expected RET sentinel 0x" + Long.toHexString(RET) + ")");
        }
    }
}
