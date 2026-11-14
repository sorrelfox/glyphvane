// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! HTML → Markdown, one element (or feature) per case.

use gv_html::{PasteOptions, Profile, html_to_markdown};

fn md(html: &str, profile: Profile) -> String {
    html_to_markdown(html, PasteOptions::new(profile))
}

fn check(cases: &[(&str, &str)], profile: Profile) {
    let mut failures = Vec::new();
    for (html, want) in cases {
        let got = md(html, profile);
        if got != *want {
            failures.push(format!("{html:?}\n  want {want:?}\n   got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn blocks() {
    check(
        &[
            ("<h1>Title</h1>", "# Title"),
            ("<h3>A <b>b</b></h3>", "### A b"),
            ("<h2>Ends with #</h2>", "## Ends with \\#"),
            ("<h6>x<br>y</h6>", "###### x y"),
            ("<p>one</p><p>two</p>", "one\n\ntwo"),
            ("<div>one<div>two</div>three</div>", "one\n\ntwo\n\nthree"),
            ("plain", "plain"),
            ("<p>  lots   of\n\t space </p>", "lots of space"),
            ("<p>a<br>b</p>", "a\\\nb"),
            ("<p>a<br><br>b</p>", "a\n\nb"),
            ("<p>a<br></p>", "a"),
            ("<p>a</p><hr><p>b</p>", "a\n\n***\n\nb"),
            ("<blockquote><p>q</p><p>r</p></blockquote>", "> q\n>\n> r"),
            (
                "<blockquote>a<blockquote>b</blockquote></blockquote>",
                "> a\n>\n> > b",
            ),
            ("<p></p><p>&nbsp;</p><div> </div>", ""),
        ],
        Profile::CommonMark,
    );
}

#[test]
fn inline_formatting() {
    check(
        &[
            ("<b>bold</b> and <i>it</i>", "**bold** and *it*"),
            ("<strong>s</strong> <em>e</em>", "**s** *e*"),
            ("<b>bold <i>both</i></b>", "**bold *both***"),
            ("<b> padded </b>x", "**padded** x"),
            ("a<b>b</b>c", "a<strong>b</strong>c"),
            ("<b>x</b><i>y</i>", "<strong>x</strong><em>y</em>"),
            ("<code>a_b</code>", "`a_b`"),
            ("<code>a`b</code>", "``a`b``"),
            ("<kbd>Ctrl</kbd>+<kbd>C</kbd>", "`Ctrl`+`C`"),
            ("<b><code>x</code></b>", "**`x`**"),
            ("<del>gone</del>", "<del>gone</del>"),
            (
                "H<sub>2</sub>O x<sup>2</sup>",
                "H<sub>2</sub>O x<sup>2</sup>",
            ),
            ("<mark>hi</mark>", "hi"),
            ("<span style=\"font-weight:700\">b</span>", "**b**"),
            ("<span style=\"font-style:italic\">i</span>", "*i*"),
            (
                "<b style=\"font-weight:normal\"><span style=\"font-weight:700\">x</span> y</b>",
                "**x** y",
            ),
            (
                "<span style=\"font-family:'Courier New'\">mono</span>",
                "`mono`",
            ),
            (
                "<span style=\"text-decoration:line-through\">s</span>",
                "<del>s</del>",
            ),
            ("<span style=\"display:none\">hidden</span>shown", "shown"),
            ("<u>under</u> <ins>ins</ins>", "under ins"),
        ],
        Profile::CommonMark,
    );
    check(
        &[
            ("<del>gone</del>", "~~gone~~"),
            ("<s>a</s>, <strike>b</strike>", "~~a~~, ~~b~~"),
            ("<s>a</s> <s>b</s>", "~~a b~~"),
            ("H<sub>2</sub>O", "H<sub>2</sub>O"),
            ("<mark>hi</mark>", "hi"),
        ],
        Profile::GfmFile,
    );
    check(
        &[("<mark>hi</mark>", "==hi=="), ("<del>x</del>", "~~x~~")],
        Profile::Obsidian,
    );
    check(
        &[
            ("H<sub>2</sub>O x<sup>2</sup>", "H~2~O x^2^"),
            ("<sup>a b</sup>", "<sup>a b</sup>"),
            ("<mark>hi</mark>", "==hi=="),
        ],
        Profile::Extended,
    );
}

#[test]
fn escaping() {
    check(
        &[
            ("*not em*", "\\*not em\\*"),
            ("snake_case and _x_", "snake_case and \\_x\\_"),
            ("1. not a list", "1\\. not a list"),
            ("# not a heading", "\\# not a heading"),
            ("- not a bullet", "\\- not a bullet"),
            ("&gt; not a quote", "\\> not a quote"),
            ("[x](y) `c`", "\\[x\\](y) \\`c\\`"),
            ("&lt;div&gt; a &lt; b", "\\<div> a < b"),
            ("AT&amp;T &amp;amp;", "AT&T \\&amp;"),
            ("a\\b \\*", "a\\b \\\\\\*"),
            ("~~x~~ $1 ==h==", "~~x~~ $1 ==h=="),
            ("<p>a<br>- b</p>", "a\\\n\\- b"),
        ],
        Profile::CommonMark,
    );
    check(
        &[("~~x~~ $1 a|b", "\\~\\~x\\~\\~ \\$1 a|b")],
        Profile::GfmFile,
    );
}

#[test]
fn links_and_images() {
    check(
        &[
            ("<a href=\"https://x.org/\">x</a>", "[x](https://x.org/)"),
            (
                "<a href=\"https://x.org/a b\" title='T \"q\"'>x</a>",
                "[x](https://x.org/a%20b \"T \\\"q\\\"\")",
            ),
            ("<a href=\"docs/a b.md\">rel</a>", "[rel](<docs/a b.md>)"),
            ("<a href=\"#sec\">frag</a>", "[frag](#sec)"),
            (
                "<a href=\"mailto:a@b.org\">mail</a>",
                "[mail](mailto:a@b.org)",
            ),
            ("<a name=\"anchor\">no href</a>", "no href"),
            ("<a href=\"https://x.org/\"> </a>after", "after"),
            (
                "<a href=\"https://x.org/\"><b>bold</b> link</a>",
                "[**bold** link](https://x.org/)",
            ),
            (
                "<a href=\"https://x.org/\">a [b]</a>",
                "[a \\[b\\]](https://x.org/)",
            ),
            (
                "<img src=\"https://x.org/i.png\" alt=\"An *image*\">",
                "![An \\*image\\*](https://x.org/i.png)",
            ),
            (
                "<img src=\"i.png\" alt=\"a\" title=\"t\">",
                "![a](i.png \"t\")",
            ),
            (
                "<a href=\"https://x.org/\"><img src=\"https://x.org/b.svg\" alt=\"badge\"></a>",
                "[![badge](https://x.org/b.svg)](https://x.org/)",
            ),
            (
                "<img src=\"data:image/gif;base64,R0lGOD\" data-src=\"https://x.org/real.png\" alt=\"lazy\">",
                "![lazy](https://x.org/real.png)",
            ),
            (
                "<img src=\"file:///C:/Temp/clip.png\" alt=\"word\">",
                "word",
            ),
            (
                "<a href=\"https://x.org/a_(b)\">w</a>",
                "[w](https://x.org/a_(b))",
            ),
            (
                "<a href=\"file:///home/a.pdf\">f</a>",
                "[f](file:///home/a.pdf)",
            ),
            ("<a href=\"C:\\Windows\\calc.exe\">c</a>", "c"),
        ],
        Profile::CommonMark,
    );
}

#[test]
fn base_url_resolution() {
    let opts = PasteOptions::new(Profile::GfmFile).with_base_url(Some("https://ex.com/docs/page"));
    let got = html_to_markdown(
        "<a href=\"../a\">a</a> <a href=\"#s\">s</a> <img src=\"/i.png\" alt=\"i\"> <a href=\"//cdn.ex.com/x\">c</a>",
        opts,
    );
    assert_eq!(
        got,
        "[a](https://ex.com/a) [s](https://ex.com/docs/page#s) ![i](https://ex.com/i.png) [c](https://cdn.ex.com/x)"
    );
    // A `file:` base is ignored.
    let opts = PasteOptions::new(Profile::GfmFile).with_base_url(Some("file://host/share/"));
    assert_eq!(
        html_to_markdown("<a href=\"x.md\">x</a>", opts),
        "[x](x.md)"
    );
}

#[test]
fn code_blocks() {
    check(
        &[
            (
                "<pre><code class=\"language-rust\">fn main() {\n    x();\n}\n</code></pre>",
                "```rust\nfn main() {\n    x();\n}\n```",
            ),
            ("<pre>a  b\n\n c</pre>", "```\na  b\n\n c\n```"),
            ("<pre>has\n```\nfence</pre>", "````\nhas\n```\nfence\n````"),
            (
                "<div class=\"highlight highlight-source-python\"><pre><span class=\"pl-k\">def</span> f(): <span>pass</span></pre></div>",
                "```python\ndef f(): pass\n```",
            ),
            ("<pre lang=\"go\">x</pre>", "```go\nx\n```"),
            ("<pre class=\"brush: js\">x<br>y</pre>", "```js\nx\ny\n```"),
            ("<pre class=\"language-x`y\">z</pre>", "```\nz\n```"),
            ("<pre>   </pre>", ""),
        ],
        Profile::CommonMark,
    );
}

#[test]
fn lists() {
    check(
        &[
            ("<ul><li>a</li><li>b</li></ul>", "- a\n- b"),
            ("<ol><li>a</li><li>b</li></ol>", "1. a\n2. b"),
            ("<ol start=\"7\"><li>a</li><li>b</li></ol>", "7. a\n8. b"),
            (
                "<ul><li>a<ul><li>b<ol><li>c</li></ol></li></ul></li></ul>",
                "- a\n  - b\n    1. c",
            ),
            (
                "<ul><li><p>one</p><p>two</p></li><li><p>three</p></li></ul>",
                "- one\n\n  two\n\n- three",
            ),
            (
                "<ul><li><p>tight</p></li><li><p>p</p></li></ul>",
                "- tight\n- p",
            ),
            (
                "<ul><li>a</li><ul><li>nested</li></ul></ul>",
                "- a\n  - nested",
            ),
            ("<ul><li>a</li></ul><ul><li>b</li></ul>", "- a\n\n* b"),
            ("<ol><li>a</li></ol><ol><li>b</li></ol>", "1. a\n\n1) b"),
            ("<ul><li></li><li>x</li></ul>", "-\n- x"),
            ("<ul><li><pre>code</pre></li></ul>", "- ```\n  code\n  ```"),
            ("<ul><li><hr></li></ul>", "- ***"),
            ("<ul><li>- dash</li></ul>", "- \\- dash"),
            (
                "<ul><li>x<ol start=\"3\"><li>y</li></ol></li></ul>",
                "- x\n\n  3. y",
            ),
            (
                "<ul><li><input type=\"checkbox\" checked> done</li></ul>",
                "- \\[x\\] done",
            ),
        ],
        Profile::CommonMark,
    );
    check(
        &[
            (
                "<ul class=\"contains-task-list\"><li class=\"task-list-item\"><input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled checked> done</li><li class=\"task-list-item\"><input type=\"checkbox\" disabled> todo</li></ul>",
                "- [x] done\n- [ ] todo",
            ),
            (
                "<ul><li>text <input type=\"checkbox\"> later</li></ul>",
                "- text later",
            ),
        ],
        Profile::GfmFile,
    );
}

#[test]
fn tables() {
    let t = "<table><thead><tr><th>A</th><th align=\"right\">B</th><th style=\"text-align:center\">C</th></tr></thead>\
             <tbody><tr><td>1</td><td><b>2</b></td><td>a|b</td></tr><tr><td colspan=\"2\">wide</td><td><code>x|y</code></td></tr></tbody></table>";
    check(
        &[
            (
                t,
                "| A | B | C |\n| --- | ---: | :---: |\n| 1 | **2** | a\\|b |\n| wide | | `x\\|y` |",
            ),
            (
                "<table><tr><td>no</td><td>header</td></tr><tr><td>x</td></tr></table>",
                "| no | header |\n| --- | --- |\n| x | |",
            ),
            (
                "<table><tr><td><p>p1</p><p>p2</p></td><td><ul><li>i</li></ul></td></tr><tr><td>a<br>b</td><td></td></tr></table>",
                "| p1<br>p2 | i |\n| --- | --- |\n| a<br>b | |",
            ),
            (
                "<table><tr><td><p>layout</p><p>cell</p></td></tr></table>",
                "layout\n\ncell",
            ),
            (
                "<table><caption>Cap</caption><tr><th>h</th></tr><tr><td>d</td></tr></table>",
                "Cap\n\n| h |\n| --- |\n| d |",
            ),
        ],
        Profile::GfmFile,
    );
    check(
        &[(
            t,
            "<table>\n<tr><th>A</th><th align=\"right\">B</th><th align=\"center\">C</th></tr>\n<tr><td>1</td><td><strong>2</strong></td><td>a|b</td></tr>\n<tr><td>wide</td><td></td><td><code>x|y</code></td></tr>\n</table>",
        )],
        Profile::CommonMark,
    );
}

#[test]
fn alerts() {
    let html = "<div class=\"markdown-alert markdown-alert-warning\"><p class=\"markdown-alert-title\"><svg><path d=\"\"></path></svg>Warning</p><p>Careful.</p></div>";
    check(&[(html, "> [!WARNING]\n> Careful.")], Profile::GfmFile);
    check(&[(html, "> [!warning]\n> Careful.")], Profile::Obsidian);
    check(
        &[(html, "> **Warning**\n>\n> Careful.")],
        Profile::CommonMark,
    );
}

#[test]
fn math() {
    let html = "<p>E <math><semantics><mrow><mi>m</mi></mrow><annotation encoding=\"application/x-tex\">mc^2</annotation></semantics></math> ok</p>";
    check(&[(html, "E $mc^2$ ok")], Profile::Obsidian);
    check(&[(html, "E `mc^2` ok")], Profile::CommonMark);
}

#[test]
fn dropped_content() {
    check(
        &[
            ("<script>alert(1)</script>text", "text"),
            ("<style>p{}</style>text", "text"),
            ("<!-- comment -->text", "text"),
            ("<template><p>t</p></template>text", "text"),
            ("<p>text</p><noscript>ns</noscript>", "text"),
            ("<iframe src=\"https://x.org\"></iframe>text", "text"),
            ("<svg><text>svg text</text></svg>text", "text"),
            (
                "<button>Click</button><select><option>o</option></select>text",
                "text",
            ),
            ("<p hidden>h</p>text", "text"),
            ("<object data=\"x\">fallback</object>text", "text"),
            ("<video src=\"v.mp4\">no video</video>text", "text"),
            ("<p><!--[if !supportLists]-->no endif</p>", "no endif"),
        ],
        Profile::CommonMark,
    );
}
