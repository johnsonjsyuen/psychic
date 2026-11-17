#!/usr/bin/env python3
"""Performance benchmarks of shell history search (Ctrl-R): B1-B4, B6-B8.

docs/ctrl-r/04-bench.md section 6 is the specification; the targets are
defined in docs/ctrl-r/00-blueprint.md section 2 and copied into TARGETS below.

    HOME=<scratch home> ./bench/history.py --work <scratch dir> setup
    HOME=<scratch home> ./bench/history.py --work <scratch dir> all

Modes: setup, startup [N], keystroke [N], track [N], train [N], size,
twostage, latency [N], all. Every mode works on <work>/cmd-perf, which
`setup` builds; `all` writes <work>/cmd-results.json.

Safety rules this script enforces, because every psychic subcommand except
`track-command` writes a log into its data directory:

* it refuses to run unless HOME names a scratch directory (not the account's
  home from the password database) and --work is given and lies outside the
  real data directory;
* every psychic process gets --data-dir <work>/cmd-perf and a minimal
  environment (scratch HOME, PATH, TERM, PSYCHIC_NO_RETRAIN=1, so no picker
  starts a training beside a measurement);
* it kills only the processes it started.

The pty plumbing (spawn, read_until_quiet, drain_for) is bench/harness.py's;
that file is imported, never edited (ADR-19). Standard library only.
"""

import argparse
import json
import math
import os
import pwd
import select
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import COLS, REPO, ROWS, Version, drain_for, read_until_quiet, spawn  # noqa: E402

# PSYCHIC_BENCH_BINARY measures another build (a copy of an earlier one, to
# compare two commits interleaved on the same perf DB); it is not checked
# against the sources.
BINARY_OVERRIDE = os.environ.get("PSYCHIC_BENCH_BINARY")
BINARY = Path(BINARY_OVERRIDE) if BINARY_OVERRIDE else REPO / "target/release/psychic"

# 00-blueprint.md section 2 is the definition of every target; these are copies.
TARGETS = {
    "B1_worker_ms_p95": 8.0,
    "B2_redraw_ms_p95": 16.0,
    "B3_first_results_ms_p95": 50.0,
    "B4_track_ms_p95": 10.0,
    "B6_retrain_s": 60.0,
    "B7_db_mb": 300.0,
    "B8_overlap": 0.98,
    "B8_top1": 0.995,
}
# File search's own figures (00 section 2, reference points, not targets).
FILE_REFERENCE = "file search reference: round trip 1.16 ms, keystroke -> redraw 2.45 ms, first full render 11.55 ms"
# A mode whose dropped trials exceed this share prints UNRELIABLE (E-N-08).
MAX_DROPPED = 0.10
# Six words, each typed and deleted; each starts with another letter, so its
# first key is a full scan, as at the start of a real search (the worker would
# answer a repeated first letter from its trail of earlier matches).
KEY_CYCLE = [k for word in (b"git st", b"cargo ", b"docker", b"make t", b"npm in", b"kubect")
             for k in list(word) + [0x7F] * 6]
# A missed redraw counts as this long in B2 (E-N-08: dropped and counted).
MISS_MS = 2000.0


# ---------------------------------------------------------------------------
# Safety and environment
# ---------------------------------------------------------------------------

def fail(message):
    print(message, file=sys.stderr)
    sys.exit(1)


def real_home():
    return Path(pwd.getpwuid(os.getuid()).pw_dir).resolve()


def check_environment(work):
    """Refuse anything that could touch the real data directory."""
    home = os.environ.get("HOME")
    if not home:
        fail("HOME is not set: set it to a scratch directory")
    home = Path(home).resolve()
    if home == real_home():
        fail("HOME is your real home directory: set HOME to a scratch directory "
             "(every psychic subcommand writes a log into its data directory)")
    real_data = real_home() / ".local/share/psychic"
    work = work.resolve()
    if work == real_data or real_data in work.parents:
        fail(f"--work {work} lies inside the real data directory")
    home.mkdir(parents=True, exist_ok=True)
    return home


def child_env(home):
    """The whole environment of every psychic process: nothing inherited
    that could point it at real data (XDG_DATA_HOME, PSYCHIC_HISTORY, ...)."""
    return {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "TERM": "xterm-256color",
        "LANG": os.environ.get("LANG", "C.UTF-8"),
        "PSYCHIC_NO_RETRAIN": "1",
    }


def check_binary():
    """E-N-14: a binary older than the newest Rust source measures old code."""
    if not BINARY.exists():
        fail(f"{BINARY} does not exist: run just build")
    if BINARY_OVERRIDE:
        return
    newest = max(p.stat().st_mtime for p in (REPO / "src").rglob("*.rs"))
    if BINARY.stat().st_mtime < newest:
        fail("binary older than sources: run just build")


def sqlite3_cli(db, sql):
    """E-N-09: the sqlite3 command is needed for setup and size."""
    try:
        out = subprocess.run(["sqlite3", str(db), sql], check=True, capture_output=True, text=True)
    except FileNotFoundError:
        fail("bench/history.py needs the sqlite3 command")
    return out.stdout.strip()


def percentile(values, q):
    """p(q) = sorted(v)[min(len(v) - 1, ceil(q * len(v)) - 1)] (section 6)."""
    if not values:
        return float("nan")
    v = sorted(values)
    return v[min(len(v) - 1, max(0, math.ceil(q * len(v)) - 1))]


def load_average():
    return " ".join(Path("/proc/loadavg").read_text().split()[:3])


class Bench:
    """One perf DB under <work>/cmd-perf and the way to run psychic on it."""

    def __init__(self, work, home):
        self.work = work
        self.home = home
        self.data = work / "cmd-perf"
        self.env = child_env(home)
        self.version = Version("cmd", BINARY, self.data)
        self.log = self.data / "app.log"

    def psychic(self, *args, capture=True):
        cmd = [str(BINARY), "--data-dir", str(self.data), *args]
        return subprocess.run(cmd, env=self.env, check=True, text=True,
                              capture_output=capture, cwd="/tmp")

    def pinned(self, name):
        return self.work / f"pinned-cmd-{name}"

    def pin_model(self):
        """Restore the pinned model before every picker launch (section 6)."""
        for name in ("cmd_model.txt", "cmd_model_stats.json"):
            if not self.pinned(name).exists():
                fail("no pinned model: run setup first")
            shutil.copyfile(self.pinned(name), self.data / name)

    def context(self):
        path = self.data / "bench-context.json"
        if not path.exists():
            fail("run ./bench/history.py setup first")
        return json.loads(path.read_text())

    def log_size(self):
        return self.log.stat().st_size if self.log.exists() else 0

    def timings_since(self, offset):
        """The TIMING lines appended to app.log after `offset`, as dicts.
        psychic rotates app.log to app.log.1 at start-up past 8 MiB: then the
        rest of the old file and all of the new one are read."""
        if not self.log.exists():
            return []
        text = ""
        if self.log.stat().st_size < offset:
            rotated = self.log.with_name("app.log.1")
            if rotated.exists():
                with open(rotated, "rb") as f:
                    f.seek(offset)
                    text = f.read().decode("utf-8", "replace")
            offset = 0
        with open(self.log, "rb") as f:
            f.seek(offset)
            text += f.read().decode("utf-8", "replace")
        out = []
        for line in text.splitlines():
            at = line.find("TIMING ")
            if at >= 0:
                try:
                    out.append(json.loads(line[at + 7:]))
                except json.JSONDecodeError:
                    pass
        return out

    def spawn_picker(self):
        """A picker on a fresh 40x120 pty, started from /tmp with the bench context."""
        self.pin_model()
        ctx = self.context()
        saved = dict(os.environ)
        # harness.spawn exec's with this process's environment.
        os.environ.clear()
        os.environ.update(self.env)
        try:
            return spawn(self.version, ("history", "--cwd", ctx["cwd"], "--session", ctx["session"]),
                         rows=ROWS, cols=COLS, cwd="/tmp")
        finally:
            os.environ.clear()
            os.environ.update(saved)


def stop_picker(pid, fd):
    """Esc, as a user leaves; SIGKILL only if it has not exited in 2 s.
    Not harness.stop: that also pkills by pattern, and a picker started with
    PSYCHIC_NO_RETRAIN has no training child to clean up."""
    try:
        os.write(fd, b"\x1b")
    except OSError:
        pass
    end = time.perf_counter() + 2.0
    while time.perf_counter() < end:
        if select.select([fd], [], [], 0.05)[0]:
            try:
                os.read(fd, 1 << 16)
            except OSError:
                pass
        if os.waitpid(pid, os.WNOHANG)[0]:
            break
    else:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
    try:
        os.close(fd)
    except OSError:
        pass


def row(name, p50, p95, target, verdict, unit="ms"):
    return f"{name:<52}{p50:>8.2f}{unit:<3}{p95:>8.2f}{unit:<3}{target:>9}  {verdict}"


def verdict(value, limit, higher_is_better=False, unreliable=False):
    if unreliable or value != value:  # NaN
        return "UNRELIABLE"
    ok = value >= limit if higher_is_better else value <= limit
    return "PASS" if ok else "FAIL"


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------

def setup(b, args):
    """6.1: the perf DB, its model (pinned) and the benchmark context."""
    if b.data.exists():
        shutil.rmtree(b.data)
    b.data.mkdir(parents=True)
    if args.from_dir:
        src = Path(args.from_dir) / "events.db"
        if not src.exists():
            fail(f"{src} does not exist")
        # .backup through a read-only open: a consistent copy that includes
        # what is still in the source's -wal, and never writes to it.
        sqlite3_cli(f"file:{src}?mode=ro", f".backup '{b.data / 'events.db'}'")
        os.chmod(b.data / "events.db", 0o600)
        print(f"copied {src}")
    else:
        anchor = args.anchor if args.anchor is not None else int(time.time())
        print(b.psychic("internal", "bench-history", "generate", "--commands", str(args.commands),
                        "--runs", str(args.runs), "--seed", str(args.seed), "--anchor", str(anchor),
                        "--profile", args.profile).stdout.strip())
    b.psychic("history", "--enable", "--no-import")
    started = time.perf_counter()
    b.psychic("history", "--retrain")
    print(f"trained in {time.perf_counter() - started:.1f} s")
    for name in ("cmd_model.txt", "cmd_model_stats.json"):
        shutil.copyfile(b.data / name, b.pinned(name))
    db = b.data / "events.db"
    cwd = sqlite3_cli(db, "SELECT cwd FROM command_runs WHERE cwd IS NOT NULL AND cwd != '' "
                          "GROUP BY cwd ORDER BY COUNT(*) DESC, cwd LIMIT 1")
    session = sqlite3_cli(db, "SELECT shell_session FROM command_runs ORDER BY started_at DESC, id DESC LIMIT 1")
    (b.data / "bench-context.json").write_text(json.dumps({"cwd": cwd, "session": session}) + "\n")
    print(b.psychic("internal", "bench-history", "stats").stdout.strip())
    print("context: most frequent cwd and the newest run's session written to bench-context.json")


def startup(b, n):
    """6.2 (B3): start to first results on screen, from the TIMING lines."""
    keys = {"cmd_first_full_render": [], "cmd_first_results": [], "cmd_first_render": [],
            "cmd_index_load": [], "cmd_model_load": [], "first_query": []}
    dropped = 0
    for trial in range(n + 1):
        offset = b.log_size()
        pid, fd = b.spawn_picker()
        try:
            drain_for(fd, 1.5)
        finally:
            stop_picker(pid, fd)
        if trial == 0:
            continue  # warm-up, discarded
        lines = b.timings_since(offset)
        ops = {}
        for line in lines:
            ops.setdefault(line["op"], line)
        if "cmd_first_full_render" not in ops:
            dropped += 1
            continue
        for key in keys:
            if key == "first_query":
                if "cmd_query" in ops:
                    keys[key].append(ops["cmd_query"]["total_ms"])
            elif key in ops:
                keys[key].append(ops[key]["ms"])
    unreliable = dropped > MAX_DROPPED * n
    b3 = keys["cmd_first_full_render"]
    print(f"startup: {n} trials (+1 warm-up), {dropped} dropped, load {load_average()}")
    print(f"{'':<34}{'p50':>9}{'p95':>9}{'max':>9}")
    labels = {"cmd_first_full_render": "first results on screen (B3)", "cmd_first_results": "first results",
              "cmd_first_render": "first paint", "cmd_index_load": "index load",
              "cmd_model_load": "model load", "first_query": "first query, worker"}
    for key, values in keys.items():
        if values:
            print(f"  {labels[key]:<32}{percentile(values, .5):9.2f}{percentile(values, .95):9.2f}{max(values):9.2f}")
    if not keys["cmd_model_load"]:
        print("model not loaded: rerun setup")
    status = verdict(percentile(b3, .95), TARGETS["B3_first_results_ms_p95"], unreliable=unreliable)
    return {"B3": {"p50": percentile(b3, .5), "p95": percentile(b3, .95), "n": len(b3), "dropped": dropped,
                   "verdict": status},
            "startup_stages": {k: {"p50": percentile(v, .5), "p95": percentile(v, .95)} for k, v in keys.items()}}


def keystroke(b, n, pause=0.0):
    """6.3 (B2 from the pty, B1 from the worker's TIMING lines).

    B2 is keystroke to the first byte of the redraw, which is the echo of the
    typed character: the picker redraws the query line before the worker
    answers. Key to results drawn comes from the picker's own
    `cmd_key_to_results` TIMING line. `pause` adds that many seconds before
    each key: past CMD_IMPRESSION_AGE_MS (200 ms) the picker logs the shown
    rows as impressions (an SQLite write on the UI thread) before the echo,
    as it does for a user who pauses."""
    offset = b.log_size()
    pid, fd = b.spawn_picker()
    redraw, misses = [], 0
    try:
        time.sleep(1.0)
        read_until_quiet(fd, idle=0.5, limit=4.0)
        for i in range(n):
            read_until_quiet(fd)
            if pause:
                time.sleep(pause)
                read_until_quiet(fd, idle=0.02)
            start = time.perf_counter()
            os.write(fd, bytes([KEY_CYCLE[i % len(KEY_CYCLE)]]))
            if not select.select([fd], [], [], MISS_MS / 1000.0)[0]:
                misses += 1
                redraw.append(MISS_MS)
                continue
            redraw.append((time.perf_counter() - start) * 1000.0)
            try:
                os.read(fd, 1 << 16)
            except OSError:
                break
            time.sleep(0.05)
    finally:
        stop_picker(pid, fd)
    lines = b.timings_since(offset)
    model_loaded = any(line["op"] == "cmd_model_load" for line in lines)
    queries = [line for line in lines if line["op"] == "cmd_query"][1:]
    results = [line["ms"] for line in lines if line["op"] == "cmd_key_to_results"][1:]
    total = [q["total_ms"] for q in queries]
    fresh = [q for q in queries if not q["reused"]]
    first_letter = [q["total_ms"] for q in fresh if q["query_len"] == 1 and not q["narrowed"]]
    no_model = not model_loaded or all(q["stage2"] == 0 for q in queries)
    unreliable = misses > MAX_DROPPED * n

    def line(name, values, digits=3):
        print(f"  {name:<40}{percentile(values, .5):9.{digits}f}{percentile(values, .95):9.{digits}f}"
              f"{max(values, default=float('nan')):9.{digits}f}{len(values):6d}")

    print(f"keystroke: {n} keys, pause {pause:.2f} s, {len(queries)} worker queries, {misses} misses, "
          f"load {load_average()}")
    print(f"{'':<42}{'p50':>9}{'p95':>9}{'max':>9}{'n':>6}")
    line("keystroke -> first byte, the echo (B2)", redraw)
    line("keystroke -> results drawn", results)
    line("worker per keystroke (B1)", total)
    line("  of those not reused", [q["total_ms"] for q in fresh])
    line("  first letter (full scan)", first_letter)
    line("  first letter, reused or not", [q["total_ms"] for q in queries
                                           if q["query_len"] == 1 and not q["narrowed"]])
    stages = {}
    for key in ("filter_ms", "heuristic_ms", "features_ms", "predict_ms", "blend_ms"):
        values = [q[key] for q in fresh if q[key] is not None]
        stages[key] = {"p50": percentile(values, .5), "p95": percentile(values, .95)}
        line(key + " (not reused)", values)
    matched = [q["matched"] for q in queries]
    stage2 = [q["stage2"] for q in queries]
    print(f"  matched p50 {percentile(matched, .5)}, max {max(matched, default=0)}; "
          f"stage2 p50 {percentile(stage2, .5)}, max {max(stage2, default=0)}; "
          f"reused {sum(q['reused'] for q in queries)}, narrowed {sum(q['narrowed'] for q in queries)}")
    by_len = {}
    for q in fresh:
        by_len.setdefault(q["query_len"], []).append(q["total_ms"])
    for length in sorted(by_len):
        v = by_len[length]
        print(f"  len {length}: not reused n {len(v):3d}, total p50 {percentile(v, .5):7.3f} p95 {percentile(v, .95):7.3f}")
    if no_model:
        print("model not loaded: rerun setup")
    return {
        "B2": {"p50": percentile(redraw, .5), "p95": percentile(redraw, .95), "n": len(redraw), "misses": misses,
               "pause_s": pause,
               "verdict": verdict(percentile(redraw, .95), TARGETS["B2_redraw_ms_p95"], unreliable=unreliable)},
        "key_to_results": {"p50": percentile(results, .5), "p95": percentile(results, .95), "n": len(results)},
        "B1": {"p50": percentile(total, .5), "p95": percentile(total, .95), "n": len(total),
               "p95_not_reused": percentile([q["total_ms"] for q in fresh], .95),
               "p95_first_letter": percentile(first_letter, .95),
               "verdict": verdict(percentile(total, .95), TARGETS["B1_worker_ms_p95"], unreliable=no_model)},
        "worker_stages_not_reused": stages,
    }


def track(b, n):
    """6.4 (B4): wall time of track-command against the perf DB."""
    ctx = b.context()
    times = []
    for i in range(n + 10):
        cmd = [str(BINARY), "--data-dir", str(b.data), "track-command", "--cwd", ctx["cwd"], "--exit-code", "0",
               "--duration-ms", "5", "--session", "bench", "--", f"bench command {i % 50}"]
        start = time.perf_counter()
        subprocess.run(cmd, env=b.env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd="/tmp")
        if i >= 10:
            times.append((time.perf_counter() - start) * 1000.0)
    print(f"track-command: {n} runs after 10 warm-up, load {load_average()}")
    print(f"  wall p50 {percentile(times, .5):.2f} ms, p95 {percentile(times, .95):.2f} ms, max {max(times):.2f} ms")
    return {"B4": {"p50": percentile(times, .5), "p95": percentile(times, .95), "max": max(times), "n": n,
                   "verdict": verdict(percentile(times, .95), TARGETS["B4_track_ms_p95"])}}


def train(b, n):
    """6.7 (B6): history --retrain, N times; the pinned model is restored after."""
    times = []
    model = b.data / "cmd_model.txt"
    for _ in range(n):
        model.unlink(missing_ok=True)
        start = time.perf_counter()
        out = b.psychic("history", "--retrain").stdout
        times.append(time.perf_counter() - start)
        # A run that found a training lock exits 0 at once without training.
        if not model.exists() or "Another training" in out:
            b.pin_model()
            fail("retrain did not train (a training lock?): " + out.strip()[:200])
    b.pin_model()
    times.sort()
    median = times[len(times) // 2]
    print(f"retrain: {n} runs, median {median:.1f} s, max {max(times):.1f} s, load {load_average()}")
    return {"B6": {"median": median, "max": max(times), "n": n,
                   "verdict": verdict(max(times), TARGETS["B6_retrain_s"])}}


def size(b):
    """6.7 (B7): events.db after VACUUM."""
    db = b.data / "events.db"
    sqlite3_cli(db, "VACUUM")
    mb = db.stat().st_size / 1e6
    print(f"events.db after VACUUM: {mb:.1f} MB")
    return {"B7": {"mb": mb, "verdict": verdict(mb, TARGETS["B7_db_mb"])}}


def twostage(b):
    """6.5 (B8), through `bench-history twostage` with the pinned model."""
    b.pin_model()
    out = b.psychic("internal", "bench-history", "twostage", "--queries", "400").stdout
    print(out.strip())
    values = {}
    for line in out.splitlines():
        if line.startswith("mean top-10 overlap:"):
            values["overlap"] = float(line.split(":")[1])
        elif line.startswith("top-1 agreement:"):
            values["top1"] = float(line.split(":")[1])
    ok = values.get("overlap", 0) >= TARGETS["B8_overlap"] and values.get("top1", 0) >= TARGETS["B8_top1"]
    return {"B8": {**values, "verdict": "PASS" if ok else "FAIL"}}


def latency(b, n, pause=0.0):
    """6.6: filter + rank without a terminal (an iteration aid, not a bar)."""
    b.pin_model()
    print(b.psychic("internal", "bench-history", "latency", "--keystrokes", str(n),
                    "--pause-ms", str(round(pause * 1000))).stdout.strip())
    print(f"load {load_average()}")


def print_table(r):
    print()
    print("perf DB: " + r["db"] + ", 40x120 pty" + " " * 6 + f"{'p50':>11}{'p95':>11}{'target':>9}  verdict")
    print(row("first results on screen (B3)", r["B3"]["p50"], r["B3"]["p95"], "50 ms", r["B3"]["verdict"]))
    print(row("keystroke -> redraw, first byte = echo (B2)", r["B2"]["p50"], r["B2"]["p95"], "16 ms", r["B2"]["verdict"]))
    print(row("  same, 0.3 s pause before each key", r["B2_paused"]["p50"], r["B2_paused"]["p95"], "16 ms",
              r["B2_paused"]["verdict"]))
    print(row("keystroke -> results drawn (info)", r["key_to_results"]["p50"], r["key_to_results"]["p95"], "-", ""))
    print(row("  same, 0.3 s pause before each key", r["key_to_results_paused"]["p50"],
              r["key_to_results_paused"]["p95"], "-", ""))
    print(row("worker per keystroke (B1)", r["B1"]["p50"], r["B1"]["p95"], "8 ms", r["B1"]["verdict"]))
    print(row("track-command wall (B4)", r["B4"]["p50"], r["B4"]["p95"], "10 ms", r["B4"]["verdict"]))
    print(f"{'retrain (B6)':<52}{r['B6']['median']:>8.1f}s  max{r['B6']['max']:>5.1f}s {'60 s':>9}  {r['B6']['verdict']}")
    print(f"{'events.db after VACUUM (B7)':<52}{r['B7']['mb']:>8.1f}MB{'':>11}{'300 MB':>9}  {r['B7']['verdict']}")
    b8 = r["B8"]
    print(f"{'two-stage top-10 overlap / top-1 agreement (B8)':<52}{b8.get('overlap', float('nan')):>8.4f}"
          f"{'':>3}{b8.get('top1', float('nan')):>8.4f}{'':>3}{'0.98/0.995':>9}  {b8['verdict']}")
    print(FILE_REFERENCE)


def run_all(b):
    results = {"date": time.strftime("%Y-%m-%d"), "load_before": load_average()}
    counts = b.psychic("internal", "bench-history", "stats").stdout.splitlines()
    stats = dict(line.split(": ", 1) for line in counts if ": " in line)
    results["db"] = f"{int(stats['commands']):,} commands / {int(stats['runs']):,} runs"
    results.update(startup(b, 30))
    results.update(keystroke(b, 200))
    slow = keystroke(b, 100, pause=0.3)
    results["B2_paused"] = slow["B2"]
    results["key_to_results_paused"] = slow["key_to_results"]
    if slow["B2"]["verdict"] != "PASS":
        results["B2"]["verdict"] = slow["B2"]["verdict"]
    results.update(train(b, 3))
    results.update(size(b))
    results.update(twostage(b))
    results.update(track(b, 200))
    results["load_after"] = load_average()
    (b.work / "cmd-results.json").write_text(json.dumps(results, indent=2) + "\n")
    print_table(results)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--work", required=True, type=Path,
                        help="scratch directory on a real disk (not tmpfs); holds cmd-perf and the pinned model")
    sub = parser.add_subparsers(dest="mode", required=True)
    s = sub.add_parser("setup")
    s.add_argument("--commands", type=int, default=50000)
    s.add_argument("--runs", type=int, default=500000)
    s.add_argument("--seed", type=int, default=1)
    s.add_argument("--profile", default="interactive", choices=["interactive", "repetitive", "agent"])
    s.add_argument("--anchor", type=int, default=None, help="default: now")
    s.add_argument("--from", dest="from_dir", help="copy events.db from this directory instead of generating")
    parsers = {}
    for name, default in (("startup", 30), ("keystroke", 200), ("track", 200), ("train", 3), ("latency", 300)):
        parsers[name] = sub.add_parser(name)
        parsers[name].add_argument("n", nargs="?", type=int, default=default)
    parsers["keystroke"].add_argument("--pause", type=float, default=0.0,
                                      help="seconds to wait before each key (impressions are due after 0.2 s)")
    parsers["latency"].add_argument("--pause", type=float, default=0.0,
                                    help="seconds to sleep before each recorded key (threads and clocks go idle)")
    for name in ("size", "twostage", "all"):
        sub.add_parser(name)
    args = parser.parse_args()

    home = check_environment(args.work)
    check_binary()
    args.work.mkdir(parents=True, exist_ok=True)
    b = Bench(args.work.resolve(), home)
    if args.mode != "setup" and not (b.data / "events.db").exists():
        fail("run ./bench/history.py setup first")
    if args.mode == "setup":
        setup(b, args)
    elif args.mode == "startup":
        startup(b, args.n)
    elif args.mode == "keystroke":
        keystroke(b, args.n, args.pause)
    elif args.mode == "track":
        track(b, args.n)
    elif args.mode == "train":
        train(b, args.n)
    elif args.mode == "size":
        size(b)
    elif args.mode == "twostage":
        twostage(b)
    elif args.mode == "latency":
        latency(b, args.n, args.pause)
    elif args.mode == "all":
        run_all(b)


if __name__ == "__main__":
    main()
