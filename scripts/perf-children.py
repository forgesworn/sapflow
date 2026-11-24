#!/usr/bin/env python3
"""Where the samples under chosen functions go, from `perf script` stacks.

    perf script -i perf.data --comms NAME -F ip,sym | perf-children.py FUNCTION...

For each FUNCTION (a substring of a symbol), prints its share of all the
samples read, then the functions it called directly and the functions the
samples ended in, each as a share of FUNCTION's own samples.
"""
import collections
import re
import sys

HASH = re.compile(r"::h[0-9a-f]{16}$")


def stacks(lines):
    stack = []
    for line in lines:
        line = line.strip()
        if not line:
            if stack:
                yield stack
            stack = []
            continue
        parts = line.split(None, 1)
        name = parts[1] if len(parts) == 2 else "[unknown]"
        name = HASH.sub("", name.split("+0x")[0]).replace("_$u7b$$u7b$closure$u7d$$u7d$", "{closure}")
        stack.append(name)
    if stack:
        yield stack


def main():
    targets = sys.argv[1:]
    if not targets:
        sys.exit(__doc__)
    total = 0
    under = collections.Counter()
    callees = collections.defaultdict(collections.Counter)
    leaves = collections.defaultdict(collections.Counter)
    for stack in stacks(sys.stdin):
        total += 1
        for target in targets:
            # Leaf first: the outermost frame matching is the one whose
            # children are wanted.
            index = max((i for i, name in enumerate(stack) if target in name), default=None)
            if index is None:
                continue
            under[target] += 1
            callees[target][stack[index - 1] if index else "(itself)"] += 1
            leaves[target][stack[0]] += 1
    print(f"samples: {total}")
    for target in targets:
        count = under[target]
        print(f"\n== {target}: {count} samples, {100 * count / max(total, 1):.1f}% of all")
        for title, table in (("called directly", callees[target]), ("ended in", leaves[target])):
            print(f"  {title}:")
            for name, n in table.most_common(18):
                print(f"    {100 * n / max(count, 1):5.1f}%  {name[:110]}")


if __name__ == "__main__":
    main()
