---
tags: [tour]
---

# Bases

A base is a saved view of notes, built from their properties. The file
`Projects.base` lists the notes in the `Projects` folder; open it on its
own, or see it embedded below. Back to [[Welcome]].

## An embedded base

![[Projects.base]]

Press **]** and **[** to switch between its views: a table, a table grouped
by status, cards with a cover image, and a list of the projects still open.
One view can be embedded on its own by naming it:

![[Projects.base#Still open]]

## A base inside a note

A `base` code block holds a base's settings right in the note. This one
lists the notes tagged `#tour` that link to [[Welcome]]:

```base
filters:
  and:
    - file.hasTag("tour")
    - file.hasLink(link("Welcome"))
views:
  - type: table
    name: Tour notes
    order:
      - file.name
      - file.mtime
    sort:
      - property: file.name
        direction: ASC
```

## Things to change

Edit the `status`, `priority` or `due` property of a project, such as
[[Garden beds]], and watch the base follow. The [[Tags and properties]] note
explains properties.
