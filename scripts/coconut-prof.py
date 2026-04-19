#!/usr/bin/env python3
"""Parse coconutOS serial output and display a shard profiling report.

Reads the profiling summary table emitted by the supervisor at halt,
extracts shard lifecycle events from log lines, and parses per-token
inference benchmark data.

Usage:
    ./scripts/qemu-run.sh 2>&1 | python3 scripts/coconut-prof.py
    python3 scripts/coconut-prof.py /tmp/boot.log
    python3 scripts/coconut-prof.py /tmp/boot.log --baseline /tmp/native.log
"""

import sys
import re
import argparse


def parse_profiling_table(lines):
    """Extract rows from the '--- Shard Profiling Summary ---' table."""
    shards = []
    in_table = False
    header_seen = False

    for line in lines:
        line = line.rstrip()

        if "--- Shard Profiling Summary ---" in line:
            in_table = True
            header_seen = False
            continue

        if in_table and not header_seen:
            if line.lstrip().startswith("ID"):
                header_seen = True
            continue

        if in_table and header_seen:
            # Table rows: " 0      12           4523        8        45  gpu-hal"
            m = re.match(
                r"\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(.+)", line
            )
            if m:
                shards.append(
                    {
                        "id": int(m.group(1)),
                        "syscalls": int(m.group(2)),
                        "avg_cycles": int(m.group(3)),
                        "switches": int(m.group(4)),
                        "wall_ms": int(m.group(5)),
                        "name": m.group(6).strip(),
                    }
                )
            else:
                # End of table (blank line or non-matching)
                in_table = False

    return shards


def parse_lifecycle_events(lines):
    """Extract shard lifecycle events from log lines."""
    events = []
    for line in lines:
        line = line.rstrip()

        # "Shard N: creating (name)..."
        m = re.search(r"Shard (\d+): creating \(([^)]+)\)", line)
        if m:
            events.append((int(m.group(1)), "create", m.group(2)))
            continue

        # "Shard N: sys_exit(code)"
        m = re.search(r"Shard (\d+): sys_exit\((\d+)\)", line)
        if m:
            events.append((int(m.group(1)), "exit", m.group(2)))
            continue

        # "Shard N: blocked on channel ..."
        m = re.search(r"Shard (\d+):.*blocked on channel", line)
        if m:
            events.append((int(m.group(1)), "blocked", "channel"))
            continue

    return events


def parse_benchmark(lines):
    """Extract per-token benchmark data from serial output."""
    bench = {"tokens": [], "run_ms": None}

    for line in lines:
        line = line.rstrip()

        # "bench: total_tokens=16 total_cycles=12345 dim=32 layers=2"
        m = re.match(
            r"bench: total_tokens=(\d+) total_cycles=(\d+) dim=(\d+) layers=(\d+)",
            line,
        )
        if m:
            bench["total_tokens"] = int(m.group(1))
            bench["total_cycles"] = int(m.group(2))
            bench["dim"] = int(m.group(3))
            bench["layers"] = int(m.group(4))
            continue

        # "bench: token=0 cycles=1234"
        m = re.match(r"bench: token=(\d+) cycles=(\d+)", line)
        if m:
            bench["tokens"].append(
                {"pos": int(m.group(1)), "cycles": int(m.group(2))}
            )
            continue

        # "bench: run_ms=123" (emitted by supervisor)
        m = re.match(r"bench: run_ms=(\d+)", line)
        if m:
            bench["run_ms"] = int(m.group(1))
            continue

    return bench if bench["tokens"] else None


def parse_native_benchmark(lines):
    """Extract per-token benchmark data from native baseline output."""
    bench = {"tokens": []}

    for line in lines:
        line = line.rstrip()

        # "bench: total_tokens=16 total_ns=12345 dim=32 layers=2"
        m = re.match(
            r"bench: total_tokens=(\d+) total_ns=(\d+) dim=(\d+) layers=(\d+)",
            line,
        )
        if m:
            bench["total_tokens"] = int(m.group(1))
            bench["total_ns"] = int(m.group(2))
            bench["dim"] = int(m.group(3))
            bench["layers"] = int(m.group(4))
            continue

        # "bench: token=0 ns=1234"
        m = re.match(r"bench: token=(\d+) ns=(\d+)", line)
        if m:
            bench["tokens"].append(
                {"pos": int(m.group(1)), "ns": int(m.group(2))}
            )
            continue

    return bench if bench["tokens"] else None


def print_report(shards, events, bench, native_bench):
    """Print formatted profiling report with totals."""
    if not shards and not bench and not native_bench:
        print("No profiling data found in input.")
        return

    print("=" * 72)
    print("coconut-prof: Shard Profiling Report")
    print("=" * 72)
    print()

    # Summary table
    if shards:
        print(
            f"{'ID':>3}  {'Syscalls':>8}  {'Cycles/Call':>11}  "
            f"{'Switches':>8}  {'Wall (ms)':>9}  Name"
        )
        print("-" * 72)

        total_syscalls = 0
        total_switches = 0
        total_wall = 0

        for s in shards:
            print(
                f"{s['id']:>3}  {s['syscalls']:>8}  {s['avg_cycles']:>11}  "
                f"{s['switches']:>8}  {s['wall_ms']:>9}  {s['name']}"
            )
            total_syscalls += s["syscalls"]
            total_switches += s["switches"]
            total_wall += s["wall_ms"]

        print("-" * 72)
        print(
            f"{'':>3}  {total_syscalls:>8}  {'':>11}  "
            f"{total_switches:>8}  {total_wall:>9}  TOTAL"
        )
        print()

        # Per-shard syscall percentage
        if total_syscalls > 0:
            print("Syscall distribution:")
            for s in shards:
                pct = 100.0 * s["syscalls"] / total_syscalls
                bar = "#" * int(pct / 2)
                print(f"  {s['name']:<20} {pct:5.1f}%  {bar}")
            print()

    # Lifecycle events
    if events:
        print("Lifecycle events:")
        for shard_id, event, detail in events:
            print(f"  Shard {shard_id}: {event} ({detail})")
        print()

    # Inference benchmark
    if bench:
        print("=" * 72)
        print("Inference Benchmark (coconutOS)")
        print("=" * 72)
        print()

        if "dim" in bench:
            print(f"  Model: dim={bench['dim']}, layers={bench['layers']}")

        tokens = bench["tokens"]
        cycles = [t["cycles"] for t in tokens]

        if "total_cycles" in bench:
            print(f"  Total cycles: {bench['total_cycles']:,}")
        print(f"  Tokens: {len(tokens)}")

        if cycles:
            avg = sum(cycles) // len(cycles)
            mn = min(cycles)
            mx = max(cycles)
            sorted_c = sorted(cycles)
            p50 = sorted_c[len(sorted_c) // 2]
            p99 = sorted_c[int(len(sorted_c) * 0.99)]

            print(f"  Per-token cycles:  mean={avg:,}  min={mn:,}  max={mx:,}")
            print(f"                     p50={p50:,}  p99={p99:,}")

            # If we have run_ms from supervisor, estimate TSC frequency
            if bench.get("run_ms") and "total_cycles" in bench:
                run_ms = bench["run_ms"]
                if run_ms > 0:
                    # This is approximate — run_ms covers all shards, not just inference
                    tsc_hz = bench["total_cycles"] * 1000 // run_ms
                    print(f"  Est. TSC freq: ~{tsc_hz / 1e6:.0f} MHz")
                    total_ms = bench["total_cycles"] / (tsc_hz / 1000)
                    tok_per_sec = len(tokens) / (total_ms / 1000)
                    print(f"  Est. throughput: ~{tok_per_sec:.0f} tokens/sec")

        print()

        # Per-token detail
        print(f"  {'Token':>5}  {'Cycles':>14}")
        print(f"  {'-----':>5}  {'-' * 14:>14}")
        for t in tokens:
            print(f"  {t['pos']:>5}  {t['cycles']:>14,}")
        print()

    # Native baseline comparison
    if native_bench:
        print("=" * 72)
        print("Inference Benchmark (native baseline)")
        print("=" * 72)
        print()

        if "dim" in native_bench:
            print(
                f"  Model: dim={native_bench['dim']}, "
                f"layers={native_bench['layers']}"
            )

        tokens = native_bench["tokens"]
        ns_list = [t["ns"] for t in tokens]

        if "total_ns" in native_bench:
            total_ns = native_bench["total_ns"]
            total_ms = total_ns / 1e6
            tok_per_sec = len(tokens) / (total_ns / 1e9)
            print(f"  Total: {total_ms:.3f} ms")
            print(f"  Throughput: {tok_per_sec:,.0f} tokens/sec")

        if ns_list:
            avg_ns = sum(ns_list) // len(ns_list)
            mn_ns = min(ns_list)
            mx_ns = max(ns_list)
            print(
                f"  Per-token:  mean={avg_ns / 1e3:.1f} us  "
                f"min={mn_ns / 1e3:.1f} us  max={mx_ns / 1e3:.1f} us"
            )
        print()

    # Side-by-side comparison
    if bench and native_bench and bench.get("run_ms") and "total_cycles" in bench:
        run_ms = bench["run_ms"]
        if run_ms > 0 and "total_ns" in native_bench:
            tsc_hz = bench["total_cycles"] * 1000 // run_ms
            coconut_ms = bench["total_cycles"] / (tsc_hz / 1000)
            native_ms = native_bench["total_ns"] / 1e6

            print("=" * 72)
            print("Comparison")
            print("=" * 72)
            print()
            print(f"  {'':20} {'coconutOS':>14} {'Native':>14} {'Ratio':>10}")
            print(f"  {'-' * 20} {'-' * 14} {'-' * 14} {'-' * 10}")
            print(
                f"  {'Total (ms)':<20} {coconut_ms:>14.3f} {native_ms:>14.3f}"
                f" {coconut_ms / native_ms if native_ms > 0 else 0:>9.1f}x"
            )

            coconut_tps = len(bench["tokens"]) / (coconut_ms / 1000)
            native_tps = len(native_bench["tokens"]) / (native_ms / 1000)
            print(
                f"  {'Tokens/sec':<20} {coconut_tps:>14,.0f} {native_tps:>14,.0f}"
                f" {coconut_tps / native_tps if native_tps > 0 else 0:>9.2f}x"
            )
            print()


def main():
    parser = argparse.ArgumentParser(
        description="Parse coconutOS profiling and benchmark output."
    )
    parser.add_argument(
        "logfile",
        nargs="?",
        help="coconutOS serial log file (reads stdin if omitted)",
    )
    parser.add_argument(
        "--baseline",
        help="Native baseline log file (from bench-native)",
    )
    args = parser.parse_args()

    if args.logfile:
        with open(args.logfile) as f:
            lines = f.readlines()
    else:
        lines = sys.stdin.readlines()

    shards = parse_profiling_table(lines)
    events = parse_lifecycle_events(lines)
    bench = parse_benchmark(lines)

    native_bench = None
    if args.baseline:
        with open(args.baseline) as f:
            native_lines = f.readlines()
        native_bench = parse_native_benchmark(native_lines)

    print_report(shards, events, bench, native_bench)


if __name__ == "__main__":
    main()
