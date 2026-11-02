//! Paper sizes and zoom steps, as `PageLayout.swift` and `ScalingScrollView.swift` have them.

/// TextEdit's print margin on every side, in points.
pub const PAGE_MARGIN_PT: f32 = 72.0;
/// The gap between pages on the Mac. The layout hook has no gaps yet, so nothing draws
/// this. It stays here so that the value is on record for that hook's change.
pub const PAGE_SEPARATOR_PT: f32 = 5.0;
/// The zoom range: 25% to 1600%.
pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 16.0;

const DIPS_PER_POINT: f32 = 96.0 / 72.0;

/// A sheet of paper, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paper {
    pub width: f32,
    pub height: f32,
}

impl Paper {
    pub const LETTER: Paper = Paper {
        width: 612.0,
        height: 792.0,
    };
    pub const A4: Paper = Paper {
        width: 595.0,
        height: 842.0,
    };
    pub const LEGAL: Paper = Paper {
        width: 612.0,
        height: 1008.0,
    };

    /// Letter in the US and Canada, A4 elsewhere (`PaperSize.regionDefault`).
    pub fn for_region(region: &str) -> Paper {
        if region.eq_ignore_ascii_case("US") || region.eq_ignore_ascii_case("CA") {
            Paper::LETTER
        } else {
            Paper::A4
        }
    }
    /// The saved `PaperSize` value, or the region's size for nothing or junk, as on the Mac.
    pub fn from_setting(saved: Option<&str>, region: &str) -> Paper {
        match saved {
            Some("Letter") => Paper::LETTER,
            Some("A4") => Paper::A4,
            Some("Legal") => Paper::LEGAL,
            _ => Paper::for_region(region),
        }
    }
    /// The width between the margins, in points.
    pub fn text_width_pt(self) -> f32 {
        self.width - 2.0 * PAGE_MARGIN_PT
    }
    /// The height between the margins, in points.
    pub fn text_height_pt(self) -> f32 {
        self.height - 2.0 * PAGE_MARGIN_PT
    }
    /// The width between the margins at 100% zoom, in dips. A point of paper is 96/72
    /// dips, so a page shows at its true size and the PDF uses this same width.
    pub fn text_width_dips(self) -> f32 {
        self.text_width_pt() * DIPS_PER_POINT
    }
    pub fn margin_dips(self) -> f32 {
        PAGE_MARGIN_PT * DIPS_PER_POINT
    }
    pub fn width_dips(self) -> f32 {
        self.width * DIPS_PER_POINT
    }
}

/// Zoom In doubles. From just under to just over 50% it snaps to 100%.
pub fn zoom_in_scale(scale: f32) -> f32 {
    let next = if scale > 0.4 && scale < 0.6 {
        1.0
    } else {
        scale * 2.0
    };
    next.min(MAX_ZOOM)
}
/// Zoom Out halves. From just under to just over 200% it snaps to 100%.
pub fn zoom_out_scale(scale: f32) -> f32 {
    let next = if scale > 1.8 && scale < 2.2 {
        1.0
    } else {
        scale / 2.0
    };
    next.max(MIN_ZOOM)
}

/// The menu item that switches modes names the mode it switches to.
pub fn wrap_menu_title(page_wrap_on: bool) -> &'static str {
    if page_wrap_on {
        "Wrap to Window"
    } else {
        "Wrap to Page"
    }
}
