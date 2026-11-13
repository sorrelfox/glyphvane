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
        LogOut,
        PencilLine,
        SlidersHorizontal,
        SpellCheck,
        Table2,
        TextWrap,
        X,
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
    ChevronRight,
    ChevronDown,
    Select,
    GitBranch,
    Profile,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
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
            Self::ChevronRight => IconName::ChevronRight,
            Self::ChevronDown => IconName::ChevronDown,
            Self::Select => IconName::ChevronsUpDown,
            Self::GitBranch => IconName::GitBranch,
            Self::Profile => IconName::Layers2,
            Self::WindowMinimize => IconName::WindowMinimize,
            Self::WindowMaximize => IconName::WindowMaximize,
            Self::WindowRestore => IconName::WindowRestore,
            Self::WindowClose => IconName::WindowClose,
        }
    }
}

/// The application's asset source: GPUI Kit's default component icons plus
/// the design's extra Lucide icons, every Lucide icon drawn with the design's
/// stroke width.
#[derive(Debug, Clone, Copy, Default)]
pub struct Assets;

/// Lucide draws with a stroke of 2 on its 24 grid; the design uses 1.75.
const LUCIDE_STROKE: &str = "stroke-width=\"2\"";
const DESIGN_STROKE: &str = "stroke-width=\"1.75\"";

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let data = match DesignIcons.load(path)? {
            Some(data) => Some(data),
            None => gpui::assets::Assets.load(path)?,
        };
        Ok(data.map(restroke))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = DesignIcons.list(path)?;
        paths.extend(gpui::assets::Assets.list(path)?);
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

    const ALL: [Icon; 27] = [
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
        Icon::ChevronRight,
        Icon::ChevronDown,
        Icon::Select,
        Icon::GitBranch,
        Icon::Profile,
        Icon::WindowMinimize,
        Icon::WindowMaximize,
        Icon::WindowRestore,
        Icon::WindowClose,
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
    fn lucide_icons_use_the_design_stroke() {
        let path = Icon::Profile.name().path();
        let data = Assets.load(&path).ok().flatten().unwrap_or_default();
        let svg = String::from_utf8_lossy(&data);
        assert!(svg.contains(DESIGN_STROKE), "{path} keeps Lucide's stroke");
        assert!(!svg.contains(LUCIDE_STROKE));
    }
}
