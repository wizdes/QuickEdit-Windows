//! The line-number gutter, the document map, Wrap to Page and zoom of one window.
//!
//! [`ViewFrame`] owns these parts and the editor's side margins. The numbers come from
//! the Mac app's `LineNumberRulerView`, `DocumentMapView`, `MultiplePageView`,
//! `ScalingScrollView` and `PageLayout`. The README lists the calls a host makes.
mod fade;
mod frame;
mod gutter;
mod map;
mod page;
mod system;

pub use fade::{FADE_IN_SECONDS, FADE_OUT_SECONDS, Fade};
pub use frame::{FrameCounts, LARGE_FILE_BYTES, LARGE_FILE_TITLE_NOTE, MapBox, ViewFrame};
pub use gutter::{
    GUTTER_FONT, GUTTER_LEFT_PADDING, GUTTER_REDRAW_SECONDS, GUTTER_RIGHT_PADDING, GutterLabel,
    gutter_digits, gutter_font_points, gutter_labels, gutter_width,
};
pub use map::{
    MAP_BLOCK_HEIGHT, MAP_COLUMN_WIDTH, MAP_HOVER_MARGIN, MAP_REDRAW_SECONDS, MAP_ROW_HEIGHT,
    MAP_WIDTH, MapGeometry, first_line_for_box_top, map_top_line, row_runs,
};
pub use page::{
    MAX_ZOOM, MIN_ZOOM, PAGE_MARGIN_PT, PAGE_SEPARATOR_PT, Paper, wrap_menu_title, zoom_in_scale,
    zoom_out_scale,
};
pub use system::{system_paper, system_reduce_motion};
