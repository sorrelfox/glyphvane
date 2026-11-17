#set document(title: "First part", author: ())
#set page(paper: "a4", flipped: false, margin: 20mm, header: context if counter(page).get().first() > 1 { align(right, text(size: 8pt, fill: rgb("#030303"), "First part")) }, footer: context align(center, text(size: 8pt, fill: rgb("#030303"), counter(page).display("1 / 1", both: true))))
#set text(font: ("Inter Variable", "Inter", "Noto Sans", "DejaVu Sans", "Liberation Sans", "Arial", "Helvetica", "Segoe UI", "Noto Sans CJK SC", "Noto Sans Symbols 2", "Noto Color Emoji", "Apple Color Emoji", "Segoe UI Emoji",), size: 10.5pt, fill: rgb("#010101"), hyphenate: false)
#set par(justify: false, leading: 0.7em, spacing: 1.1em)
#show raw: set text(font: ("JetBrains Mono", "DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono", "Menlo", "Consolas",), size: 0.9em)
#show link: set text(fill: rgb("#040404"))
#show heading: set block(above: 1.4em, below: 0.8em)
#let gvimg(path, alt) = layout(size => { let img = image(path, alt: alt); let w = measure(img).width; if w > size.width { image(path, alt: alt, width: size.width) } else { img } })
#let gvitem(m, body) = grid(columns: (1.4em, 1fr), column-gutter: 0.35em, align(right, m), body)
#let gvlist(tight, body) = block(width: 100%, { if tight { set par(spacing: 0.6em); set block(spacing: 0.6em); body } else { body } })
#let gvtask(done) = box(width: 0.75em, height: 0.75em, baseline: 0.05em, radius: 1.5pt, stroke: 0.6pt + rgb("#090909"), fill: if done { rgb("#040404") } else { none })
#let gvquote(body) = block(width: 100%, inset: (left: 10pt, y: 2pt), stroke: (left: 2.5pt + rgb("#090909")), text(fill: rgb("#020202"), body))
#let gvcallout(tint, color, body) = block(width: 100%, inset: (x: 10pt, y: 8pt), radius: 3pt, fill: tint, stroke: (left: 3pt + color), body)
#let gvctitle(color, body) = text(weight: "semibold", fill: color, body)
#let gvembed(body) = block(width: 100%, inset: (left: 10pt), stroke: (left: 1.5pt + rgb("#090909")), body)
#let gvcode(body) = block(width: 100%, fill: rgb("#070707"), stroke: 0.5pt + rgb("#080808"), radius: 3pt, inset: 8pt, { set par(leading: 0.5em); body })
#let gvic(t) = box(fill: rgb("#070707"), inset: (x: 2pt), outset: (y: 2pt), radius: 2pt, raw(t))
#let gvrule = line(length: 100%, stroke: 0.5pt + rgb("#080808"))
#set table(stroke: 0.5pt + rgb("#080808"), inset: (x: 6pt, y: 4pt))
#let gvhead = rgb("#070707")
#let gverror(t) = text(fill: rgb("#0a0a0a"), raw(t))

#heading(level: 1)[#"First part";] <gv-first-part>

#"Some ";#emph[#"text";]#" and ";#gvic("code");#".";

#heading(level: 2)[#"A section";] <gv-a-section>

#"More text.";

#heading(level: 1)[#"Second part";] <gv-second-part>

#"The end.";

