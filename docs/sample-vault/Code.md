---
tags: [tour]
---

# Code

Inline code sits in backticks: `cargo build`. A fenced block with a language
name after the opening fence is highlighted. Back to [[Welcome]].

```rust
/// Where the wind comes from, in degrees.
fn heading(north: f32, east: f32) -> f32 {
    let degrees = east.atan2(north).to_degrees();
    if degrees < 0.0 { degrees + 360.0 } else { degrees }
}
```

```python
def compass_point(degrees: float) -> str:
    points = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"]
    return points[round(degrees / 45) % 8]
```

```sh
# Count the notes in this vault
find . -name '*.md' | wc -l
```

```toml
# .glyphvane.toml at the root of this vault
profile = "obsidian"
root = true
```

```json
{ "station": "Hilltop", "wind": { "speed": 14, "from": "SW" } }
```

A block without a language is shown as plain text:

```
No colours here.
```

> [!tip]
> The colours follow the app theme, or the **Code theme** setting. In the
> editor, the code is highlighted as you type.
