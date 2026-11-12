"""Earnings dates for SAIL Capital (round 13), via yfinance.

Usage: python -I earnings.py SYMBOL [SYMBOL ...]  (Yahoo symbols, at most 100)
stdout: {"dates": {symbol: ["YYYY-MM-DD", ...]}, "failed": [symbol]}

Only the symbols leave the machine. Funds, indices, currencies and commodities have no
earnings and come back empty.
"""

import json
import re
import sys

SYMBOL = re.compile(r"^[A-Za-z0-9^][A-Za-z0-9.\-^=]{0,24}$")


def main(argv):
    symbols = [s for s in argv[1:101] if SYMBOL.match(s)]
    try:
        import yfinance as yf
    except Exception as exc:  # noqa: BLE001
        sys.stdout.write(json.dumps({"error": f"yfinance is not installed: {type(exc).__name__}"}))
        return 3
    dates, failed = {}, []
    for symbol in symbols:
        try:
            frame = yf.Ticker(symbol).get_earnings_dates(limit=16)
            if frame is None or frame.empty:
                dates[symbol] = []
                continue
            dates[symbol] = sorted({stamp.date().isoformat() for stamp in frame.index})
        except Exception:  # noqa: BLE001
            failed.append(symbol)
    sys.stdout.write(json.dumps({"dates": dates, "failed": failed}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
