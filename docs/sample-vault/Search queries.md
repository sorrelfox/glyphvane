---
tags: [tour]
---

# Search queries

A `query` code block holds a search, and the Reading view shows its results
in place, kept up to date as the vault changes. It uses the same syntax as
the Search panel (**Ctrl+Shift+F**). Back to [[Welcome]].

## Open tasks about the tour

```query
task-todo:read
```

## Project notes that are not finished

```query
tag:#project -[status:done]
```

## Notes that mention the wind

```query
wind OR gust -file:Search
```

Click a result to open it. To change a query, edit the text of its block.
