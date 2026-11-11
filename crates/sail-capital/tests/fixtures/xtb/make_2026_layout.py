"""Writes xtb_2026_layout.xlsx: a synthetic export in the layout XTB's web platform produced
in November 2026 (three sheets, an account block above each table, "(UTC)" time headers,
times as plain spreadsheet serial numbers, totals rows, and open positions grouped as one
instrument row followed by its individual positions).

Every value is invented. Standard library only: run `python3 make_2026_layout.py`.
"""
import datetime as dt
import zipfile
from xml.sax.saxutils import escape

EPOCH = dt.datetime(1899, 12, 30)


def serial(y, mo, d, h=0, mi=0, s=0):
    """A UTC time as a spreadsheet serial number."""
    return (dt.datetime(y, mo, d, h, mi, s) - EPOCH).total_seconds() / 86400


ACCOUNT = 51234567
SUMMER = serial(2025, 7, 1, 14, 30)  # Lisbon is UTC+1 then, so a wrong zone shows.

closed = [
    ["Account number", ACCOUNT],
    ["Closed Positions"],
    ["Date from (UTC)", serial(2025, 1, 1)],
    ["Date to (UTC)", serial(2025, 12, 31, 23, 59, 59)],
    ["Instrument", "Ticker", "Category", "Type", "Volume", "Open Price", "Open Time (UTC)",
     "Close Price", "Close Time (UTC)", "Product", "Profit/Loss", "Gross Profit",
     "Purchase Value", "Sale Value", "Stop Loss", "Take Profit", "Commission", "Margin",
     "Swap", "Rollover", "Open Conversion Rate", "Close Conversion Rate", "Close Origin",
     "Position ID", "Comment"],
    ["Example Fruit Computers", "AAPL.US", "STOCK", "BUY", 5, 150.0, SUMMER, 180.0,
     serial(2025, 7, 2, 16, 0), "My Trades", 150.0, 150.0, 750.0, 900.0, None, None, 0.0,
     None, None, None, 1.0, 1.0, "Web", 1000000001, None],
    ["US 500", "US500", "CFD", "SELL", 0.1, 5500.0, serial(2025, 7, 3, 9, 0), 5450.0,
     serial(2025, 7, 4, 9, 0), "My Trades", 12.5, 12.5, None, None, None, None, 0.0, 55.0,
     -0.3, None, 1.0, 1.0, "Web", 1000000002, None],
    ["Profit/loss", None, None, None, None, None, None, None, None, None, 162.5, 162.5],
]

cash = [
    ["Account number", ACCOUNT],
    ["Cash Operations"],
    ["Date from (UTC)", serial(2025, 1, 1)],
    ["Date to (UTC)", serial(2025, 12, 31, 23, 59, 59)],
    ["Type", "Instrument", "Ticker", "Category", "Time", "Amount", "ID", "Comment", "Product",
     "Position ID"],
    ["Deposit", None, None, None, serial(2025, 6, 30, 10, 0), 2000.0, 2000000001, "Deposit",
     "My Trades", None],
    ["Stock purchase", "Example Fruit Computers", "AAPL.US", "STOCK", SUMMER, -1500.0,
     2000000002, "OPEN BUY 10 @ 150.00", "My Trades", 1000000001],
    ["Stock purchase", "TotalEnergies", "TTE.FR", "STOCK", serial(2025, 7, 1, 15, 0), -220.0,
     2000000003, "OPEN BUY TTE.FR 4 @ 55.00", "My Trades", 1000000003],
    ["Stock sell", "Example Fruit Computers", "AAPL.US", "STOCK", serial(2025, 7, 2, 16, 0),
     900.0, 2000000004, "CLOSE BUY 5/10 @ 180.00", "My Trades", 1000000001],
    ["Dividend", "Example Fruit Computers", "AAPL.US", "STOCK", serial(2025, 8, 15, 12, 0),
     1.13, 2000000005, "AAPL.US USD 0.2500/ SHR", "My Trades", None],
    ["Withholding tax", "Example Fruit Computers", "AAPL.US", "STOCK",
     serial(2025, 8, 15, 12, 0), -0.17, 2000000006, "AAPL.US USD WHT 15%", "My Trades", None],
    ["Close trade", "US 500", "US500", "CFD", serial(2025, 7, 4, 9, 0), 12.5, 2000000007,
     "Profit of position #1000000002", "My Trades", None],
    ["Swap", "US 500", "US500", "CFD", serial(2025, 7, 4, 9, 0), -0.3, 2000000008,
     "Swap of position #1000000002", "My Trades", None],
    ["Dividend equivalent", "US 500", "US500", "CFD", serial(2025, 7, 3, 21, 0), 0.4,
     2000000009, "US500 USD 0.4000/ CFD", "My Trades", None],
    ["Adjustment fee", "US 500", "US500", "CFD", serial(2025, 7, 3, 21, 0), -0.06, 2000000010,
     "US500 USD ADJ 15%", "My Trades", None],
    ["Free funds interest", None, None, None, serial(2025, 8, 1, 6, 0), 0.5, 2000000011,
     "Free-funds Interest 2025-07", "My Trades", None],
    ["Free funds interest tax", None, None, None, serial(2025, 8, 1, 6, 0), -0.1, 2000000012,
     "Free-funds Interest Tax 2025-07", "My Trades", None],
    ["Subaccount transfer", None, None, None, serial(2025, 8, 2, 6, 0), -10.0, 2000000013,
     "Transfer from 51234567 to 51234568", "My Trades", None],
    ["Spinoff", "TotalEnergies", "TTE.FR", "STOCK", serial(2025, 9, 1, 6, 0), 0.8, 2000000014,
     "TTE.FR EUR 0.2000/ SHR", "My Trades", None],
    ["Total", None, None, None, None, 1184.7],
]

open_ = [
    ["Account number", ACCOUNT],
    ["Open Positions"],
    ["Data as of report generated", serial(2025, 12, 31, 18, 0)],
    ["Product", "Metric", "Amount", "Currency"],
    ["My Trades", "Open position value", 1166.0, "EUR"],
    ["My Trades", "Open position profit", 216.0, "EUR"],
    ["Investment Plans", "Open position value", 0.0, "EUR"],
    ["Investment Plans", "Open position profit", 0.0, "EUR"],
    [],
    ["Note", "Summary values and open positions are shown as of the report generation time"],
    ["Product", "Instrument/Position", "Ticker", "Category", "Type", "Volume", "Value",
     "Current price", "Open price", "Open time (UTC)", "Stop Loss", "Take Profit",
     "Net Profit %", "Net Profit", "Gross Profit", "Margin", "Open Commission", "Swap",
     "Rollover"],
    ["My Trades", "Example Fruit Computers", "AAPL.US", "STOCK", None, 5, 950.0, None, 150.0,
     None, None, None, 26.67, 200.0, 200.0, None, None, None, None],
    ["My Trades", 1000000001, "AAPL.US", None, "BUY", 5, 950.0, 190.0, 150.0, SUMMER, None,
     None, 26.67, 200.0, 200.0, None, None, None, None],
    ["My Trades", "TotalEnergies", "TTE.FR", "STOCK", None, 4, 216.0, None, 55.0, None, None,
     None, -1.82, -4.0, -4.0, None, None, None, None],
    ["My Trades", 1000000003, "TTE.FR", None, "BUY", 2, 108.0, 54.0, 55.0,
     serial(2025, 7, 1, 15, 0), None, None, -1.82, -2.0, -2.0, None, None, None, None],
    ["My Trades", 1000000004, "TTE.FR", None, "BUY", 2, 108.0, 54.0, 55.0,
     serial(2025, 7, 1, 15, 0), None, None, -1.82, -2.0, -2.0, None, None, None, None],
]


def col_name(i):
    name = ""
    i += 1
    while i:
        i, r = divmod(i - 1, 26)
        name = chr(65 + r) + name
    return name


def sheet_xml(rows):
    out = ['<?xml version="1.0" encoding="UTF-8" standalone="yes"?>',
           '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
           "<sheetData>"]
    for r, row in enumerate(rows, start=1):
        cells = []
        for c, value in enumerate(row):
            ref = f"{col_name(c)}{r}"
            if value is None:
                continue
            if isinstance(value, str):
                cells.append(f'<c r="{ref}" t="inlineStr"><is><t>{escape(value)}</t></is></c>')
            else:
                cells.append(f'<c r="{ref}"><v>{value!r}</v></c>')
        out.append(f'<row r="{r}">{"".join(cells)}</row>')
    out.append("</sheetData></worksheet>")
    return "".join(out)


SHEETS = [("Closed Positions", closed), ("Cash Operations", cash), ("Open Positions", open_)]

with zipfile.ZipFile("xtb_2026_layout.xlsx", "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("[Content_Types].xml",
               '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
               '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
               '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
               '<Default Extension="xml" ContentType="application/xml"/>'
               '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
               + "".join(f'<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
                         for i in range(1, len(SHEETS) + 1))
               + "</Types>")
    z.writestr("_rels/.rels",
               '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
               '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
               '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>'
               "</Relationships>")
    z.writestr("xl/workbook.xml",
               '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
               '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
               'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>'
               + "".join(f'<sheet name="{escape(n)}" sheetId="{i}" r:id="rId{i}"/>'
                         for i, (n, _) in enumerate(SHEETS, start=1))
               + "</sheets></workbook>")
    z.writestr("xl/_rels/workbook.xml.rels",
               '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
               '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
               + "".join(f'<Relationship Id="rId{i}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{i}.xml"/>'
                         for i in range(1, len(SHEETS) + 1))
               + "</Relationships>")
    for i, (_, rows) in enumerate(SHEETS, start=1):
        z.writestr(f"xl/worksheets/sheet{i}.xml", sheet_xml(rows))
