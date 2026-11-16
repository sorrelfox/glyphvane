// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Icons: which Lucide icon each part of the interface uses, and the asset
//! source that serves them.

use std::borrow::Cow;

use gpui::assets::IconName;
use gpui::{AssetSource, Result, SharedString, Styled as _, px};

// Lucide icons of the design that GPUI Kit's default component bundle does not
// embed. The rest of the inventory (panel-left, book-open, plus, ...) is in
// that bundle already.
gpui::assets::icon_assets!(
    DesignIcons,
    [
        ArrowUpDown,
        ArrowUpRight,
        Bookmark,
        Bug,
        Check,
        CircleQuestionMark,
        ClipboardList,
        ChevronsDownUp,
        Columns2,
        Contrast,
        CornerUpRight,
        FilePlus,
        FolderPlus,
        GitBranch,
        Hash,
        Image,
        Keyboard,
        Layers2,
        Lightbulb,
        Link,
        ListFilter,
        Lock,
        List,
        LogOut,
        PencilLine,
        Quote,
        SlidersHorizontal,
        SpellCheck,
        Table2,
        TextWrap,
        X,
        Zap,
    ]
);

/// The interface's icons, named by what they are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    SidebarToggle,
    PanelToggle,
    Back,
    Forward,
    NewTab,
    CloseTab,
    MoreActions,
    Menu,
    Preview,
    Edit,
    Split,
    Files,
    Search,
    Tags,
    Bookmarks,
    NewNote,
    NewFolder,
    Sort,
    CollapseAll,
    Folder,
    Note,
    Canvas,
    Base,
    ImageFile,
    OtherFile,
    ChevronRight,
    ChevronDown,
    /// A fold marker in the editor's gutter, and a folded one.
    FoldMarker,
    Folded,
    Select,
    GitBranch,
    Profile,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
    /// A code block's copy button.
    CopyCode,
    /// A code block's soft wrap toggle.
    WrapCode,
    /// Find in page: previous match, next match, close.
    FindPrevious,
    FindNext,
    CloseFind,
    /// The check mark of a menu item that is on (not designed).
    MenuCheck,
    /// A menu row that opens a submenu.
    Submenu,
    /// The search icon of the quick switcher's and the command palette's
    /// input row.
    SwitcherSearch,
    /// An alias row of the quick switcher ("Alias → Note").
    SwitcherAlias,
    /// A heading row of the quick switcher (`#` mode; not designed).
    SwitcherHeading,
    /// A block row of the quick switcher (`^` mode; not designed).
    SwitcherBlock,
    /// A note that does not exist yet (the design draws its outline
    /// dashed).
    NotCreated,
    /// The infobar's kind icons and its dismiss button.
    NoticeInformation,
    NoticeWarning,
    NoticeError,
    Dismiss,
}

impl Icon {
    /// The icon as an element of `size` logical pixels, drawn in the current
    /// text colour.
    pub fn sized(self, size: f32) -> gpui::component::Icon {
        gpui::component::Icon::new(self.name()).size(px(size))
    }

    /// The Lucide icon (or, for window controls, the GPUI Kit glyph) to draw.
    pub fn name(self) -> IconName {
        match self {
            Self::SidebarToggle => IconName::PanelLeft,
            Self::PanelToggle => IconName::PanelRight,
            Self::Back => IconName::ArrowLeft,
            Self::Forward => IconName::ArrowRight,
            Self::NewTab => IconName::Plus,
            Self::CloseTab => IconName::X,
            Self::MoreActions => IconName::Ellipsis,
            Self::Menu => IconName::Menu,
            Self::Preview => IconName::BookOpen,
            Self::Edit => IconName::PencilLine,
            Self::Split => IconName::Columns2,
            Self::Files => IconName::Folder,
            Self::Search => IconName::Search,
            Self::Tags => IconName::Hash,
            Self::Bookmarks => IconName::Bookmark,
            Self::NewNote => IconName::FilePlus,
            Self::NewFolder => IconName::FolderPlus,
            Self::Sort => IconName::ArrowUpDown,
            Self::CollapseAll => IconName::ChevronsDownUp,
            Self::Folder => IconName::Folder,
            Self::Note => IconName::FileText,
            Self::Canvas => IconName::LayoutDashboard,
            Self::Base => IconName::Table2,
            Self::ImageFile => IconName::Image,
            // Not in the design's inventory: Lucide's plain `file`.
            Self::OtherFile => IconName::File,
            Self::ChevronRight => IconName::ChevronRight,
            Self::ChevronDown => IconName::ChevronDown,
            Self::FoldMarker => IconName::ChevronDown,
            Self::Folded => IconName::ChevronRight,
            Self::Select => IconName::ChevronsUpDown,
            Self::GitBranch => IconName::GitBranch,
            Self::Profile => IconName::Layers2,
            Self::WindowMinimize => IconName::WindowMinimize,
            Self::WindowMaximize => IconName::WindowMaximize,
            Self::WindowRestore => IconName::WindowRestore,
            Self::WindowClose => IconName::WindowClose,
            Self::CopyCode => IconName::Copy,
            Self::WrapCode => IconName::TextWrap,
            Self::FindPrevious => IconName::ArrowUp,
            Self::FindNext => IconName::ArrowDown,
            Self::CloseFind => IconName::X,
            // Not in the design's inventory: Lucide's `check`.
            Self::MenuCheck => IconName::Check,
            Self::Submenu => IconName::ChevronRight,
            Self::SwitcherSearch => IconName::Search,
            Self::SwitcherAlias => IconName::CornerUpRight,
            Self::SwitcherHeading => IconName::Hash,
            Self::SwitcherBlock => IconName::Link,
            Self::NotCreated => IconName::FilePlus,
            Self::NoticeInformation => IconName::Info,
            Self::NoticeWarning => IconName::TriangleAlert,
            Self::NoticeError => IconName::CircleX,
            Self::Dismiss => IconName::X,
        }
    }
}

/// Wraps caption glyph paths in an SVG on the 24 grid with the design's
/// stroke of 1.5.
macro_rules! caption_svg {
    ($paths:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">"#,
            $paths,
            "</svg>"
        )
    };
}

/// The glyphs of the Windows caption buttons. They are not Lucide icons: the
/// design draws them as thin lines on the 24 grid, stroke 1.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionGlyph {
    Minimize,
    Maximize,
    /// Not in the design. Windows' convention: two offset squares.
    Restore,
    Close,
}

impl CaptionGlyph {
    pub const ALL: [Self; 4] = [Self::Minimize, Self::Maximize, Self::Restore, Self::Close];

    /// The glyph as an element of `size` logical pixels, drawn in the current
    /// text colour.
    pub fn sized(self, size: f32) -> gpui::component::Icon {
        gpui::component::Icon::empty()
            .path(self.path())
            .size(px(size))
    }

    /// The glyph's path in the application's asset source.
    pub fn path(self) -> &'static str {
        match self {
            Self::Minimize => "icons/glyphvane/caption-minimize.svg",
            Self::Maximize => "icons/glyphvane/caption-maximize.svg",
            Self::Restore => "icons/glyphvane/caption-restore.svg",
            Self::Close => "icons/glyphvane/caption-close.svg",
        }
    }

    fn svg(self) -> &'static str {
        match self {
            Self::Minimize => caption_svg!(r#"<path d="M3 12h18"/>"#),
            Self::Maximize => caption_svg!(r#"<path d="M4 4h16v16H4z"/>"#),
            Self::Restore => {
                caption_svg!(r#"<path d="M4 8h12v12H4z"/><path d="M8 8V4h12v12h-4"/>"#)
            }
            Self::Close => caption_svg!(r#"<path d="M3 3l18 18M21 3 3 21"/>"#),
        }
    }

    fn from_path(path: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|glyph| glyph.path() == path)
    }
}

/// The design's icon for a note that does not exist yet: Lucide's
/// `file-plus` with its outline drawn dashed, 3 on and 2.6 off (icons.md).
pub const FILE_PLUS_DASHED: &str = "icons/glyphvane/file-plus-dashed.svg";

/// Draws the first path of an SVG (the outline of a Lucide file icon)
/// dashed.
fn dash_outline(svg: Cow<'static, [u8]>) -> Cow<'static, [u8]> {
    match std::str::from_utf8(&svg) {
        Ok(text) if text.contains("<path ") => Cow::Owned(
            text.replacen("<path ", "<path stroke-dasharray=\"3 2.6\" ", 1)
                .into_bytes(),
        ),
        _ => svg,
    }
}

/// The application's asset source: GPUI Kit's default component icons plus
/// the design's extra Lucide icons, every Lucide icon drawn with the design's
/// stroke width, and the Windows caption glyphs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Assets;

/// Lucide draws with a stroke of 2 on its 24 grid; the design uses 1.75.
const LUCIDE_STROKE: &str = "stroke-width=\"2\"";
const DESIGN_STROKE: &str = "stroke-width=\"1.75\"";

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(glyph) = CaptionGlyph::from_path(path) {
            return Ok(Some(Cow::Borrowed(glyph.svg().as_bytes())));
        }
        if path == FILE_PLUS_DASHED {
            return Ok(self.load(&IconName::FilePlus.path())?.map(dash_outline));
        }
        let data = match DesignIcons.load(path)? {
            Some(data) => Some(data),
            None => gpui::assets::Assets.load(path)?,
        };
        Ok(data.map(restroke))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = DesignIcons.list(path)?;
        paths.extend(gpui::assets::Assets.list(path)?);
        paths.extend(
            CaptionGlyph::ALL
                .map(CaptionGlyph::path)
                .into_iter()
                .chain([FILE_PLUS_DASHED])
                .filter(|glyph| glyph.starts_with(path))
                .map(SharedString::from),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

fn restroke(svg: Cow<'static, [u8]>) -> Cow<'static, [u8]> {
    match std::str::from_utf8(&svg) {
        Ok(text) if text.contains(LUCIDE_STROKE) => {
            Cow::Owned(text.replace(LUCIDE_STROKE, DESIGN_STROKE).into_bytes())
        }
        _ => svg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 51] = [
        Icon::SidebarToggle,
        Icon::PanelToggle,
        Icon::Back,
        Icon::Forward,
        Icon::NewTab,
        Icon::CloseTab,
        Icon::MoreActions,
        Icon::Menu,
        Icon::Preview,
        Icon::Edit,
        Icon::Split,
        Icon::Files,
        Icon::Search,
        Icon::Tags,
        Icon::Bookmarks,
        Icon::NewNote,
        Icon::NewFolder,
        Icon::Sort,
        Icon::CollapseAll,
        Icon::Folder,
        Icon::Note,
        Icon::Canvas,
        Icon::Base,
        Icon::ImageFile,
        Icon::OtherFile,
        Icon::ChevronRight,
        Icon::ChevronDown,
        Icon::FoldMarker,
        Icon::Folded,
        Icon::Select,
        Icon::GitBranch,
        Icon::Profile,
        Icon::WindowMinimize,
        Icon::WindowMaximize,
        Icon::WindowRestore,
        Icon::WindowClose,
        Icon::CopyCode,
        Icon::WrapCode,
        Icon::FindPrevious,
        Icon::FindNext,
        Icon::CloseFind,
        Icon::MenuCheck,
        Icon::Submenu,
        Icon::SwitcherAlias,
        Icon::SwitcherHeading,
        Icon::SwitcherBlock,
        Icon::NotCreated,
        Icon::NoticeInformation,
        Icon::NoticeWarning,
        Icon::NoticeError,
        Icon::Dismiss,
    ];

    #[test]
    fn every_interface_icon_is_served() {
        for icon in ALL {
            let path = icon.name().path();
            let data = Assets.load(&path).ok().flatten();
            assert!(
                data.is_some(),
                "{icon:?} ({path}) is not in the asset source"
            );
        }
    }

    #[test]
    fn caption_glyphs_are_served_with_their_own_stroke() {
        for glyph in CaptionGlyph::ALL {
            let data = Assets.load(glyph.path()).ok().flatten().unwrap_or_default();
            let svg = String::from_utf8_lossy(&data);
            assert!(svg.contains("stroke-width=\"1.5\""), "{glyph:?}: {svg}");
            assert!(
                Assets
                    .list("icons/")
                    .unwrap_or_default()
                    .iter()
                    .any(|p| p == glyph.path())
            );
        }
    }

    #[test]
    fn the_dashed_file_icon_dashes_only_its_outline() {
        let data = Assets
            .load(FILE_PLUS_DASHED)
            .ok()
            .flatten()
            .unwrap_or_default();
        let svg = String::from_utf8_lossy(&data);
        assert_eq!(
            svg.matches("stroke-dasharray=\"3 2.6\"").count(),
            1,
            "{svg}"
        );
        assert!(svg.matches("<path ").count() > 1);
        assert!(svg.contains(DESIGN_STROKE));
        assert!(
            Assets
                .list("icons/glyphvane/")
                .unwrap_or_default()
                .iter()
                .any(|p| p == FILE_PLUS_DASHED)
        );
    }

    #[test]
    fn lucide_icons_use_the_design_stroke() {
        let path = Icon::Profile.name().path();
        let data = Assets.load(&path).ok().flatten().unwrap_or_default();
        let svg = String::from_utf8_lossy(&data);
        assert!(svg.contains(DESIGN_STROKE), "{path} keeps Lucide's stroke");
        assert!(!svg.contains(LUCIDE_STROKE));
    }
}
