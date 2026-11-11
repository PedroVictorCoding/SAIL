# README screenshot provenance

Captured from commit `192e552` using the unchanged `scripts/ui/library_fill_browser_test.py` fixture, at 1440 × 900 and 390 × 844 viewports. The fixture serves the actual Reader assets and synthetic API responses in an isolated Chromium context; no personal service is contacted. Images are unedited browser captures, visually reviewed for private data.

Reproduce from the repository root after installing the documented browser test dependencies:

```bash
mkdir -p /tmp/readme-shots
.venv-voice/bin/python scripts/ui/library_fill_browser_test.py /tmp/readme-shots
```

Copy `library-fill-1440.png` to `library-desktop.png` and `library-fill-390.png` to `library-mobile.png` after reviewing them. The test also checks reader toolbar behavior, embedded layout, horizontal overflow and JavaScript errors. Book titles, authors, timestamps and progress are synthetic fixture values.
