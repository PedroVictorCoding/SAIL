"""Earnings dates for SAIL Capital (round 13), via yfinance.

Usage: python -I earnings.py SYMBOL [SYMBOL ...]  (Yahoo symbols, at most 100)
stdout: {"dates": {symbol: ["YYYY-MM-DD", ...]},
         "events": {symbol: [["YYYY-MM-DD", after_close|null, surprise_pct|null], ...]},
         "failed": [symbol]}

Round 24: `events` adds, per report, whether it came after the close (exchange-local hour
12 or later; before noon counts as before the open) and the EPS surprise in percent when
Yahoo gives both the estimate and the reported figure.

Only the symbols leave the machine. Funds, indices, currencies and commodities have no
earnings and come back empty.
"""

import json
import math
import re
import sys

SYMBOL = re.compile(r"^[A-Za-z0-9^][A-Za-z0-9.\-^=]{0,24}$")


def number(value):
    try:
        value = float(value)
    except (TypeError, ValueError):
        return None
    return value if math.isfinite(value) else None


def event_rows(frame):
    """One [date, after_close, surprise_pct] per report; the first row per date wins."""
    seen = {}
    for stamp, row in frame.iterrows():
        day = stamp.date().isoformat()
        if day in seen:
            continue
        hour = getattr(stamp, "hour", None)
        after_close = None if hour is None or (hour == 0 and stamp.minute == 0) else hour >= 12
        estimate = number(row.get("EPS Estimate"))
        reported = number(row.get("Reported EPS"))
        surprise = None
        if estimate is not None and reported is not None and estimate != 0:
            surprise = round((reported - estimate) / abs(estimate) * 100.0, 4)
        seen[day] = [day, after_close, surprise]
    return list(seen.values())


def main(argv):
    symbols = [s for s in argv[1:101] if SYMBOL.match(s)]
    try:
        import yfinance as yf
    except Exception as exc:  # noqa: BLE001
        sys.stdout.write(json.dumps({"error": f"yfinance is not installed: {type(exc).__name__}"}))
        return 3
    dates, events, failed = {}, {}, []
    for symbol in symbols:
        try:
            frame = yf.Ticker(symbol).get_earnings_dates(limit=16)
            if frame is None or frame.empty:
                dates[symbol] = []
                events[symbol] = []
                continue
            dates[symbol] = sorted({stamp.date().isoformat() for stamp in frame.index})
            events[symbol] = sorted(event_rows(frame))
        except Exception:  # noqa: BLE001
            failed.append(symbol)
    out = {"dates": dates, "events": events, "failed": failed}
    sys.stdout.write(json.dumps(out, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
