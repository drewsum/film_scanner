# Tooltip graphics

Drawings shown under tooltips that explain a control (#27): what levels' black point moves,
how sharpening's amount and radius shape an edge, which way the four frame orders number the
frames. Every tooltip drawing lives here; `src/tooltip_art.rs` builds them into the program
and renders them, and `response.on_hover_art(text, "name", &["part"])` makes the tooltip
(or `art(text, "name", &["part"])` for helpers such as `signed` that take a tooltip).

When you write a tooltip for a control whose meaning is easier to see than to read (a curve,
an edge, the film), add a drawing for it here and list it in `DRAWINGS`.

Conventions:

- SVG, about 260 to 310 x 100 to 160 px (`width`/`height` on the `<svg>` element, matched
  by `viewBox`), `font-family="sans-serif"`, `font-size="11"` (10 for captions).
- Colours are tokens filled from the GUI's visuals, so a drawing matches the light or dark
  theme:
  - `{bg}` the background, a shade darker than the tooltip: start with
    `<rect ... rx="8" fill="{bg}"/>` over the whole drawing;
  - `{fg}` text and lines, `{muted}` axes and guides, `{grid}` faint fills,
    `{accent}` emphasis, `{warn}` a warning tint.
  - `{red}`, `{green}`, `{blue}`, `{cyan}`, `{magenta}`, `{yellow}`, `{amber}` where the
    colour is what's shown (a balance slider's ends). Red, green and blue are the
    histogram's.
- Fixed colours only for picture content that has no theme: a grey photo, a black-to-white
  tone bar, the clipping marks.
- Any other token (`{black}`, `{radius}`, `{scanned}`) names a part a tooltip can point at:
  drawn in `{accent}` when the tooltip passes its name in `highlight`, else in `{fg}`. One
  drawing can serve several controls that way.
- Each drawing must be used by a tooltip, and must render (tests in `tooltip_art.rs`).
  To look at them all, in both themes:
  `TOOLTIP_ART_OUT=<folder> cargo test -p film-scanner preview -- --ignored`.

The film stocks' characteristic curves aren't here: `src/stock_curves.rs` draws them from
each stock's datasheet, with the same tokens, for the stock lists' tooltips and the Invert
step's sidebar.
