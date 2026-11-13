"""Research index membership for SAIL Capital (round 19), via public Wikipedia pages.

Usage: python -I universe.py [sp500] [stoxx50]
stdout: {"indexes": {"sp500": [...members...], "stoxx50": [...]},
         "failed": ["..."], "notes": ["..."]}

A member is {"yahoo", "name", "class": "stock", "membership": [[from, to|null], ...]}.
Wikipedia lists current members only; the notes say so. Prices are fetched separately by
SAIL, under the research: key, so this helper sends no instrument lists to a price source.
"""

import json
import re
import sys
import urllib.request
from datetime import date

from bs4 import BeautifulSoup

PAGES = {
    "sp500": "https://en.wikipedia.org/wiki/List_of_S%26P_500_companies",
    "stoxx50": "https://en.wikipedia.org/wiki/EURO_STOXX_50",
}
HEADERS = {"User-Agent": "SAIL-research/1.0 (private research)"}
SYMBOL = re.compile(r"^[A-Za-z0-9^][A-Za-z0-9.\-^=]{0,24}$")


def fetch(index):
    request = urllib.request.Request(PAGES[index], headers=HEADERS)
    with urllib.request.urlopen(request, timeout=60) as response:
        return BeautifulSoup(response.read().decode("utf-8"), "html.parser")


def columns(table):
    rows = table.find_all("tr")
    if not rows:
        return {}, []
    headings = [cell.get_text(" ", strip=True) for cell in rows[0].find_all(["th", "td"])]
    return {name: position for position, name in enumerate(headings)}, rows[1:]


def cell(row, position):
    values = row.find_all(["th", "td"])
    if position is None or position >= len(values):
        return ""
    return values[position].get_text(" ", strip=True)


def yahoo(symbol):
    symbol = symbol.strip().upper()
    if not SYMBOL.match(symbol):
        return None
    return symbol.replace(".", "-") if not any(suffix in symbol for suffix in (".AS", ".DE", ".PA", ".MI", ".MC", ".LS", ".BR", ".HE", ".ST", ".VI", ".IR")) else symbol


def date_added(value):
    match = re.fullmatch(r"(\d{4})-(\d{2})-(\d{2})", value.strip())
    if not match:
        return None
    try:
        return date(*(int(part) for part in match.groups())).isoformat()
    except ValueError:
        return None


def members(index):
    soup = fetch(index)
    table = soup.find("table", id="constituents")
    if table is None:
        raise ValueError("constituents table not found")
    at, rows = columns(table)
    symbol_at = at.get("Symbol") if index == "sp500" else at.get("Ticker")
    name_at = at.get("Security") if index == "sp500" else at.get("Name")
    added_at = at.get("Date added") if index == "sp500" else None
    out = []
    for row in rows:
        symbol = cell(row, symbol_at)
        ticker = yahoo(symbol)
        name = cell(row, name_at)
        if not ticker or not name:
            continue
        start = date_added(cell(row, added_at)) if added_at is not None else None
        out.append({
            "yahoo": ticker,
            "name": name,
            "class": "stock",
            "membership": [[start or "1900-01-01", None]],
        })
    return out


def main(argv):
    wanted = [value.strip().lower() for value in argv[1:] if value.strip().lower() in PAGES]
    wanted = wanted or list(PAGES)
    indexes, failed, notes = {}, [], []
    for index in wanted:
        try:
            indexes[index] = members(index)
            if not indexes[index]:
                raise ValueError("no members parsed")
        except Exception as exc:  # noqa: BLE001
            failed.append(index)
            notes.append(f"{index}: {type(exc).__name__}: {exc}")
    if indexes:
        notes.append("Wikipedia lists current members only; former members are not included.")
    sys.stdout.write(json.dumps({"indexes": indexes, "failed": failed, "notes": notes}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
