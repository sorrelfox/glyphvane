// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Remote images in headless windows, against a loopback HTTP server each
//! test starts (127.0.0.1, an ephemeral port; nothing leaves the
//! machine): Ask shows placeholders and the infobar and fetches nothing
//! until *Load once*, *Always allow* or *Block*; Block never fetches; Allow
//! still asks for a loopback host; switching to Block hides a loaded
//! image; in-vault settings cannot loosen the policy.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use image::{ImageEncoder as _, Rgba, RgbaImage};

use super::*;
use crate::files::notice::{Notice, NoticeAction, Topic};
use crate::reader::remote::{Policy, RemoteSession};
use crate::settings::Target;

const MODE: &str = "privacy_and_security.remote_content";
const ALLOW: &str = "privacy_and_security.remote_allow_sites";
const BLOCK: &str = "privacy_and_security.remote_block_sites";

/// Serves one PNG for every path and counts the requests.
struct Server {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("address").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&hits);
        let body = png();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                        break;
                    }
                }
                count.fetch_add(1, Ordering::SeqCst);
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self { port, hits }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn png() -> Vec<u8> {
    let img = RgbaImage::from_pixel(40, 20, Rgba([10, 120, 200, 255]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&img, 40, 20, image::ExtendedColorType::Rgba8)
        .expect("png");
    out
}

/// A window on a note with one remote image from `server`; the remote
/// cache in a folder of its own. `mode` is set before the note loads.
struct Setup {
    _dir: tempfile::TempDir,
    _cache: tempfile::TempDir,
    workspace: Entity<Workspace>,
    reader: Entity<Reader>,
    vcx: VisualTestContext,
}

fn setup(cx: &mut TestAppContext, server: &Server, mode: Option<&str>) -> Setup {
    let doc = format!("# Remote\n\n![logo]({})\n", server.url("/logo.png"));
    setup_doc(cx, &doc, mode)
}

/// A window on a note with the text `doc`, as [`setup`].
fn setup_doc(cx: &mut TestAppContext, doc: &str, mode: Option<&str>) -> Setup {
    let dir = temp(&[("doc.md", doc)]);
    let file = dir.path().join("doc.md");
    let cache = tempfile::tempdir().expect("cache dir");
    let (window, workspace) = open(cx, &file);
    cx.update(|cx| {
        RemoteSession::use_cache_dir(cx, cache.path().to_path_buf());
        if let Some(mode) = mode {
            set(
                cx,
                Target::AllVaults,
                MODE,
                toml::Value::String(mode.into()),
            );
        }
    });
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    Setup {
        _dir: dir,
        _cache: cache,
        workspace,
        reader,
        vcx,
    }
}

fn set(cx: &mut gpui::App, target: Target, key: &str, value: toml::Value) {
    AppSettings::set_value(cx, target, key, &value).expect("setting accepted");
}

fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..6 {
        draw(vcx);
    }
}

impl Setup {
    fn notice(&mut self) -> Option<Notice> {
        self.workspace
            .read_with(&self.vcx, |ws, cx| ws.infobar_notice(cx))
    }

    /// Clicks the infobar button for `action`.
    fn click(&mut self, action: NoticeAction) {
        let notice = self.notice().expect("an infobar");
        let index = notice
            .buttons
            .iter()
            .position(|b| b.action == action)
            .expect("the button");
        let selector: &'static str = Box::leak(format!("infobar-action-{index}").into_boxed_str());
        let at = self
            .vcx
            .debug_bounds(selector)
            .expect("the button is drawn")
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        settle(&mut self.vcx);
    }

    /// The image cards' lines.
    fn cards(&mut self) -> Vec<String> {
        let st = state(&self.reader, &mut self.vcx);
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        doc.tree
            .items()
            .iter()
            .filter_map(|i| s.cache.image_cards.get(&i.id))
            .flatten()
            .map(|c| c.message.clone())
            .collect()
    }

    fn images_ready(&mut self) -> usize {
        state(&self.reader, &mut self.vcx)
            .borrow()
            .cache
            .images
            .ready()
    }

    fn list(&mut self, key: &str) -> Vec<String> {
        self.vcx.update(|_, cx| {
            cx.global::<AppSettings>()
                .effective()
                .value(key)
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
    }
}

#[gpui::test]
fn ask_waits_then_load_once_fetches_in_the_background(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, None);
    // Ask (the default): a placeholder and the infobar; nothing fetched.
    assert_eq!(server.hits(), 0);
    assert_eq!(
        t.cards(),
        vec!["logo: Remote image from 127.0.0.1, not loaded.".to_owned()]
    );
    let notice = t.notice().expect("the remote-content infobar");
    assert_eq!(notice.topic, Topic::RemoteContent);
    assert_eq!(
        notice.title.as_ref(),
        "This note references content from 127.0.0.1."
    );
    assert!(notice.offers(&NoticeAction::RemoteLoadOnce));
    assert!(notice.offers(&NoticeAction::RemoteBlock));
    let waiting = t
        .reader
        .read_with(&t.vcx, |r, _| r.remote_waiting().to_vec());
    assert_eq!(waiting, vec!["127.0.0.1".to_owned()]);

    t.click(NoticeAction::RemoteLoadOnce);
    // Fetched once, off the frame, then drawn like a local image.
    assert_eq!(server.hits(), 1);
    assert!(t.cards().is_empty(), "{:?}", t.cards());
    assert_eq!(t.images_ready(), 1);
    assert!(t.notice().is_none());
    // Load once writes nothing.
    assert!(t.list(ALLOW).is_empty());
    assert_eq!(cx.update(|cx| RemoteSession::started(cx)), 1);
}

#[gpui::test]
fn a_loaded_picture_source_does_not_build_the_document_again(cx: &mut TestAppContext) {
    let server = Server::start();
    let doc = format!(
        "# Remote\n\nText.\n\n<picture>\n  <source media=\"(prefers-color-scheme: dark)\" \
         srcset=\"{}\">\n  <img alt=\"Logo\" src=\"{}\">\n</picture>\n",
        server.url("/dark.png"),
        server.url("/light.png")
    );
    let mut t = setup_doc(cx, &doc, None);
    let version = |t: &mut Setup| state(&t.reader, &mut t.vcx).borrow().doc_version;
    let before = version(&mut t);
    assert_eq!(t.images_ready(), 0);
    t.click(NoticeAction::RemoteLoadOnce);
    assert!(server.hits() >= 1);
    // The picture takes the image's size in place: the rest of the
    // document is not built again.
    assert_eq!(t.images_ready(), 1);
    assert_eq!(version(&mut t), before);
    let st = state(&t.reader, &mut t.vcx);
    let s = st.borrow();
    let size = s
        .doc
        .as_ref()
        .expect("loaded")
        .tree
        .nodes()
        .iter()
        .find_map(|n| match &n.kind {
            gv_render_tree::NodeKind::Picture(p) => Some(p.size(false)),
            _ => None,
        });
    assert_eq!(
        size,
        Some(gv_render_tree::ImageSize::Known {
            width: 40.0,
            height: 20.0
        })
    );
}

#[gpui::test]
fn always_allow_adds_the_site_and_loads(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, None);
    assert_eq!(server.hits(), 0);
    t.click(NoticeAction::RemoteAlwaysAllow);
    assert_eq!(t.list(ALLOW), vec!["127.0.0.1".to_owned()]);
    assert_eq!(server.hits(), 1);
    assert_eq!(t.images_ready(), 1);
    assert!(t.notice().is_none());
}

#[gpui::test]
fn block_this_site_never_fetches(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, None);
    t.click(NoticeAction::RemoteBlock);
    assert_eq!(t.list(BLOCK), vec!["127.0.0.1".to_owned()]);
    assert_eq!(
        t.cards(),
        vec!["logo: Images from 127.0.0.1 are blocked.".to_owned()]
    );
    assert!(t.notice().is_none());
    assert_eq!(server.hits(), 0);
    assert_eq!(cx.update(|cx| RemoteSession::started(cx)), 0);
}

#[gpui::test]
fn block_mode_never_fetches_and_offers_nothing(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, Some("block"));
    assert_eq!(
        t.cards(),
        vec!["logo: Remote content is blocked in your settings.".to_owned()]
    );
    assert!(t.notice().is_none(), "no load buttons in Block");
    // An allow list does not lift a global Block.
    t.vcx.update(|_, cx| {
        set(
            cx,
            Target::AllVaults,
            ALLOW,
            toml::Value::Array(vec![toml::Value::String("127.0.0.1".into())]),
        );
    });
    settle(&mut t.vcx);
    assert_eq!(server.hits(), 0);
    assert_eq!(cx.update(|cx| RemoteSession::started(cx)), 0);
}

#[gpui::test]
fn allow_mode_still_asks_for_a_loopback_host(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, Some("allow"));
    assert_eq!(server.hits(), 0);
    assert!(t.notice().is_some_and(|n| n.topic == Topic::RemoteContent));
}

#[gpui::test]
fn switching_to_block_hides_a_loaded_image(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, None);
    t.click(NoticeAction::RemoteLoadOnce);
    assert_eq!(t.images_ready(), 1);
    assert!(t.cards().is_empty());
    t.vcx.update(|_, cx| {
        set(
            cx,
            Target::AllVaults,
            MODE,
            toml::Value::String("block".into()),
        );
    });
    settle(&mut t.vcx);
    assert_eq!(
        t.cards(),
        vec!["logo: Remote content is blocked in your settings.".to_owned()]
    );
    assert_eq!(server.hits(), 1);
}

#[gpui::test]
fn the_infobar_dismisses_until_other_sites_wait(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, None);
    assert!(t.notice().is_some());
    let at = t
        .vcx
        .debug_bounds("infobar-dismiss")
        .expect("dismiss")
        .center();
    t.vcx.simulate_click(at, Modifiers::none());
    settle(&mut t.vcx);
    assert!(t.notice().is_none());
    // The placeholder stays, and nothing was fetched.
    assert_eq!(t.cards().len(), 1);
    assert_eq!(server.hits(), 0);
}

#[gpui::test]
fn vault_settings_files_only_tighten(cx: &mut TestAppContext) {
    // A cloned vault asks for Allow and allow-lists a tracker: ignored.
    let vault = temp(&[
        ("note.md", "# N\n"),
        (
            ".glyphvane/settings.toml",
            "[privacy_and_security]\nremote_content = \"allow\"\nremote_allow_sites = [\"tracker.example\"]\nremote_block_sites = [\"ads.example\"]\n",
        ),
    ]);
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        AppSettings::open_vault(cx, vault.path());
        let p = Policy::of_app(cx);
        assert_eq!(p.mode, gv_settings::RemoteContent::Ask);
        assert!(p.allow.is_empty(), "{:?}", p.allow);
        // Tightening is kept: the vault's block list applies.
        assert_eq!(p.block, vec!["ads.example".to_owned()]);
        // The vault's overlay in the app data dir is the user's: it may
        // switch the vault to Allow...
        set(cx, Target::Vault, MODE, toml::Value::String("allow".into()));
        assert_eq!(Policy::of_app(cx).mode, gv_settings::RemoteContent::Allow);
        // ...but a global Block is absolute.
        set(
            cx,
            Target::AllVaults,
            MODE,
            toml::Value::String("block".into()),
        );
        assert_eq!(Policy::of_app(cx).mode, gv_settings::RemoteContent::Block);
    });
}

const SHELF: &str = "views:\n  - type: cards\n    name: Shelf\n    image: note.cover\n";

/// A base embedded in the note of `t` (its covers approve for that note).
fn embedded_base(t: &mut Setup) -> Entity<crate::bases_view::BasesView> {
    let path = t
        .reader
        .read_with(&t.vcx, |r, _| r.path.clone())
        .expect("a note");
    let root = path.parent().map(Path::to_path_buf);
    let name: Arc<str> = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .expect("a file name")
        .into();
    t.vcx.update(|_, cx| {
        crate::bases_view::BasesView::embedded(Arc::from(SHELF), Some(name), None, root, cx)
    })
}

#[gpui::test]
fn load_once_in_the_infobar_also_loads_an_embedded_bases_covers(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, None);
    let base = embedded_base(&mut t);
    let cover = server.url("/cover.png");
    let state = |t: &mut Setup| base.update(&mut t.vcx, |b, cx| b.cover_state(&cover, cx));
    assert_eq!(state(&mut t), "waiting");
    t.click(NoticeAction::RemoteLoadOnce);
    // The note's image and, once asked again, the base's cover load.
    assert_eq!(t.images_ready(), 1);
    assert_ne!(state(&mut t), "waiting");
    settle(&mut t.vcx);
    t.vcx.run_until_parked();
    assert_eq!(state(&mut t), "image");
    assert_eq!(server.hits(), 2);
    assert!(t.list(ALLOW).is_empty(), "nothing is written");
}

#[gpui::test]
fn load_once_on_a_bases_cover_also_loads_the_notes_images(cx: &mut TestAppContext) {
    let server = Server::start();
    let mut t = setup(cx, &server, None);
    let base = embedded_base(&mut t);
    assert!(t.notice().is_some());
    assert_eq!(server.hits(), 0);
    base.update(&mut t.vcx, |b, cx| b.load_once("127.0.0.1", cx));
    settle(&mut t.vcx);
    t.vcx.run_until_parked();
    // The note's image was fetched without its infobar being answered.
    assert_eq!(server.hits(), 1);
    assert_eq!(t.images_ready(), 1);
    assert!(t.notice().is_none());
    assert!(t.list(ALLOW).is_empty(), "nothing is written");
}
