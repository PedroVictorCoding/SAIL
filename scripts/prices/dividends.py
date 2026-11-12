"""Per-share dividend history for SAIL Capital (round 14), via yfinance.

Usage: python -I dividends.py SYMBOL [SYMBOL ...]  (Yahoo symbols, at most 100)
stdout: {"history": {symbol: [["YYYY-MM-DD", per_share], ...]}, "currency": {symbol: code},
         "failed": [symbol]}

Only the symbols leave the machine. Per-share amounts are in the listing's currency;
pence-quoted listings come back as GBX.
"""

import json
import math
import re
import sys

SYMBOL = re.compile(r"^[A-Za-z0-9^][A-Za-z0-9.\-^=]{0,24}$")
MINOR_UNITS = {"GBp": "GBX", "ZAc": "ZAX", "ILA": "ILA"}


def main(argv):
    symbols = [s for s in argv[1:101] if SYMBOL.match(s)]
    try:
        import yfinance as yf
    except Exception as exc:  # noqa: BLE001
        sys.stdout.write(json.dumps({"error": f"yfinance is not installed: {type(exc).__name__}"}))
        return 3
    history, currency, failed = {}, {}, []
    for symbol in symbols:
        try:
            ticker = yf.Ticker(symbol)
            series = ticker.dividends
            rows = []
            for stamp, value in series.items():
                value = float(value)
                if math.isfinite(value) and value > 0:
                    rows.append([stamp.date().isoformat(), round(value, 8)])
            history[symbol] = rows
            try:
                code = (ticker.history_metadata or {}).get("currency")
            except Exception:  # noqa: BLE001
                code = None
            if code is None:
                try:
                    code = ticker.fast_info.get("currency")
                except Exception:  # noqa: BLE001
                    code = None
            if code:
                currency[symbol] = MINOR_UNITS.get(code, code)
        except Exception:  # noqa: BLE001
            failed.append(symbol)
    sys.stdout.write(json.dumps({"history": history, "currency": currency, "failed": failed}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
