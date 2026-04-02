#!/usr/bin/env python3
"""
Compare BTC arbs vs Weather arbs side by side.
Which market type is actually worth pursuing?

Run: python3 analysis/weather_vs_btc.py
"""

import csv
from pathlib import Path

CSV_PATH = Path(__file__).parent.parent / "logs" / "paper_arb_trades.csv"
WX_OUTCOME_PATH = Path(__file__).parent.parent / "logs" / "weather_outcomes.csv"

rows = []
with open(CSV_PATH) as f:
    for r in csv.DictReader(f):
        rows.append(r)

btc = [r for r in rows if "Bitcoin" in r["question"]]
wx = [r for r in rows if "Bitcoin" not in r["question"]]

def stats(subset, label):
    if not subset:
        print(f"\n{label}: No trades")
        return
    edges = [1.0 - float(r["combined"]) for r in subset]
    profits = [float(r["expected_profit"]) for r in subset]
    costs = [float(r["total_cost"]) for r in subset]

    # Taker fee adjusted
    taker_profits = []
    for r in subset:
        a = float(r["side_a_price"])
        b = float(r["side_b_price"])
        shares = float(r["shares"])
        edge = 1.0 - a - b
        fa = 0.25 * (a*(1-a))**2
        fb = 0.25 * (b*(1-b))**2
        taker_profits.append((edge - a*fa - b*fb) * shares)

    print(f"\n{'='*50}")
    print(f"  {label} ({len(subset)} trades)")
    print(f"{'='*50}")
    print(f"  Avg edge:         {sum(edges)/len(edges)*100:.2f}%")
    print(f"  Min/Max edge:     {min(edges)*100:.2f}% / {max(edges)*100:.2f}%")
    print(f"  Paper profit:     ${sum(profits):.2f}")
    print(f"  After taker fees: ${sum(taker_profits):.2f}")
    print(f"  Avg cost/trade:   ${sum(costs)/len(costs):.2f}")
    print(f"  Avg profit/trade: ${sum(taker_profits)/len(taker_profits):.4f}")

    # Unique markets
    conditions = set(r["condition_id"] for r in subset)
    print(f"  Unique markets:   {len(conditions)}")

stats(btc, "BTC 5-MINUTE ARBS")
stats(wx, "WEATHER ARBS")

# Weather outcomes if available
print()
if WX_OUTCOME_PATH.exists():
    wx_outcomes = []
    with open(WX_OUTCOME_PATH) as f:
        for r in csv.DictReader(f):
            wx_outcomes.append(r)
    if wx_outcomes:
        wins = sum(1 for r in wx_outcomes if r.get("won") == "true")
        losses = sum(1 for r in wx_outcomes if r.get("won") == "false")
        pnl = sum(float(r.get("pnl", 0)) for r in wx_outcomes)
        print(f"WEATHER EDGE OUTCOMES (not arbs -- directional bets):")
        print(f"  {wins}W / {losses}L, P&L: ${pnl:.2f}")
    else:
        print("Weather outcomes CSV empty.")
else:
    print("No weather outcomes CSV found.")

print()
print("COMPARISON:")
print()
print("  BTC arbs:")
print("    + High frequency (~8/hour)")
print("    + Market settles in 5 minutes (fast capital turnover)")
print("    - Extremely competitive (institutional arb bots)")
print("    - Likely phantom from WS update lag")
print("    - Coin flip risk if only one side fills")
print()
print("  Weather arbs:")
print("    + Less competition (niche market)")
print("    + Wider edges when they appear")
print("    + Can combine with directional weather edge knowledge")
print("    - Low frequency (~1/hour)")
print("    - Thin order books (might not fill at all)")
print("    - Markets settle in 24-48h (capital locked longer)")
print("    - Fewer markets to scan")
