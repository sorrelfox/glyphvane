---
tags: [tour]
---

# Diagrams

A code block in the `mermaid` language is drawn as a diagram. Back to
[[Welcome]].

## Flowchart

```mermaid
flowchart LR
    A[Open the vault] --> B{Found a link?}
    B -- yes --> C[Follow it]
    B -- no --> D[Search with Ctrl+Shift+F]
    C --> E[Read on]
    D --> E
```

## Sequence

```mermaid
sequenceDiagram
    participant You
    participant Glyphvane
    You->>Glyphvane: Open Welcome.md
    Glyphvane-->>You: The rendered note
    You->>Glyphvane: Click a wikilink
    Glyphvane-->>You: The linked note
```

## Pie

```mermaid
pie title Notes in this vault by kind
    "Tour notes" : 15
    "Projects" : 3
    "Other" : 2
```

Flowcharts, sequence, class, state, Gantt, pie, mind map and many other
diagram types are supported. A diagram that cannot be drawn shows its source.
