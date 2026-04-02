#!/usr/bin/env python3
"""
Compare taker execution vs maker execution for arb trades.
Shows why maker arbs are theoretically better but practically harder.

Run: python3 analysis/arb_taker_vs_maker.py
"""

import csv
from pathlib import Path

CSV_PATH = Path(__file__).parent.parent / "logs" / "paper_arb_trades.csv"

rows = []
with open(CSV_PATH) as f:
    for r in csv.DictReader(f):
        rows.append(r)

print(f"=== TAKER vs MAKER ARB COMPARISON ({len(rows)} trades) ===\n")

taker_total = 0.0
maker_total = 0.0
taker_profitable = 0
maker_profitable = 0

for r in rows:
    a = float(r["side_a_price"])
    b = float(r["side_b_price"])
    shares = float(r["shares"])
    combined = a + b
    edge = 1.0 - combined

    # --- TAKER: buy at ask, pay fees ---
    fee_rate_a = 0.25 * (a * (1 - a)) ** 2
    fee_rate_b = 0.25 * (b * (1 - b)) ** 2
    fee_per_share = a * fee_rate_a + b * fee_rate_b
    taker_profit = (edge - fee_per_share) * shares
    taker_total += taker_profit
    if taker_profit > 0:
        taker_profitable += 1

    # --- MAKER: post below ask, zero fees, but must get filled ---
    # You'd post at ask - 0.01 on each side (one tick below)
    # Combined becomes: (a - 0.01) + (b - 0.01) = combined - 0.02
    # Edge becomes: edge + 0.02 (better!)
    # Fee: $0 (maker)
    # But BOTH must fill
    maker_edge = edge + 0.02  # saving 1 cent on each side
    maker_profit = maker_edge * shares
    maker_total += maker_profit
    if maker_profit > 0:
        maker_profitable += 1

print("SCENARIO 1: TAKER (buy at ask, pay fees)")
print(f"  Profitable: {taker_profitable}/{len(rows)}")
print(f"  Total profit: ${taker_total:.2f}")
print(f"  Per trade: ${taker_total/len(rows):.4f}")
print()

print("SCENARIO 2: MAKER (post 1c below ask, zero fees)")
print(f"  Profitable: {maker_profitable}/{len(rows)}")
print(f"  Total profit (IF both fill): ${maker_total:.2f}")
print(f"  Per trade (IF both fill): ${maker_total/len(rows):.4f}")
print()

print("SCENARIO 3: REALISTIC MAKER (with fill rate estimates)")
for fill_pct in [0.70, 0.50, 0.30, 0.10, 0.05]:
    # Both sides must fill. If each has fill_pct chance:
    both_fill = fill_pct * fill_pct
    # If only one fills, you have directional risk (50/50 coin flip on BTC)
    one_fill = 2 * fill_pct * (1 - fill_pct)
    neither = (1 - fill_pct) ** 2

    # When both fill: full maker profit
    # When one fills: you hold to resolution, 50% win $1, 50% lose cost
    # Average single-side loss: you paid ~0.50 per share, 50% chance of $0
    # Expected value of single fill: 0.5 * 1.0 + 0.5 * 0.0 - cost = 0.5 - 0.5 = $0
    # Actually: cost is the price you paid. EV of holding = 0.5 (BTC is a coin flip)
    # So EV of single fill = 0.5 - price_paid. If price ~0.5, EV ~= 0.
    # Slight negative because you get the unfavorable side more often.
    # Conservative: assume single-fill EV = -0.02 per share (slight adverse selection)

    avg_shares = sum(float(r["shares"]) for r in rows) / len(rows)
    avg_cost_per_side = sum((float(r["side_a_price"]) + float(r["side_b_price"])) / 2
                           for r in rows) / len(rows)

    effective_profit = (
        both_fill * maker_total
        + one_fill * len(rows) * (-0.02 * avg_shares)
        + neither * 0
    )

    print(f"  Fill rate {fill_pct*100:.0f}% per side -> {both_fill*100:.1f}% both fill: "
          f"${effective_profit:.2f} total, ${effective_profit/len(rows):.4f}/trade")

print()
print("BOTTOM LINE:")
print("  Taker arbs work IF you can execute faster than other bots.")
print("  Maker arbs have better economics but terrible fill rates.")
print("  The arb DISAPPEARS the moment one side fills (market corrects).")
