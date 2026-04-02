#!/bin/bash
# Run all analysis scripts in sequence.
# Usage: bash analysis/run_all.sh

set -e
DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=========================================="
echo "  Running all arb analysis scripts"
echo "=========================================="
echo

for script in \
    "$DIR/arb_summary.py" \
    "$DIR/arb_edge_histogram.py" \
    "$DIR/arb_timing.py" \
    "$DIR/arb_taker_vs_maker.py" \
    "$DIR/weather_vs_btc.py" \
    "$DIR/arb_reality_check.py"; do

    echo
    echo "--- $(basename "$script") ---"
    echo
    python3 "$script"
    echo
    echo "-------------------------------------------"
done

echo
echo "Done. All scripts finished."
