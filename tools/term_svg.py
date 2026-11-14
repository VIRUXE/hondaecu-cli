"""Draw a captured terminal session as an SVG "screenshot" for the README.

usage: python tools/term_svg.py <capture.txt> [<out.svg>] [--title TEXT]

The capture is plain text. A line starting with "$ " is the command as typed;
every other line is shown exactly as the tool printed it. A line holding only
"..." marks output cut for the picture and is drawn dimmed. With no <out.svg>,
the picture is written next to the capture with the extension .svg.

Colours: the VERDICT line and the words "ok:", "PROBLEM:" and "could not
tell:" are green, red and amber; section summaries ("yes", "no", "could not
tell") the same. Nothing else about the text is changed.

To regenerate every picture in docs/img:

    for f in docs/img/*.txt; do python tools/term_svg.py "$f"; done

Standard library only.
"""
import os
import re
import sys
from xml.sax.saxutils import escape

FONT_SIZE = 13
CHAR_W = 7.8          # advance of one monospaced character at FONT_SIZE
LINE_H = 18
PAD_X = 18
PAD_TOP = 48          # below the title bar
PAD_BOTTOM = 18
BAR_H = 30

BG = "#0d1117"
BAR = "#1f2630"
FG = "#c9d1d9"
DIM = "#6e7681"
GREEN = "#3fb950"
RED = "#f85149"
AMBER = "#d29922"
CYAN = "#58a6ff"
WHITE = "#f0f6fc"

STATE_WORDS = [("could not tell:", AMBER), ("PROBLEM:", RED), ("ok:", GREEN)]
SECTION = re.compile(r"^([A-Z][A-Z ,]+ \.+ )(yes|no|could not tell)$")
ITEM = re.compile(r"^  \S.* \.+ $")   # "  warm idle ................ "
VERDICT_COLOUR = {"WORTH BURNING": GREEN, "DO NOT BURN": RED, "COULD NOT TELL": AMBER}


def span(text, colour=None, bold=False):
    attrs = ""
    if colour:
        attrs += ' fill="%s"' % colour
    if bold:
        attrs += ' font-weight="bold"'
    return "<tspan%s>%s</tspan>" % (attrs, escape(text)) if attrs else escape(text)


def colour_line(line):
    """The line as a list of SVG fragments; the characters are unchanged."""
    if line.startswith("$ "):
        return [span("$ ", GREEN, True), span(line[2:], WHITE, True)]
    if line.strip() == "...":
        return [span(line, DIM)]
    if line.startswith("VERDICT: "):
        for word, colour in VERDICT_COLOUR.items():
            if line[9:].startswith(word):
                return [span(line, colour, True)]
    m = SECTION.match(line)
    if m:
        colour = {"yes": GREEN, "no": RED}.get(m.group(2), AMBER)
        return [span(m.group(1), WHITE, True), span(m.group(2), colour, True)]
    if line.startswith("ROM note:"):
        return [span("ROM note:", CYAN, True), span(line[9:])]
    if line.startswith("NOT JUDGED:"):
        return [span(line, DIM)]
    if line.isupper() and line.strip():
        return [span(line, WHITE, True)]
    for word, colour in STATE_WORDS:
        i = line.find(word)
        if i >= 0 and ITEM.match(line[:i]):
            return [span(line[:i]), span(word, colour, True), span(line[i + len(word):])]
    return [span(line)]


def render(lines, title):
    width_chars = max([len(l) for l in lines] + [60])
    w = int(PAD_X * 2 + width_chars * CHAR_W)
    h = PAD_TOP + LINE_H * len(lines) + PAD_BOTTOM
    out = [
        '<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" '
        'viewBox="0 0 %d %d" role="img" aria-label="%s">' % (w, h, w, h, escape(title, {'"': "&quot;"})),
        '<rect width="%d" height="%d" rx="8" fill="%s"/>' % (w, h, BG),
        '<path d="M8 0H%d a8 8 0 0 1 8 8V%dH0V8a8 8 0 0 1 8-8z" fill="%s"/>' % (w - 8, BAR_H, BAR),
        '<circle cx="20" cy="15" r="6" fill="#ff5f56"/>',
        '<circle cx="40" cy="15" r="6" fill="#ffbd2e"/>',
        '<circle cx="60" cy="15" r="6" fill="#27c93f"/>',
        '<text x="%d" y="19" fill="%s" font-family="-apple-system, Segoe UI, Helvetica, Arial, sans-serif" '
        'font-size="12" text-anchor="middle">%s</text>' % (w // 2, DIM, escape(title)),
        '<g font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, \'Liberation Mono\', monospace" '
        'font-size="%d" fill="%s">' % (FONT_SIZE, FG),
    ]
    for n, line in enumerate(lines):
        if not line:
            continue
        y = PAD_TOP + LINE_H * n + FONT_SIZE
        out.append('<text x="%d" y="%d" xml:space="preserve" style="white-space:pre" '
                   'textLength="%.1f" lengthAdjust="spacingAndGlyphs">%s</text>'
                   % (PAD_X, y, len(line) * CHAR_W, "".join(colour_line(line))))
    out.append("</g>")
    out.append("</svg>")
    return "\n".join(out) + "\n"


def main(argv):
    args = list(argv[1:])
    title = "hondaecu-cli"
    if "--title" in args:
        i = args.index("--title")
        title = args[i + 1]
        del args[i:i + 2]
    if not args:
        sys.exit(__doc__)
    src = args[0]
    dst = args[1] if len(args) > 1 else os.path.splitext(src)[0] + ".svg"
    with open(src, encoding="utf-8") as f:
        lines = [l.rstrip("\r\n").expandtabs() for l in f]
    while lines and not lines[-1].strip():
        lines.pop()
    with open(dst, "w", encoding="utf-8", newline="\n") as f:
        f.write(render(lines, title))
    print("%s: %d lines" % (dst, len(lines)))


if __name__ == "__main__":
    main(sys.argv)
