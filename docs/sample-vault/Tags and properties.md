---
title: Tags and properties
tags:
  - tour
  - tour/metadata
aliases:
  - Properties
  - Front matter
status: done
rating: 4
created: 2026-11-19
reviewed: true
related: "[[Bases]]"
---

# Tags and properties

The block between the two `---` lines at the top of this note is its **front
matter**. Glyphvane shows it as a list of properties above the text. Back to
[[Welcome]].

## Properties

Each line is a name and a value: text, a number, a date, `true` or `false`,
a list, or a link such as the `related` property above. Links in properties
count as links, so this note shows up in the backlinks of [[Bases]].

Properties are what [[Bases]] filter and sort on, and what search finds with
brackets: try `[status:done]` or `[rating:>3]` in the Search panel.

## Tags

Tags can live in the front matter (`tags:`) or anywhere in the text, like
#tour and #weather/wind. A slash nests a tag under another, so
#weather/rain and #weather/wind both appear under `#weather` in the Tags
panel.

Search for `tag:#weather` to find both nested tags. A `#` inside code, such
as `#not-a-tag`, is not a tag.

## Aliases

The `aliases` property gives a note other names. Press **Ctrl+O** and type
"front matter": this note is offered under that name too.

## Comments and highlights

You can mark text as ==highlighted==. Text between two pairs of percent
signs is a comment, only visible in the editor:
%%You found the hidden comment.%%

%%
A comment can also span several lines,
when the percent signs stand on lines of their own.
%%
