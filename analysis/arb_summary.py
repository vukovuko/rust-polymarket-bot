#!/usr/bin/env python3
"""
Full arb trade analysis: edge distribution, taker fee impact,
price quality, timing, and realistic profit estimate.

Run: python3 analysis/arb_summary.py
"""

import csv
import sys
from pathlib import Path
from datetime import datetime

CSV_PATH = Path(__file__).parent.parent / "logs" / "paper_arb_trades.csv"

if not CSV_PATH.exists():
    print(f"No CSV found at {CSV_PATH}")
    sys.exit(1)

rows = []
with open(CSV_PATH) as f:
    reader = csv.DictReader(f)
    for r in reader:
        rows.append(r)

if not rows:
    print("CSV is empty.")
    sys.exit(1)

print(f"=== ARB TRADE ANALYSIS ({len(rows)} trades) ===\n")

# --- Shadow execution: verified vs phantom ---
has_verified_col = "verified" in rows[0]
if has_verified_col:
    verified_rows = [r for r in rows if r.get("verified") == "true"]
    phantom_rows = [r for r in rows if r.get("verified") == "false"]
    unverified_rows = [r for r in rows if r.get("verified") not in ("true", "false")]

    v_profit = sum(float(r["expected_profit"]) for r in verified_rows)
    p_profit = sum(float(r["expected_profit"]) for r in phantom_rows)

    print("0. SHADOW EXECUTION RESULTS")
    print(f"   Verified:   {len(verified_rows):4d} ({len(verified_rows)/len(rows)*100:.0f}%)  paper profit: ${v_profit:.2f}")
    print(f"   Phantom:    {len(phantom_rows):4d} ({len(phantom_rows)/len(rows)*100:.0f}%)  paper profit: ${p_profit:.2f}")
    if unverified_rows:
        print(f"   Unverified: {len(unverified_rows):4d} (REST fetch failed)")
    print()

    # Verified-only taker profit
    v_taker = 0.0
    for r in verified_rows:
        a = float(r["side_a_price"])
        b = float(r["side_b_price"])
        shares = float(r["shares"])
        edge = 1.0 - a - b
        fee_a = 0.25 * (a * (1 - a)) ** 2
        fee_b = 0.25 * (b * (1 - b)) ** 2
        fee = a * fee_a + b * fee_b
        v_taker += (edge - fee) * shares

    print(f"   Verified profit after taker fees: ${v_taker:.2f}")
    print(f"   THIS is the real edge number. Everything below includes phantoms.")
    print()

    # REST vs WS price comparison for verified arbs
    if verified_rows:
        rest_deltas = []
        for r in verified_rows:
            ws_combined = float(r["combined"])
            rest_a = r.get("rest_a_price", "-")
            rest_b = r.get("rest_b_price", "-")
            if rest_a != "-" and rest_b != "-":
                rest_combined = float(rest_a) + float(rest_b)
                rest_deltas.append(rest_combined - ws_combined)
        if rest_deltas:
            avg_delta = sum(rest_deltas) / len(rest_deltas) * 100
            print(f"   REST vs WS price delta (verified): avg {avg_delta:+.2f}% (positive = REST tighter)")

    # Depth stats for verified arbs
    depths_a = [float(r["depth_a"]) for r in verified_rows if r.get("depth_a", "-") != "-"]
    depths_b = [float(r["depth_b"]) for r in verified_rows if r.get("depth_b", "-") != "-"]
    if depths_a:
        print(f"   Depth at best ask (verified): median {sorted(depths_a)[len(depths_a)//2]:.1f} / {sorted(depths_b)[len(depths_b)//2]:.1f} shares")
    print()
else:
    print("(No verified/phantom data — old CSV format)\n")

# --- Basic stats ---
total_paper_profit = 0.0
total_cost = 0.0
btc_count = 0
btc_profit = 0.0
weather_count = 0
weather_profit = 0.0
edges = []
taker_profits = []
suspicious_price_count = 0

# Edge buckets
buckets = {
    "2-3%": 0, "3-4%": 0, "4-5%": 0, "5-6%": 0,
    "6-7%": 0, "7-10%": 0, "10%+": 0,
}

for r in rows:
    a = float(r["side_a_price"])
    b = float(r["side_b_price"])
    combined = float(r["combined"])
    shares = float(r["shares"])
    cost = float(r["total_cost"])
    paper_profit = float(r["expected_profit"])
    question = r["question"]

    edge = 1.0 - combined
    edges.append(edge)
    total_paper_profit += paper_profit
    total_cost += cost

    # Market type
    if "Bitcoin" in question:
        btc_count += 1
        btc_profit += paper_profit
    else:
        weather_count += 1
        weather_profit += paper_profit

    # Edge bucket
    pct = edge * 100
    if pct < 3:
        buckets["2-3%"] += 1
    elif pct < 4:
        buckets["3-4%"] += 1
    elif pct < 5:
        buckets["4-5%"] += 1
    elif pct < 6:
        buckets["5-6%"] += 1
    elif pct < 7:
        buckets["6-7%"] += 1
    elif pct < 10:
        buckets["7-10%"] += 1
    else:
        buckets["10%+"] += 1

    # Taker fee: fee_rate = 0.25 * (p*(1-p))^2
    fee_rate_a = 0.25 * (a * (1 - a)) ** 2
    fee_rate_b = 0.25 * (b * (1 - b)) ** 2
    total_fee_per_share = a * fee_rate_a + b * fee_rate_b
    profit_after_fees = (edge - total_fee_per_share) * shares
    taker_profits.append(profit_after_fees)

    # Suspicious price check (9004975 pattern)
    if "9004975" in r["side_a_price"] or "9004975" in r["side_b_price"]:
        suspicious_price_count += 1

total_taker_profit = sum(taker_profits)
profitable_count = sum(1 for p in taker_profits if p > 0)

# Timing
first_ts = datetime.fromisoformat(rows[0]["timestamp"].replace("Z", "+00:00"))
last_ts = datetime.fromisoformat(rows[-1]["timestamp"].replace("Z", "+00:00"))
hours = (last_ts - first_ts).total_seconds() / 3600

# --- Print results ---
print("1. PAPER PROFIT (no fees)")
print(f"   Total: ${total_paper_profit:.2f}")
print(f"   Per trade: ${total_paper_profit / len(rows):.4f}")
print(f"   Capital deployed: ${total_cost:.2f}")
print()

print("2. PROFIT AFTER TAKER FEES")
print(f"   Total: ${total_taker_profit:.2f}")
print(f"   Per trade: ${total_taker_profit / len(rows):.4f}")
print(f"   Fee drag: ${total_paper_profit - total_taker_profit:.2f}")
print(f"   Profitable trades: {profitable_count}/{len(rows)} ({profitable_count/len(rows)*100:.1f}%)")
print()

print("3. EDGE DISTRIBUTION")
for label, count in buckets.items():
    bar = "#" * count
    print(f"   {label:>5}: {count:3d}  {bar}")
avg_edge = sum(edges) / len(edges) * 100
print(f"   Average edge: {avg_edge:.2f}%")
print()

print("4. BY MARKET TYPE")
print(f"   BTC:     {btc_count:3d} trades, ${btc_profit:.2f} paper profit")
print(f"   Weather: {weather_count:3d} trades, ${weather_profit:.2f} paper profit")
print()

print("5. PRICE QUALITY")
print(f"   Suspicious (9004975 pattern): {suspicious_price_count}/{len(rows)} ({suspicious_price_count/len(rows)*100:.1f}%)")
print(f"   These are SDK decimal artifacts, not real tick prices.")
print(f"   Impact: ~$0.001/share on affected trades (negligible).")
print()

print("6. TIMING")
print(f"   Timespan: {hours:.1f} hours")
print(f"   Rate: {len(rows)/hours:.1f} trades/hour")
print(f"   ~1 arb per {60*hours/len(rows):.1f} minutes")
print()

print("7. REALISTIC DAILY PROFIT ESTIMATE")
daily_rate = len(rows) / hours * 24
daily_taker = total_taker_profit / hours * 24
print(f"   If running 24h with taker orders: ~{daily_rate:.0f} trades, ~${daily_taker:.2f}/day")
print(f"   BUT: this assumes instant fills at displayed ask prices.")
print(f"   Real-world discount factors:")
print(f"     - Fill rate (both sides must fill): 30-70%")
print(f"     - Adverse selection (stale quotes gone by execution): 20-50%")
print(f"     - Competition (other bots take the arb first): 50-80%")
print(f"   Realistic range: ${daily_taker * 0.05:.2f} - ${daily_taker * 0.30:.2f}/day")

if has_verified_col and verified_rows:
    print()
    print("8. VERIFIED-ONLY DAILY ESTIMATE")
    v_hours = hours  # same timespan
    v_daily_rate = len(verified_rows) / v_hours * 24
    v_daily_taker = v_taker / v_hours * 24
    print(f"   Verified arbs: {len(verified_rows)} over {v_hours:.1f}h = {v_daily_rate:.1f}/day")
    print(f"   Verified profit after taker fees: ${v_daily_taker:.2f}/day")
    print(f"   Realistic (5-30% capture): ${v_daily_taker * 0.05:.2f} - ${v_daily_taker * 0.30:.2f}/day")
    print(f"   Phantom rate: {len(phantom_rows)/len(rows)*100:.0f}% — this is the key metric")
