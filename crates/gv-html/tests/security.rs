// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Pasted HTML is untrusted: script URLs, `data:` URLs, network paths and
//! dangerous OS handlers never become links or images, and nothing
//! executable survives. Every vector is converted for every profile, and
//! the Markdown is rendered back to HTML (CommonMark emitter, raw HTML
//! passed through) to check what a renderer would see.

use gv_html::{PasteOptions, Profile, html_to_markdown};

const PROFILES: &[Profile] = &[
    Profile::CommonMark,
    Profile::GfmFile,
    Profile::GfmComment,
    Profile::Obsidian,
    Profile::Extended,
];

/// (HTML, the visible text that must remain).
const VECTORS: &[(&str, &str)] = &[
    ("<a href=\"javascript:alert(1)\">click</a>", "click"),
    ("<a href=\"JaVaScRiPt:alert(1)\">click</a>", "click"),
    ("<a href=\"jav&#x09;ascript:alert(1)\">click</a>", "click"),
    ("<a href=\"jav&#x0A;ascript:alert(1)\">click</a>", "click"),
    ("<a href=\"&#106;avascript:alert(1)\">click</a>", "click"),
    (
        "<a href=\"&#x6A;&#x61;vascript&colon;alert(1)\">click</a>",
        "click",
    ),
    ("<a href=\" &#14; javascript:alert(1)\">click</a>", "click"),
    ("<a href=\"java script:alert(1)\">click</a>", "click"),
    ("<a href=\"vbscript:msgbox(1)\">click</a>", "click"),
    ("<a href=\"VBScript:msgbox(1)\">click</a>", "click"),
    ("<a href=\"livescript:x\">click</a>", "click"),
    (
        "<a href=\"data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==\">click</a>",
        "click",
    ),
    (
        "<a href=\"data:image/png;base64,iVBORw0KGgo=\">click</a>",
        "click",
    ),
    (
        "<img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"pixel\">",
        "pixel",
    ),
    (
        "<img src=\"data:image/svg+xml,&lt;svg onload=alert(1)&gt;\" alt=\"svg\">",
        "svg",
    ),
    ("<img src=\"javascript:alert(1)\" alt=\"img\">", "img"),
    ("<a href=\"\\\\attacker\\share\">unc</a>", "unc"),
    ("<a href=\"\\\\?\\UNC\\attacker\\share\">unc</a>", "unc"),
    (
        "<img src=\"\\\\attacker\\share\\x.png\" alt=\"unc\">",
        "unc",
    ),
    ("<img src=\"//\\attacker/x.png\" alt=\"unc\">", "unc"),
    ("<img src=\"\\/attacker/x.png\" alt=\"unc\">", "unc"),
    ("<img src=\"///attacker/x.png\" alt=\"unc\">", "unc"),
    ("<img src=\"file://attacker/share/x.png\" alt=\"fh\">", "fh"),
    (
        "<img src=\"file:////attacker/share/x.png\" alt=\"fh\">",
        "fh",
    ),
    ("<img src=\"file:\\\\attacker\\x.png\" alt=\"fh\">", "fh"),
    ("<img src=\"%5C%5Cattacker/x.png\" alt=\"enc\">", "enc"),
    ("<img src=\"smb://attacker/x.png\" alt=\"smb\">", "smb"),
    ("<a href=\"file://attacker/share/doc\">fh</a>", "fh"),
    ("<a href=\"smb://attacker/share\">smb</a>", "smb"),
    ("<a href=\"ms-msdt:/id PCWDiagnostic\">msdt</a>", "msdt"),
    ("<a href=\"search-ms:query=x\">search</a>", "search"),
    ("<a href=\"ms-officecmd:x\">office</a>", "office"),
    ("<a href=\"shell:startup\">shell</a>", "shell"),
    ("<img src=\"file:///etc/passwd\" alt=\"local\">", "local"),
    ("<script>alert(1)</script>safe", "safe"),
    ("<SCRIPT SRC=//x.example/x.js></SCRIPT>safe", "safe"),
    ("<img src=x onerror=alert(1) alt=\"e\">", "e"),
    ("<svg onload=alert(1)><text>t</text></svg>safe", "safe"),
    ("<iframe src=\"javascript:alert(1)\"></iframe>safe", "safe"),
    (
        "<iframe srcdoc=\"&lt;script&gt;alert(1)&lt;/script&gt;\"></iframe>safe",
        "safe",
    ),
    (
        "<object data=\"javascript:alert(1)\">fb</object>safe",
        "safe",
    ),
    ("<embed src=\"javascript:alert(1)\">safe", "safe"),
    (
        "<form action=\"javascript:alert(1)\"><button>go</button></form>safe",
        "safe",
    ),
    (
        "<math><mtext><table><mglyph><style><img src=x onerror=alert(1)>",
        "",
    ),
    (
        "<a href=\"https://ok.example/\" onclick=\"alert(1)\">ok</a>",
        "ok",
    ),
    (
        "<div style=\"background:url(javascript:alert(1))\">styled</div>",
        "styled",
    ),
    ("<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>", "alert(1)"),
    ("<p>&lt;img src=x onerror=alert(1)&gt;</p>", "onerror"),
    (
        "<p>[x](javascript:alert(1)) &lt;javascript:alert(1)&gt;</p>",
        "x",
    ),
    (
        "<a href=\"https://ok.example/\" title='a\") [y](javascript:alert(1)'>t</a>",
        "t",
    ),
    (
        "<img src=\"https://ok.example/i.png\" alt=\"a](javascript:alert(1))\">",
        "",
    ),
    (
        "<pre class=\"language-x&quot;&gt;&lt;script&gt;\">code</pre>",
        "code",
    ),
    (
        "<table><tr><td><a href=\"javascript:alert(1)\">cell</a></td><td>2</td></tr></table>",
        "cell",
    ),
    (
        "<meta http-equiv=\"refresh\" content=\"0;url=javascript:alert(1)\">safe",
        "safe",
    ),
    (
        "<base href=\"javascript:alert(1)//\"><a href=\"x\">rel</a>",
        "rel",
    ),
    (
        "<link rel=stylesheet href=\"javascript:alert(1)\">safe",
        "safe",
    ),
    ("<style>@import 'javascript:alert(1)';</style>safe", "safe"),
];

/// The values of `href` and `src` attributes in rendered HTML.
fn url_attrs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["href=\"", "src=\""] {
        let mut rest = html;
        while let Some(i) = rest.find(key) {
            let v = &rest[i + key.len()..];
            let end = v.find('"').unwrap_or(v.len());
            out.push(v[..end].to_owned());
            rest = &v[end..];
        }
    }
    out
}

fn dangerous_url(u: &str) -> bool {
    let l: String = u
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    let l = l.replace("&amp;", "&");
    [
        "javascript:",
        "vbscript:",
        "livescript:",
        "data:",
        "file:",
        "smb:",
        "ms-",
        "search",
        "shell:",
        "\\\\",
        "//",
        "%5c",
    ]
    .iter()
    .any(|p| l.starts_with(p) || l.contains("%5c%5c"))
        || l.contains('\\')
}

fn check_vector(html: &str, visible: &str, profile: Profile) -> Result<(), String> {
    check_markdown(&html_to_markdown(html, PasteOptions::new(profile)), visible)
}

fn check_markdown(md: &str, visible: &str) -> Result<(), String> {
    let tree = gv_syntax::parse(md);
    let rendered = gv_syntax::to_commonmark_html(&tree, md);
    let lower = rendered.to_ascii_lowercase();
    for bad in [
        "<script", "<iframe", "<object", "<embed", "<svg", "<style", "<form", "<meta", "<base",
        "<link", "<math",
    ] {
        if lower.contains(bad) {
            return Err(format!("rendered contains {bad}: {rendered}"));
        }
    }
    // No event-handler attributes on any rendered tag.
    let mut rest = lower.as_str();
    while let Some(lt) = rest.find('<') {
        let tag_end = rest[lt..].find('>').map_or(rest.len(), |i| lt + i);
        let tag = &rest[lt..tag_end];
        if tag
            .split_ascii_whitespace()
            .skip(1)
            .any(|a| a.starts_with("on"))
        {
            return Err(format!("event handler in {tag}: {rendered}"));
        }
        rest = &rest[tag_end.min(rest.len())..];
        if rest.is_empty() {
            break;
        }
        rest = &rest[1..];
    }
    for u in url_attrs(&rendered) {
        if dangerous_url(&u) {
            return Err(format!("dangerous URL {u:?} in {rendered}"));
        }
    }
    if !md.contains(visible) {
        return Err(format!("lost the text {visible:?}: {md:?}"));
    }
    Ok(())
}

#[test]
fn vectors_are_neutralized() {
    let mut failures = Vec::new();
    for (html, visible) in VECTORS {
        for &p in PROFILES {
            if let Err(e) = check_vector(html, visible, p) {
                failures.push(format!("[{}] {html}\n  {e}", p.id()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn checker_catches_unsafe_markdown() {
    for md in [
        "[x](javascript:alert(1))",
        "![x](//\\host/x.png)",
        "<script>alert(1)</script>",
        "<img src=x onerror=alert(1)>",
        "[x](<\\\\host\\share>)",
        "![x](data:image/png;base64,AA)",
    ] {
        assert!(check_markdown(md, "").is_err(), "{md}");
    }
    assert!(check_markdown("[x](https://ok.example/)", "x").is_ok());
}

#[test]
fn unsafe_links_become_plain_text() {
    let p = PasteOptions::new(Profile::GfmFile);
    assert_eq!(
        html_to_markdown("<a href=\"javascript:alert(1)\">click</a>", p),
        "click"
    );
    assert_eq!(
        html_to_markdown("<a href=\"\\\\host\\share\">unc</a>", p),
        "unc"
    );
    assert_eq!(
        html_to_markdown("<img src=\"file://host/x.png\" alt=\"pic\">", p),
        "pic"
    );
    assert_eq!(
        html_to_markdown("<img src=\"data:image/png;base64,AA\">", p),
        ""
    );
    // Allowed schemes stay links (following them may still ask first).
    assert_eq!(
        html_to_markdown("<a href=\"tel:+123\">call</a>", p),
        "[call](tel:+123)"
    );
}

#[test]
fn hostile_base_url_is_ignored() {
    for base in [
        "javascript:alert(1)//",
        "file://attacker/share/",
        "\\\\attacker\\share\\",
        "data:text/html,x",
    ] {
        let opts = PasteOptions::new(Profile::GfmFile).with_base_url(Some(base));
        let md = html_to_markdown(
            "<a href=\"x.md\">rel</a> <img src=\"i.png\" alt=\"i\">",
            opts,
        );
        assert_eq!(md, "[rel](x.md) ![i](i.png)", "{base}");
    }
}
