#!/usr/bin/env python3
"""
Timing analysis: when do arbs appear? How long do they last?
Shows hourly distribution and gaps between detections.

Run: python3 analysis/arb_timing.py
"""

import csv
from pathlib import Path
from datetime import datetime, timezone
from collections import defaultdict

CSV_PATH = Path(__file__).parent.parent / "logs" / "paper_arb_trades.csv"

rows = []
with open(CSV_PATH) as f:
    for r in csv.DictReader(f):
        ts = datetime.fromisoformat(r["timestamp"].replace("Z", "+00:00"))
        r["_ts"] = ts
        rows.append(r)

if not rows:
    print("No data.")
    exit()

print(f"=== ARB TIMING ANALYSIS ({len(rows)} trades) ===\n")

# Hourly distribution (UTC)
hourly = defaultdict(int)
for r in rows:
    hourly[r["_ts"].hour] += 1

total_hours = (rows[-1]["_ts"] - rows[0]["_ts"]).total_seconds() / 3600

print("Hourly distribution (UTC):")
for h in range(24):
    count = hourly.get(h, 0)
    bar = "#" * count
    print(f"  {h:02d}:00  {count:3d}  {bar}")

print()

# Gaps between consecutive trades
gaps = []
for i in range(1, len(rows)):
    gap = (rows[i]["_ts"] - rows[i-1]["_ts"]).total_seconds()
    gaps.append(gap)

gaps.sort()
print("Gap between consecutive arb detections:")
print(f"  Min:    {gaps[0]:.0f}s ({gaps[0]/60:.1f}min)")
print(f"  Median: {gaps[len(gaps)//2]:.0f}s ({gaps[len(gaps)//2]/60:.1f}min)")
print(f"  Mean:   {sum(gaps)/len(gaps):.0f}s ({sum(gaps)/len(gaps)/60:.1f}min)")
print(f"  Max:    {gaps[-1]:.0f}s ({gaps[-1]/60:.1f}min)")
print()

# How many gaps < 60s (rapid-fire arbs on same window)
rapid = sum(1 for g in gaps if g < 60)
normal = sum(1 for g in gaps if 60 <= g < 600)
slow = sum(1 for g in gaps if g >= 600)
print(f"  <1min apart:   {rapid:3d} ({rapid/len(gaps)*100:.1f}%) -- likely same 5-min window")
print(f"  1-10min apart:  {normal:3d} ({normal/len(gaps)*100:.1f}%) -- consecutive windows")
print(f"  >10min apart:   {slow:3d} ({slow/len(gaps)*100:.1f}%) -- skipped windows")
print()

# BTC vs Weather timing
btc_gaps = []
wx_gaps = []
last_btc = None
last_wx = None
for r in rows:
    if "Bitcoin" in r["question"]:
        if last_btc:
            btc_gaps.append((r["_ts"] - last_btc).total_seconds())
        last_btc = r["_ts"]
    else:
        if last_wx:
            wx_gaps.append((r["_ts"] - last_wx).total_seconds())
        last_wx = r["_ts"]

if btc_gaps:
    print(f"BTC arb frequency:     1 every {sum(btc_gaps)/len(btc_gaps)/60:.1f}min avg")
if wx_gaps:
    print(f"Weather arb frequency: 1 every {sum(wx_gaps)/len(wx_gaps)/60:.1f}min avg")
print()

# Cooldown analysis
print("NOTE: The bot has a 5-minute cooldown per condition_id.")
print("This means it can only detect ONE arb per 5-min BTC market.")
print("If the arb persists for the full 5 minutes, there could be")
print("more opportunities within each window that are being missed,")
print("OR the arb is genuinely short-lived and only appears once.")
