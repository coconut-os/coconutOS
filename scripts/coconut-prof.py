#!/usr/bin/env python3
"""Parse coconutOS serial output and display a shard profiling report.

Reads the profiling summary table emitted by the supervisor at halt
and optionally extracts shard lifecycle events from log lines.

Usage:
    ./scripts/qemu-run.sh 2>&1 | python3 scripts/coconut-prof.py
    python3 scripts/coconut-prof.py /tmp/boot.log
"""

import sys
import re


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


def print_report(shards, events):
    """Print formatted profiling report with totals."""
    if not shards:
        print("No profiling data found in input.")
        return

    print("=" * 72)
    print("coconut-prof: Shard Profiling Report")
    print("=" * 72)
    print()

    # Summary table
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


def main():
    if len(sys.argv) > 1:
        with open(sys.argv[1]) as f:
            lines = f.readlines()
    else:
        lines = sys.stdin.readlines()

    shards = parse_profiling_table(lines)
    events = parse_lifecycle_events(lines)
    print_report(shards, events)


if __name__ == "__main__":
    main()
