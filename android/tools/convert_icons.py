#!/usr/bin/env python3
"""Convert assets/source/icons/sail-icons.svg into Android vector drawables.

The web sprite draws every icon as a 24x24 stroke (fill none, round caps, width 1.6). Each
<symbol> becomes res/drawable/ic_sail_<id>.xml with the same geometry; circles and rects are
turned into paths because vector drawables only draw paths. Re-run after the sprite changes:

    python3 android/tools/convert_icons.py
"""
import pathlib
import xml.etree.ElementTree as ET

ROOT = pathlib.Path(__file__).resolve().parents[2]
SOURCE = ROOT / "assets/source/icons/sail-icons.svg"
OUT = ROOT / "android/app/src/main/res/drawable"
NS = "{http://www.w3.org/2000/svg}"
STROKE_WIDTH = "1.6"


def num(value):
    return float(value or 0)


def fmt(value):
    text = f"{value:.3f}".rstrip("0").rstrip(".")
    return text or "0"


def circle(el):
    cx, cy, r = num(el.get("cx")), num(el.get("cy")), num(el.get("r"))
    return (
        f"M{fmt(cx - r)},{fmt(cy)}a{fmt(r)},{fmt(r)} 0 1,0 {fmt(2 * r)},0"
        f"a{fmt(r)},{fmt(r)} 0 1,0 {fmt(-2 * r)},0Z"
    )


def rect(el):
    x, y = num(el.get("x")), num(el.get("y"))
    w, h = num(el.get("width")), num(el.get("height"))
    r = min(num(el.get("rx") or el.get("ry")), w / 2, h / 2)
    if r == 0:
        return f"M{fmt(x)},{fmt(y)}h{fmt(w)}v{fmt(h)}h{fmt(-w)}Z"
    return (
        f"M{fmt(x + r)},{fmt(y)}h{fmt(w - 2 * r)}a{fmt(r)},{fmt(r)} 0 0,1 {fmt(r)},{fmt(r)}"
        f"v{fmt(h - 2 * r)}a{fmt(r)},{fmt(r)} 0 0,1 {fmt(-r)},{fmt(r)}"
        f"h{fmt(-(w - 2 * r))}a{fmt(r)},{fmt(r)} 0 0,1 {fmt(-r)},{fmt(-r)}"
        f"v{fmt(-(h - 2 * r))}a{fmt(r)},{fmt(r)} 0 0,1 {fmt(r)},{fmt(-r)}Z"
    )


def path_data(el):
    tag = el.tag.replace(NS, "")
    if tag == "path":
        return el.get("d")
    if tag == "circle":
        return circle(el)
    if tag == "rect":
        return rect(el)
    raise ValueError(f"unsupported element <{tag}>")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    tree = ET.parse(SOURCE)
    names = []
    for symbol in tree.getroot().iter(f"{NS}symbol"):
        name = symbol.get("id").replace("-", "_")
        paths = [path_data(el) for el in symbol]
        body = "\n".join(
            "    <path\n"
            '        android:fillColor="#00000000"\n'
            '        android:strokeColor="#FFFFFFFF"\n'
            f'        android:strokeWidth="{STROKE_WIDTH}"\n'
            '        android:strokeLineCap="round"\n'
            '        android:strokeLineJoin="round"\n'
            f'        android:pathData="{d}" />'
            for d in paths
        )
        xml = (
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n"
            f"<!-- Generated from assets/source/icons/sail-icons.svg#{symbol.get('id')}"
            " by android/tools/convert_icons.py. Don't edit by hand. -->\n"
            '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
            '    android:width="24dp"\n'
            '    android:height="24dp"\n'
            '    android:viewportWidth="24"\n'
            '    android:viewportHeight="24">\n'
            f"{body}\n"
            "</vector>\n"
        )
        (OUT / f"ic_sail_{name}.xml").write_text(xml)
        names.append(name)
    print(f"wrote {len(names)} icons: {', '.join(names)}")


if __name__ == "__main__":
    main()
