"""Daily price history for one Yahoo symbol, via yfinance (SAIL Capital, round 10).

SAIL runs this as `python -I yfinance_daily.py SYMBOL` and reads one JSON object from
stdout. Only the symbol leaves the machine; nothing about holdings or the owner is sent.

Success: {"symbol", "currency", "timezone", "adjustment": "split_adjusted",
          "bars": [[date, open, high, low, close, volume], ...]} with numbers as strings.
Failure: {"error": "no_data" | "rate_limited" | "bad_symbol" | "failed", "detail": str}.
"""

import json
import math
from decimal import Decimal
import re
import sys

MAX_BARS = 10_000
SYMBOL = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.\-^=]{0,24}$")
# Yahoo quotes some listings in minor units; keep them distinct from the major currency.
MINOR_UNITS = {"GBp": "GBX", "ZAc": "ZAX", "ILA": "ILA"}


def emit(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")))
    sys.stdout.flush()


def number(value, whole=False):
    """A price as text with the 7 significant digits Yahoo stores (single precision),
    so `348.51001` reads `348.51`. Volumes are whole numbers."""
    if value is None:
        return None
    value = float(value)
    if not math.isfinite(value):
        return None
    if whole:
        return str(int(round(value)))
    text = format(Decimal(format(value, ".7g")).normalize(), "f")
    return text


def main(argv):
    if len(argv) != 2 or not SYMBOL.match(argv[1]):
        emit({"error": "bad_symbol", "detail": "expected one Yahoo symbol"})
        return 2
    symbol = argv[1]
    try:
        import yfinance as yf
        from yfinance.exceptions import YFRateLimitError
    except Exception as exc:  # noqa: BLE001
        emit({"error": "failed", "detail": f"yfinance is not installed: {type(exc).__name__}"})
        return 3
    try:
        debug = getattr(getattr(yf, "config", None), "debug", None)
        if debug is not None and hasattr(debug, "hide_exceptions"):
            debug.hide_exceptions = False
            options = {}
        else:
            options = {"raise_errors": True}
        ticker = yf.Ticker(symbol)
        history = ticker.history(
            period="max", interval="1d", auto_adjust=False, actions=False, **options
        )
        meta = ticker.history_metadata or {}
    except YFRateLimitError:
        emit({"error": "rate_limited", "detail": "Yahoo rate-limited the request"})
        return 4
    except Exception as exc:  # noqa: BLE001
        text = f"{type(exc).__name__} {exc}".lower()
        if "429" in text or "too many requests" in text or "ratelimit" in text:
            emit({"error": "rate_limited", "detail": type(exc).__name__})
            return 4
        unknown = ("missing", "delisted", "no data", "not found", "404")
        kind = "no_data" if any(word in text for word in unknown) else "failed"
        emit({"error": kind, "detail": type(exc).__name__})
        return 5
    if history is None or history.empty:
        emit({"error": "no_data", "detail": "no rows"})
        return 5

    bars = []
    for stamp, row in history.iterrows():
        close = number(row.get("Close"))
        if close is None:
            continue
        bars.append([
            stamp.date().isoformat(),
            number(row.get("Open")),
            number(row.get("High")),
            number(row.get("Low")),
            close,
            number(row.get("Volume"), whole=True),
        ])
    currency = meta.get("currency")
    emit({
        "symbol": symbol,
        "currency": MINOR_UNITS.get(currency, currency),
        "timezone": meta.get("exchangeTimezoneName"),
        # Yahoo's Close is adjusted for splits but not for distributions.
        "adjustment": "split_adjusted",
        "bars": bars[-MAX_BARS:],
    })
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
