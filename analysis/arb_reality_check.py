#!/usr/bin/env python3
"""
THE BRUTAL REALITY CHECK.

Takes the paper arb data and applies every real-world friction
to estimate what you'd ACTUALLY make.

Run: python3 analysis/arb_reality_check.py
"""

import csv
from pathlib import Path

CSV_PATH = Path(__file__).parent.parent / "logs" / "paper_arb_trades.csv"

rows = []
with open(CSV_PATH) as f:
    for r in csv.DictReader(f):
        rows.append(r)

from datetime import datetime
first = datetime.fromisoformat(rows[0]["timestamp"].replace("Z", "+00:00"))
last = datetime.fromisoformat(rows[-1]["timestamp"].replace("Z", "+00:00"))
hours = (last - first).total_seconds() / 3600

print("=" * 65)
print("  ARBS REALITY CHECK: Can This Actually Make Money?")
print("=" * 65)
print()

# Step 1: Paper profit
paper_total = sum(float(r["expected_profit"]) for r in rows)
print(f"STEP 1: Paper profit (no fees, instant fill)")
print(f"  {len(rows)} trades over {hours:.1f}h = ${paper_total:.2f}")
print(f"  Projected 24h: ${paper_total / hours * 24:.2f}")
print()

# Step 2: After taker fees
taker_total = 0.0
for r in rows:
    a = float(r["side_a_price"])
    b = float(r["side_b_price"])
    shares = float(r["shares"])
    edge = 1.0 - a - b
    fee_a = 0.25 * (a * (1 - a)) ** 2
    fee_b = 0.25 * (b * (1 - b)) ** 2
    fee = a * fee_a + b * fee_b
    taker_total += (edge - fee) * shares

print(f"STEP 2: After taker fees (max 1.56% per side)")
print(f"  ${taker_total:.2f} ({taker_total/paper_total*100:.0f}% of paper)")
print(f"  Fee drag: ${paper_total - taker_total:.2f}")
print(f"  Projected 24h: ${taker_total / hours * 24:.2f}")
print()

# Step 3: Execution reality for TAKER strategy
print(f"STEP 3: Taker execution reality")
print()
print(f"  You need to LIFT the ask on BOTH sides simultaneously.")
print(f"  Problems:")
print(f"    a) The ask you see via WS may be gone by the time your")
print(f"       REST POST /order arrives (50-200ms round trip)")
print(f"    b) Other arb bots see the same opportunity")
print(f"    c) 34% of prices have SDK decimal artifacts")
print(f"    d) You have NO order book depth data -- the ask might")
print(f"       be for 0.1 shares, not the 8 you want")
print()

# Estimate: what fraction of arbs can you actually capture as taker?
# - Polymarket has well-known arb bots (institutional)
# - BTC 5-min markets have high activity
# - Your latency: WS detection ~instant, but REST order placement ~100-200ms
# - Institutional bots: colocated, <10ms
# - Realistic capture rate: 5-15% of opportunities

for capture in [0.50, 0.25, 0.10, 0.05]:
    projected = taker_total * capture / hours * 24
    print(f"  If you capture {capture*100:.0f}% of opportunities: ${projected:.2f}/day")

print()

# Step 4: Maker strategy analysis
print(f"STEP 4: Maker strategy (zero fees, but fill risk)")
print()
maker_edge_total = 0.0
for r in rows:
    a = float(r["side_a_price"])
    b = float(r["side_b_price"])
    shares = float(r["shares"])
    # Post 1 tick below ask on each side, zero fees
    maker_edge = (1.0 - (a - 0.01) - (b - 0.01)) * shares
    maker_edge_total += maker_edge

print(f"  If both sides fill (1c below ask, zero fees): ${maker_edge_total:.2f}")
print()
print(f"  WHY MAKER ARBS MOSTLY DON'T WORK:")
print(f"    - You post YES bid at 0.51, NO bid at 0.46")
print(f"    - For BOTH to fill, someone must SELL to you on BOTH sides")
print(f"    - But arbs exist because nobody is selling! The ask is just")
print(f"      slightly mispriced. It will correct, not trade through.")
print(f"    - If only YES fills: you hold a 50/50 coin flip position")
print(f"    - Single-fill rate on BTC 5-min: probably 10-30%")
print(f"    - Both-fill rate: 1-9% (multiply individual rates)")
print()

for both_fill in [0.09, 0.04, 0.01]:
    # Single fill: assume 50% loss (coin flip minus entry cost)
    single_fill_rate = 2 * (both_fill ** 0.5) * (1 - both_fill ** 0.5)
    avg_shares = sum(float(r["shares"]) for r in rows) / len(rows)
    avg_price = sum((float(r["side_a_price"]) + float(r["side_b_price"])) / 2
                    for r in rows) / len(rows)
    # On single fill: you paid price p, EV = 0.50 (coin flip), loss = p - 0.50
    # But adverse selection: you get filled on the WRONG side more often
    # If BTC is going up, your YES bid fills (good) but then it already moved
    # Actually: your bid fills because someone is SELLING, meaning they think
    # price should be LOWER. Adverse selection means single fills lose more.
    single_fill_ev = -0.05 * avg_shares  # slight negative from adverse selection

    total_ev = (
        both_fill * maker_edge_total
        + single_fill_rate * len(rows) * single_fill_ev
    )
    daily = total_ev / hours * 24
    print(f"  Both fill {both_fill*100:.0f}%: ${daily:.2f}/day "
          f"(${total_ev:.2f} over {hours:.0f}h)")

print()

# Step 5: The stale quote problem
print(f"STEP 5: Are these REAL opportunities?")
print()
print(f"  The bot logs ~300 'stale complement prices' SKIPPED per cycle.")
print(f"  This means for most token pairs, the complement price is >2min old.")
print(f"  When you finally see both sides 'fresh', one might still be stale")
print(f"  in practice -- just within the 2-minute window.")
print()
print(f"  BTC 5-min markets have ~50/50 pricing (yes~0.50, no~0.50).")
print(f"  If BTC ticks up, YES ask drops and NO ask rises.")
print(f"  The WS might update YES before NO, creating a PHANTOM arb:")
print(f"    YES new ask: 0.48 (just updated)")
print(f"    NO old ask:  0.49 (hasn't updated yet)")
print(f"    Combined: 0.97 -> 3% 'edge'!")
print(f"    Reality: once NO updates, combined = 0.48 + 0.52 = 1.00")
print()
print(f"  This is the BID-ASK BOUNCE problem. It creates the ILLUSION")
print(f"  of arb opportunities that disappear in milliseconds.")
print()

# Step 6: Competition
print(f"STEP 6: Competition")
print()
print(f"  Polymarket BTC 5-min markets are the HIGHEST VOLUME markets.")
print(f"  Known participants:")
print(f"    - Institutional market makers (sub-10ms execution)")
print(f"    - Dedicated arb bots (dozens, well-funded)")
print(f"    - The market maker IS the arb bot in many cases")
print(f"  If a real 3% arb existed for more than 100ms, it would be")
print(f"  taken by a faster bot. The fact that your bot sees ~8/hour")
print(f"  and they persist long enough to detect suggests they are")
print(f"  PHANTOM (stale quotes) not REAL (executable) opportunities.")
print()

# Final verdict
print("=" * 65)
print("  VERDICT")
print("=" * 65)
print()
print("  Q: Can taker arbs make money?")
print("  A: MAYBE $1-5/day if you're fast enough and the arbs are real.")
print(f"     Paper says ${taker_total/hours*24:.2f}/day, but 70-95% of that")
print(f"     evaporates from stale quotes, competition, and partial fills.")
print()
print("  Q: Can maker arbs make money?")
print("  A: UNLIKELY. Both sides almost never fill. Single fills lose")
print("     money from adverse selection on coin-flip BTC markets.")
print()
print("  Q: What about weather arbs?")
btc_rows = [r for r in rows if "Bitcoin" in r["question"]]
wx_rows = [r for r in rows if "Bitcoin" not in r["question"]]
print(f"  A: {len(wx_rows)} weather arbs detected vs {len(btc_rows)} BTC arbs.")
print(f"     Weather markets have WIDER spreads and LOWER liquidity.")
print(f"     Arb edges are larger (less competition) but:")
print(f"     - Order books are thin: you might move the market with $5")
print(f"     - Fills are slower: maker orders may sit for hours")
print(f"     - Weather arbs might be more real since fewer bots trade them")
print()
print("  RECOMMENDATION:")
print("     1. Before going live, fetch /book depth for every arb signal")
print("        and log available liquidity at the ask price")
print("     2. Add latency tracking: measure ms from WS event to when")
print("        you COULD place an order")
print("     3. Run a 'shadow execution' test: when you see an arb,")
print("        immediately re-fetch both order books via REST and check")
print("        if the arb still exists 100-500ms later")
print("     4. Weather arbs are more promising than BTC arbs for a")
print("        small bot -- less competition, wider edges")
