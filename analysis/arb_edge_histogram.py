#!/usr/bin/env python3
"""
Visual histogram of arb edges. Shows distribution of opportunities.

Run: python3 analysis/arb_edge_histogram.py
"""

import csv
from pathlib import Path

CSV_PATH = Path(__file__).parent.parent / "logs" / "paper_arb_trades.csv"

edges = []
with open(CSV_PATH) as f:
    for r in csv.DictReader(f):
        combined = float(r["combined"])
        edges.append((1.0 - combined) * 100)

if not edges:
    print("No data.")
    exit()

# Histogram in 0.5% bins from 1% to 15%
bins = {}
for e in edges:
    bucket = round(e * 2) / 2  # round to nearest 0.5
    bins[bucket] = bins.get(bucket, 0) + 1

print(f"Edge distribution ({len(edges)} trades)\n")
print(f"{'Edge %':>7} | {'Count':>5} | Chart")
print("-" * 60)

for pct in sorted(bins.keys()):
    count = bins[pct]
    bar = "=" * count
    print(f" {pct:5.1f}%  | {count:5d} | {bar}")

print()
print(f"Min edge:  {min(edges):.2f}%")
print(f"Max edge:  {max(edges):.2f}%")
print(f"Mean edge: {sum(edges)/len(edges):.2f}%")
print(f"Median:    {sorted(edges)[len(edges)//2]:.2f}%")
