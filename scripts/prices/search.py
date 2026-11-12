"""Instrument search for SAIL Capital, via yfinance.

SAIL runs this as `python -I search.py QUERY` and reads one JSON object from stdout.
Only the query leaves the machine. Results are limited to instrument types the trade
checks understand.

Success: {"results": [{"symbol", "name", "exchange", "quote_type"}, ...]}
Failure: {"error": "bad_query" | "rate_limited" | "failed", "detail": str}.
"""

import json
import re
import sys

MAX_RESULTS = 10
SYMBOL = re.compile(r"^[A-Za-z0-9^][A-Za-z0-9.\-^=]{0,24}$")
ALLOWED_TYPES = {
    "EQUITY",
    "ETF",
    "INDEX",
    "CURRENCY",
    "FUTURE",
    "CRYPTOCURRENCY",
    "MUTUALFUND",
}


def emit(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")))
    sys.stdout.flush()


def valid_query(text):
    return 1 <= len(text) <= 64 and not any(ord(ch) < 32 or ord(ch) == 127 for ch in text)


def main(argv):
    if len(argv) != 2:
        emit({"error": "bad_query", "detail": "expected one query"})
        return 2
    query = argv[1].strip()
    if not valid_query(query):
        emit({"error": "bad_query", "detail": "the query is empty or too long"})
        return 2
    try:
        import yfinance as yf
    except Exception as exc:  # noqa: BLE001
        emit({"error": "failed", "detail": f"yfinance is not installed: {type(exc).__name__}"})
        return 3
    try:
        search = yf.Search(query, max_results=MAX_RESULTS)
        quotes = getattr(search, "quotes", None) or []
    except Exception as exc:  # noqa: BLE001
        text = f"{type(exc).__name__} {exc}".lower()
        if "429" in text or "too many requests" in text or "ratelimit" in text:
            emit({"error": "rate_limited", "detail": type(exc).__name__})
            return 4
        emit({"error": "failed", "detail": type(exc).__name__})
        return 5

    results = []
    seen = set()
    for quote in quotes:
        symbol = str(quote.get("symbol") or "").strip()
        quote_type = str(quote.get("quoteType") or "").strip().upper()
        if not SYMBOL.match(symbol) or symbol in seen or quote_type not in ALLOWED_TYPES:
            continue
        name = str(quote.get("longname") or quote.get("shortname") or symbol).strip()
        exchange = str(quote.get("exchDisp") or quote.get("exchange") or "").strip()
        seen.add(symbol)
        results.append({
            "symbol": symbol,
            "name": name[:120],
            "exchange": exchange[:80],
            "quote_type": quote_type,
        })
        if len(results) >= MAX_RESULTS:
            break
    emit({"results": results})
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
