// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! HTML export: the golden document, sanitizing, link rewriting, images,
//! and rich copy.
//!
//! The golden file is `tests/fixtures/all.html`; run with
//! `GV_BLESS=1` to write it again after an intended change.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gv_export::{
    DiagramRenderer, ExportOptions, ExportTheme, ImageMode, Limits, RemoteImages, WikiResolver,
    export_html, rich_copy, wikilink_href,
};
use gv_model::DocModel;
use gv_profile::ProfileId;
use gv_render_tree::{AllPending, NoImageSizes, RenderOptions, RenderTree, Sources};
use gv_syntax::ParseOptions;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tree(src: &str, id: ProfileId) -> RenderTree {
    let syntax = gv_syntax::parse_with(src, ParseOptions::builtin(id));
    let model = DocModel::build(&syntax, src);
    RenderTree::build(
        &syntax,
        &model,
        src,
        RenderOptions::builtin(id),
        Sources {
            links: &AllPending,
            images: &NoImageSizes,
        },
    )
}

/// A theme with made-up colours: token `i` is grey `i`.
fn theme() -> ExportTheme {
    let mut i = 0u8;
    ExportTheme::from_fn(|_| {
        i += 1;
        Some([i, i, i, 255])
    })
}

struct FakeDiagrams;

impl DiagramRenderer for FakeDiagrams {
    fn svg(&self, source: &str) -> Option<String> {
        (!source.contains("bad")).then(|| {
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" data-len=\"{}\"></svg>",
                source.len()
            )
        })
    }
}

fn options(doc: &Path) -> ExportOptions {
    let mut opts = ExportOptions::new(Some(doc.to_path_buf()), theme());
    opts.diagrams = Some(Arc::new(FakeDiagrams));
    opts
}

fn export(src: &str, id: ProfileId) -> String {
    let doc = fixtures().join("doc.md");
    export_html(&tree(src, id), &options(&doc)).html
}

/// Exports `fixtures/NAME.md` in profile `id` and compares it with
/// `fixtures/NAME.html`.
fn golden(name: &str, id: ProfileId) -> gv_export::Export {
    let path = fixtures().join(format!("{name}.md"));
    let src = std::fs::read_to_string(&path).expect("fixture");
    let out = export_html(&tree(&src, id), &options(&path));
    let golden = fixtures().join(format!("{name}.html"));
    if std::env::var_os("GV_BLESS").is_some() {
        std::fs::write(&golden, &out.html).expect("write golden");
    }
    let expected = std::fs::read_to_string(&golden).expect("golden file (GV_BLESS=1 writes it)");
    assert!(
        out.html == expected,
        "export differs from {}; run with GV_BLESS=1 and review the diff",
        golden.display()
    );
    // The same input gives the same bytes (no timestamps, no placeholders).
    let again = export_html(&tree(&src, id), &options(&path));
    assert_eq!(again.html, out.html);
    assert!(!out.html.contains('\u{E000}') && !out.html.contains('\u{FFFD}'));
    out
}

#[test]
fn golden_document() {
    let out = golden("all", ProfileId::Extended);
    let notes: Vec<_> = out
        .notes
        .iter()
        .map(|n| (n.src.as_str(), n.reason.as_str()))
        .collect();
    assert_eq!(
        notes,
        [
            ("img/missing.png", "file not found"),
            ("../outside.png", "file not found"),
        ]
    );
}

#[test]
fn golden_obsidian_document() {
    let out = golden("obsidian", ProfileId::Obsidian);
    let html = &out.html;
    assert!(
        !html.contains("hidden comment"),
        "comments are not exported"
    );
    assert!(html.contains("<mark>highlight</mark>"), "{html}");
}

#[test]
fn every_construct_is_exported() {
    let path = fixtures().join("all.md");
    let src = std::fs::read_to_string(&path).expect("fixture");
    let html = export_html(&tree(&src, ProfileId::Extended), &options(&path)).html;
    for needle in [
        "<!DOCTYPE html>",
        "<title>Export fixture</title>",
        "Content-Security-Policy\" content=\"default-src 'none'; img-src data: https:; style-src 'unsafe-inline'; font-src data:\"",
        "--text-primary: #010101;",
        "<h1 id=\"user-content-export-fixture\">Export fixture</h1>",
        "<em>emphasis</em>",
        "<strong>strong</strong>",
        "<del>struck</del>",
        "<code>code</code>",
        "<a href=\"https://example.com\" title=\"Example\" rel=\"noopener noreferrer\">link</a>",
        "<a href=\"notes/other%20note.html#user-content-Section\" rel=\"noopener noreferrer\">relative note</a>",
        "<a href=\"#user-content-tables\" rel=\"noopener noreferrer\">anchor</a>",
        "<a href=\"mailto:me@example.com\" rel=\"noopener noreferrer\">",
        "hard<br>",
        "<ol start=\"3\">",
        "<li class=\"gv-task\"><input type=\"checkbox\" disabled> open task",
        "<li class=\"gv-task\"><input type=\"checkbox\" disabled checked> done task",
        "text.</p>\n<blockquote>\n<p>A nested quote.</p>\n</blockquote>\n</blockquote>",
        "<div class=\"gv-callout gv-callout-note\">\n<p class=\"gv-callout-title\">Note</p>",
        "<div class=\"gv-callout gv-callout-warning\">",
        "<pre class=\"gv-code\"><code><span class=\"hl-",
        "&lt;b&gt;",
        "indented &lt;code&gt;",
        "<span class=\"gv-math\"><svg xmlns=\"http://www.w3.org/2000/svg\" class=\"gv-math-svg\"",
        "<div class=\"gv-math-block\"><svg",
        "<div class=\"gv-diagram\"><svg xmlns=\"http://www.w3.org/2000/svg\" data-len=\"15\"></svg></div>",
        "<th align=\"left\">Left</th><th align=\"center\">Center</th><th align=\"right\">Right</th>",
        "<td align=\"center\"><strong>b</strong></td>",
        "<sup class=\"gv-fnref\"><a href=\"#user-content-fn-1\" id=\"user-content-fnref-1-1\" rel=\"noopener noreferrer\">1</a></sup>",
        "id=\"user-content-fnref-1-2\"",
        "<section class=\"gv-footnotes\">",
        "<li id=\"user-content-fn-1\">",
        "<a href=\"#user-content-fnref-1-2\" class=\"gv-backref\"",
        "<img src=\"data:image/png;base64,iVBORw0KGgo",
        "<img src=\"https://example.com/x.png\" alt=\"remote\" title=\"Remote\">",
        "<img src=\"img/missing.png\" alt=\"missing\">",
        "<div align=\"center\" style=\"color: red\"><b>bold</b> </div>",
        "<details>\n<summary>More</summary>",
        "<table class=\"gv-properties\">",
        "<th>tags</th><td>alpha, beta</td>",
        "<a id=\"user-content-top\" href=\"#user-content-footnotes\" rel=\"noopener noreferrer\">ok</a>",
        "<hr>",
    ] {
        assert!(html.contains(needle), "missing {needle:?} in\n{html}");
    }
    assert_eq!(html.matches("data:image/png;base64").count(), 1);
    for absent in ["<script", "onclick", "javascript:", "\\\\host", "position"] {
        assert!(!html.contains(absent), "{absent:?} survived");
    }
}

/// Classic vectors (OWASP, DOMPurify, the M0 spike's network paths): none
/// may leave script, handlers, frames or network references in the
/// export.
#[test]
fn sanitizer_removes_attack_vectors() {
    let vectors = [
        "<script>alert(1)</script>",
        "<SCRIPT SRC=//evil/x.js></SCRIPT>",
        "<img src=x onerror=alert(1)>",
        "<img src=\"javascript:alert(1)\">",
        "<img src=\"jav&#x09;ascript:alert(1)\">",
        "<img src=\"\\\\attacker\\share\\x.png\">",
        "<img src=\"file://host/share/x.png\">",
        "<img src=\"file:////host/share/x.png\">",
        "<img src=\"%5C%5Chost/x.png\">",
        "<img src=\"/\\host/x.png\">",
        "<img srcset=\"\\\\host\\a.png 1x, https://ok.example/b.png 2x\">",
        "<video poster=\"javascript:alert(1)\" src=\"\\\\host\\v.mp4\"></video>",
        "<a href=\"JaVaScRiPt:alert(1)\">x</a>",
        "<a href=\"vbscript:msgbox(1)\">x</a>",
        "<a href=\"data:text/html;base64,PHNjcmlwdD4=\">x</a>",
        "<a href=\"ms-msdt:/id\">x</a>",
        "<a href=\"\\\\host\\share\">x</a>",
        "<a href=\"smb://host/share\">x</a>",
        "<iframe src=\"https://evil.example\"></iframe>",
        "<iframe srcdoc=\"<script>alert(1)</script>\"></iframe>",
        "<object data=\"x.swf\"></object><embed src=\"x.swf\">",
        "<svg onload=alert(1)><script>alert(1)</script></svg>",
        "<svg><a xlink:href=\"javascript:alert(1)\"><text>x</text></a></svg>",
        "<math><mtext><table><mglyph><style><img src=x onerror=alert(1)>",
        "<form action=\"javascript:alert(1)\"><button formaction=\"javascript:alert(1)\">go</button></form>",
        "<input type=\"text\" value=\"x\" onfocus=\"alert(1)\" autofocus>",
        "<div style=\"background:url(javascript:alert(1))\">x</div>",
        "<div style=\"width: expression(alert(1))\">x</div>",
        "<style>@import 'https://evil.example/x.css';</style>",
        "<link rel=\"stylesheet\" href=\"https://evil.example/x.css\">",
        "<meta http-equiv=\"refresh\" content=\"0;url=javascript:alert(1)\">",
        "<base href=\"javascript:alert(1)//\">",
        "<details open ontoggle=alert(1)>",
        "<a href=\"#x\" id=\"location\">clobber</a>",
        "<!-- <img src=x onerror=alert(1)> -->",
        "<noscript><p title=\"</noscript><img src=x onerror=alert(1)>\">",
        "<template><script>alert(1)</script></template>",
        "<p><b>split</p></b>",
    ];
    for v in vectors {
        let src = format!("Before\n\n{v}\n\nAfter\n");
        let html = export(&src, ProfileId::Extended);
        let body = &html[html.find("<body>").expect("body")..];
        for bad in [
            "<script",
            "onerror",
            "onload",
            "onfocus",
            "ontoggle",
            "javascript:",
            "vbscript:",
            "\\\\",
            "host/",
            "<iframe",
            "srcdoc",
            "<object",
            "<embed",
            "<svg",
            "<style",
            "<link",
            "<meta",
            "<base",
            "<form",
            "formaction",
            "<input",
            "url(",
            "expression",
            "ms-msdt",
            "smb:",
            "data:text",
            "<!--",
            "<template",
        ] {
            assert!(
                !body.to_ascii_lowercase().contains(bad),
                "{bad:?} survived {v:?}:\n{body}"
            );
        }
        assert!(body.contains("After"), "{v:?} swallowed the rest:\n{body}");
    }
    // A clobbering id is prefixed; https candidates of a srcset stay.
    let html = export(
        "<a href=\"#x\" id=\"location\">c</a>\n",
        ProfileId::Extended,
    );
    assert!(html.contains("id=\"user-content-location\""));
    let html = export(
        "<img srcset=\"\\\\host\\a.png 1x, https://ok.example/b.png 2x\">\n",
        ProfileId::Extended,
    );
    assert!(
        html.contains("srcset=\"https://ok.example/b.png 2x\""),
        "{html}"
    );
}

#[test]
fn github_profiles_drop_style() {
    let src = "<span style=\"color: red\">red</span>\n";
    let mut opts = options(&fixtures().join("doc.md"));
    opts.strip_style = true;
    let html = export_html(&tree(src, ProfileId::GfmFile), &opts).html;
    assert!(html.contains("<span>red</span>"), "{html}");
    let html = export(src, ProfileId::Extended);
    assert!(
        html.contains("<span style=\"color: red\">red</span>"),
        "{html}"
    );
}

#[test]
fn unresolved_links_are_plain_text() {
    use gv_render_tree::{LinkStatus, LinkStatusSource, LinkTarget};
    struct Missing;
    impl LinkStatusSource for Missing {
        fn status(&self, link: &LinkTarget<'_>) -> LinkStatus {
            if link.destination.contains("gone") {
                LinkStatus::Unresolved
            } else {
                LinkStatus::Exact
            }
        }
    }
    let src = "[here](a.md) and [gone](gone.md)\n";
    let id = ProfileId::Extended;
    let syntax = gv_syntax::parse_with(src, ParseOptions::builtin(id));
    let model = DocModel::build(&syntax, src);
    let t = RenderTree::build(
        &syntax,
        &model,
        src,
        RenderOptions::builtin(id),
        Sources {
            links: &Missing,
            images: &NoImageSizes,
        },
    );
    let html = export_html(&t, &options(&fixtures().join("doc.md"))).html;
    assert!(
        html.contains("<a href=\"a.html\" rel=\"noopener noreferrer\">here</a>"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"gv-unresolved\">gone</span>"),
        "{html}"
    );
}

#[test]
fn wikilinks_point_to_the_exported_note() {
    struct Vault(PathBuf);
    impl WikiResolver for Vault {
        fn resolve(&self, target: &str) -> Option<PathBuf> {
            let path = self.0.join("sub").join(format!("{target}.md"));
            path.exists().then_some(path)
        }
    }
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("sub")).expect("mkdir");
    std::fs::create_dir_all(dir.path().join("notes")).expect("mkdir");
    std::fs::write(dir.path().join("sub/Other note.md"), "# Other\n").expect("write");
    let mut opts = ExportOptions::new(Some(dir.path().join("notes/doc.md")), theme());
    opts.wiki = Some(Arc::new(Vault(dir.path().to_path_buf())));
    let slug = |s: &str| s.to_lowercase().replace(' ', "-");
    assert_eq!(
        wikilink_href("Other note", &opts, slug).as_deref(),
        Some("../sub/Other%20note.html")
    );
    assert_eq!(
        wikilink_href("Other note#A Heading", &opts, slug).as_deref(),
        Some("../sub/Other%20note.html#user-content-a-heading")
    );
    assert_eq!(
        wikilink_href("Other note#^block", &opts, slug).as_deref(),
        Some("../sub/Other%20note.html")
    );
    assert_eq!(
        wikilink_href("#Local", &opts, slug).as_deref(),
        Some("#user-content-local")
    );
    assert_eq!(wikilink_href("Missing", &opts, slug), None);
}

#[test]
fn wikilinks_in_a_document_are_rewritten() {
    use gv_render_tree::{LinkStatus, LinkStatusSource, LinkTarget};
    struct Statuses;
    impl LinkStatusSource for Statuses {
        fn status(&self, link: &LinkTarget<'_>) -> LinkStatus {
            if link.destination.starts_with("Missing") {
                LinkStatus::Unresolved
            } else {
                LinkStatus::Exact
            }
        }
    }
    struct Vault(PathBuf);
    impl WikiResolver for Vault {
        fn resolve(&self, target: &str) -> Option<PathBuf> {
            (target == "Other note").then(|| self.0.join("sub/Other note.md"))
        }
    }
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("sub")).expect("mkdir");
    std::fs::write(dir.path().join("sub/Other note.md"), "# A Heading\n").expect("write");
    let src = "See [[Other note]], [[Other note#A Heading|the heading]], [[#Local]] and [[Missing]].\n\n# Local\n";
    let id = ProfileId::Obsidian;
    let syntax = gv_syntax::parse_with(src, ParseOptions::builtin(id));
    let model = DocModel::build(&syntax, src);
    let t = RenderTree::build(
        &syntax,
        &model,
        src,
        RenderOptions::builtin(id),
        Sources {
            links: &Statuses,
            images: &NoImageSizes,
        },
    );
    let mut opts = ExportOptions::new(Some(dir.path().join("doc.md")), theme());
    opts.wiki = Some(Arc::new(Vault(dir.path().to_path_buf())));
    let html = export_html(&t, &opts).html;
    let slug = t.options().slugs.slug("A Heading");
    for needle in [
        "<a href=\"sub/Other%20note.html\" rel=\"noopener noreferrer\">Other note</a>".to_owned(),
        format!(
            "<a href=\"sub/Other%20note.html#user-content-{slug}\" rel=\"noopener noreferrer\">the heading</a>"
        ),
        format!(
            "<a href=\"#user-content-{}\" rel=\"noopener noreferrer\">",
            t.options().slugs.slug("Local")
        ),
        "<span class=\"gv-unresolved\">Missing</span>".to_owned(),
    ] {
        assert!(html.contains(&needle), "missing {needle:?} in\n{html}");
    }
    // Without a vault, a wikilink points to the note beside the document.
    opts.wiki = None;
    let html = export_html(&t, &opts).html;
    assert!(
        html.contains("<a href=\"Other%20note.html\" rel=\"noopener noreferrer\">Other note</a>"),
        "{html}"
    );
}

fn image_dir(sizes: &[(&str, usize)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let png = std::fs::read(fixtures().join("img/dot.png")).expect("png");
    for (name, size) in sizes {
        let mut bytes = png.clone();
        bytes.resize(*size, 0);
        std::fs::write(dir.path().join(name), bytes).expect("write");
    }
    dir
}

#[test]
fn image_embedding_is_capped() {
    let dir = image_dir(&[
        ("a.png", 100),
        ("b.png", 100),
        ("big.png", 5000),
        ("c.png", 100),
    ]);
    std::fs::write(dir.path().join("fake.png"), b"<html>not an image").expect("write");
    let src =
        "![a](a.png) ![b](b.png) ![big](big.png) ![c](c.png) ![a again](a.png) ![fake](fake.png)\n";
    let mut opts = options(&dir.path().join("doc.md"));
    opts.limits = Limits {
        image_bytes: 1000,
        total_bytes: 250,
    };
    let out = export_html(&tree(src, ProfileId::Extended), &opts);
    assert_eq!(
        out.html.matches("data:image/png;base64,").count(),
        3,
        "a, b and a again"
    );
    assert!(out.html.contains("<img src=\"big.png\""));
    assert!(out.html.contains("<img src=\"c.png\""));
    assert!(out.html.contains("<img src=\"fake.png\""));
    let notes: Vec<_> = out
        .notes
        .iter()
        .map(|n| (n.src.as_str(), n.reason.as_str()))
        .collect();
    assert_eq!(
        notes,
        [
            ("big.png", "larger than the image cap"),
            ("c.png", "the export's image cap is reached"),
            ("fake.png", "not a known image format"),
        ]
    );
}

#[test]
fn images_outside_the_vault_are_dropped() {
    let outer = image_dir(&[("secret.png", 100)]);
    std::fs::create_dir_all(outer.path().join("vault")).expect("mkdir");
    let mut opts = options(&outer.path().join("vault/doc.md"));
    opts.root = Some(outer.path().join("vault"));
    let out = export_html(&tree("![s](../secret.png)\n", ProfileId::Extended), &opts);
    assert!(
        !out.html.contains("secret.png") && !out.html.contains("data:image"),
        "{}",
        out.html
    );
    assert_eq!(out.notes[0].reason, "outside the vault");
}

#[test]
fn images_can_go_to_an_assets_folder() {
    let dir = image_dir(&[("a b.png", 100)]);
    let mut opts = options(&dir.path().join("doc.md"));
    opts.images = ImageMode::Assets;
    let out = export_html(
        &tree("![x](<a b.png>) ![y](a%20b.png)\n", ProfileId::Extended),
        &opts,
    );
    assert!(
        out.html
            .contains("<img src=\"assets/1-a-b.png\" alt=\"x\">"),
        "{}",
        out.html
    );
    assert!(
        out.html
            .contains("<img src=\"assets/1-a-b.png\" alt=\"y\">")
    );
    assert_eq!(out.assets.len(), 1);
    assert_eq!(out.assets[0].target, "assets/1-a-b.png");
    assert!(out.assets[0].source.ends_with("a b.png"));
}

#[test]
fn remote_images_are_never_fetched_or_embedded() {
    let html = export(
        "![r](https://example.com/r.png) ![l](http://192.168.1.1/x.png)\n",
        ProfileId::Extended,
    );
    assert!(html.contains("<img src=\"https://example.com/r.png\" alt=\"r\">"));
    // A LAN host would make the reader's browser call into the network.
    assert!(html.contains("<img alt=\"l\">"), "{html}");
}

/// Remote images served from files; anything else "not allowed".
struct FakeRemote {
    files: Vec<(&'static str, PathBuf)>,
    asked: std::sync::Mutex<Vec<String>>,
}

impl RemoteImages for FakeRemote {
    fn fetch(&self, url: &str) -> Result<PathBuf, String> {
        self.asked.lock().expect("lock").push(url.to_owned());
        self.files
            .iter()
            .find(|(u, _)| *u == url)
            .map(|(_, p)| p.clone())
            .ok_or_else(|| "not allowed by the remote-content setting".to_owned())
    }
}

#[test]
fn remote_images_are_embedded_through_the_fetcher() {
    let dir = image_dir(&[("cached.bin", 100)]);
    std::fs::write(dir.path().join("page.bin"), b"<html>not an image").expect("write");
    let remote = Arc::new(FakeRemote {
        files: vec![
            ("https://img.example/a/logo", dir.path().join("cached.bin")),
            ("https://img.example/page.png", dir.path().join("page.bin")),
        ],
        asked: std::sync::Mutex::default(),
    });
    let src = "![l](https://img.example/a/logo) ![again](https://img.example/a/logo) \
               ![no](https://other.example/n.png) ![html](https://img.example/page.png) \
               ![lan](http://192.168.1.1/x.png)\n\n\
               <img srcset=\"https://img.example/a/logo 2x\">\n";
    let mut opts = options(&dir.path().join("doc.md"));
    opts.remote = Some(remote.clone());
    let out = export_html(&tree(src, ProfileId::Extended), &opts);
    assert_eq!(
        out.html.matches("data:image/png;base64,").count(),
        2,
        "the same image twice: {}",
        out.html
    );
    assert!(
        out.html
            .contains("<img src=\"https://other.example/n.png\" alt=\"no\">")
    );
    assert!(
        out.html
            .contains("<img src=\"https://img.example/page.png\" alt=\"html\">")
    );
    assert!(out.html.contains("<img alt=\"lan\">"), "{}", out.html);
    assert!(
        out.html
            .contains("srcset=\"https://img.example/a/logo 2x\""),
        "srcset candidates stay references"
    );
    // Each URL asked once; LAN hosts and srcset candidates never.
    assert_eq!(
        *remote.asked.lock().expect("lock"),
        [
            "https://img.example/a/logo",
            "https://other.example/n.png",
            "https://img.example/page.png"
        ]
    );
    let notes: Vec<_> = out
        .notes
        .iter()
        .map(|n| (n.src.as_str(), n.reason.as_str()))
        .collect();
    assert_eq!(
        notes,
        [
            (
                "https://other.example/n.png",
                "not allowed by the remote-content setting"
            ),
            ("https://img.example/page.png", "not a known image format"),
        ]
    );

    // In an assets folder, named after the URL.
    opts.images = ImageMode::Assets;
    let out = export_html(
        &tree("![l](https://img.example/a/logo)\n", ProfileId::Extended),
        &opts,
    );
    assert!(
        out.html
            .contains("<img src=\"assets/1-logo.png\" alt=\"l\">"),
        "{}",
        out.html
    );
    assert_eq!(out.assets.len(), 1);
    assert_eq!(out.assets[0].source, dir.path().join("cached.bin"));

    // A rich copy never fetches.
    let before = remote.asked.lock().expect("lock").len();
    let src = "![l](https://img.example/a/logo)\n";
    let copy = rich_copy(&tree(src, ProfileId::Extended), 0..src.len(), &opts);
    assert!(
        copy.html.contains("https://img.example/a/logo"),
        "{}",
        copy.html
    );
    assert_eq!(remote.asked.lock().expect("lock").len(), before);
}

#[test]
fn table_of_contents() {
    let mut opts = options(&fixtures().join("doc.md"));
    opts.toc = true;
    let html = export_html(
        &tree("# A\n\n## B & x\n\n# C\n", ProfileId::Extended),
        &opts,
    )
    .html;
    assert!(
        html.contains(
            "<nav class=\"gv-toc\">\n<ul>\n<li><a href=\"#user-content-a\">A</a></li>\n<ul>\n\
             <li><a href=\"#user-content-b--x\">B &amp; x</a></li>\n</ul>\n\
             <li><a href=\"#user-content-c\">C</a></li>\n</ul>\n</nav>"
        ),
        "{html}"
    );
}

#[test]
fn rich_copy_of_a_selection() {
    let src = "# Title\n\nSome **bold** and *it*alic [link](https://e.example) text.\n\n- one\n- two\n\nLast $x$.\n";
    let t = tree(src, ProfileId::Extended);
    let opts = options(&fixtures().join("doc.md"));
    // From inside "Some" to inside "alic".
    let start = src.find("me **").expect("start");
    let end = src.find("alic").expect("end") + 2;
    let copy = rich_copy(&t, start..end, &opts);
    assert_eq!(
        copy.html,
        "<p>me <strong>bold</strong> and <em>it</em>al</p>"
    );
    assert_eq!(copy.text, "me bold and ital");

    // Whole blocks: a heading, a list and a formula.
    let copy = rich_copy(&t, 0..src.len(), &opts);
    assert!(
        copy.html
            .starts_with("<h1 id=\"user-content-title\">Title</h1>"),
        "{}",
        copy.html
    );
    assert!(
        copy.html
            .contains("<ul>\n<li>one\n</li>\n<li>two\n</li>\n</ul>"),
        "{}",
        copy.html
    );
    assert!(
        copy.html
            .contains("<a href=\"https://e.example\" rel=\"noopener noreferrer\">link</a>")
    );
    assert!(copy.html.contains("<span class=\"gv-math\"><svg"));
    assert!(!copy.html.contains("<html") && !copy.html.contains("<style"));

    // Code: the selected lines.
    let src = "```\nfirst\nsecond\nthird\n```\n";
    let t = tree(src, ProfileId::Extended);
    let at = src.find("second").expect("line");
    let copy = rich_copy(&t, at..at + 3, &opts);
    assert_eq!(
        copy.html,
        "<pre class=\"gv-code\"><code>second</code></pre>"
    );
}

#[test]
fn live_blocks_fall_back_to_their_link_or_code() {
    let src = "![[Plan.canvas]]\n\n```base\nviews: []\n```\n\n```query\ntag:#book\n```\n";
    let html = export(src, ProfileId::Obsidian);
    // The embed's line is kept; the fences are code blocks.
    assert!(html.contains("Plan.canvas"), "{html}");
    assert!(html.contains("views: []"), "{html}");
    assert!(html.contains("tag:#book"), "{html}");
    assert_eq!(html.matches("<pre").count(), 2, "{html}");
    // The same as a profile where these are not live blocks.
    let gfm = export(src, ProfileId::GfmFile);
    assert_eq!(gfm.matches("<pre").count(), 2, "{gfm}");
}
