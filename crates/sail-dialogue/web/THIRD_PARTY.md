`mermaid.js` is the unmodified Mermaid 10.9.3 distribution from
https://cdn.jsdelivr.net/npm/mermaid@10.9.3/dist/mermaid.min.js.

Mermaid is distributed under the MIT license. The full license is included in `MERMAID_LICENSE.txt`.
Source and full license: https://github.com/mermaid-js/mermaid/tree/v10.9.3

`vendor/xterm/` contains @xterm/xterm 5.5.0 and @xterm/addon-fit 0.10.0,
from their pinned npm release tarballs. Both are MIT licensed (see
`vendor/xterm/LICENSE`). Source: https://github.com/xtermjs/xterm.js.
These assets are served locally; the browser needs no CDN connection.

`vendor/paper-shaders/paper-shaders.js` is a subset of @paper-design/shaders 0.0.82
(ShaderMount, MeshGradient, GrainGradient and their helpers), Apache License 2.0,
Copyright 2026 Paper — see `vendor/paper-shaders/LICENSE` and `NOTICE`. The published
ES modules were concatenated into one classic script that exposes `PaperShaders`;
the code is otherwise unmodified. Powered by Paper Shaders: https://shaders.paper.design
Source: https://github.com/paper-design/shaders.

`vendor/liquidglass/liquidglass.js` is the unmodified npm `dist/index.js` of
@ybouane/liquidglass 1.0.3 (MIT), which bundles html-to-image (MIT). Both notices are
in `vendor/liquidglass/LICENSE`. Source: https://github.com/ybouane/liquidglass.
It is loaded on demand only in the expressive ambient mode on desktop.
