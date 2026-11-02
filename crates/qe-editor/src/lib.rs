//! QuickEdit's editable text area.
use qe_api::{
    Affinity, ByteOffset, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, LineIndex,
    LookupError, Modifiers, MouseButton, MouseCursor, Point, Rect, StoreStatus, TextMeasurer,
    TextPosition, TextSnapshot, TextStore, Theme, Utf16Offset, View, ViewContext, Widget, WidgetId,
    WidgetRole, WidgetStates,
};
use qe_buffer::{MappedStore, PieceStore, UndoHistory};
use qe_layout::{DirectWriteLayout, HeightMap, LineCache};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

pub trait Clipboard {
    fn read(&mut self) -> Result<String, String>;
    fn write(&mut self, value: &str) -> Result<(), String>;
}
#[derive(Default)]
pub struct MemoryClipboard(pub String);
impl Clipboard for MemoryClipboard {
    fn read(&mut self) -> Result<String, String> {
        Ok(self.0.clone())
    }
    fn write(&mut self, value: &str) -> Result<(), String> {
        self.0 = value.into();
        Ok(())
    }
}

type LayoutResult = (
    qe_layout::CacheTicket,
    Result<qe_api::LineLayout, qe_layout::LayoutError>,
);

/// Counts the six line endings without changing any bytes.
pub fn preferred_line_ending_bytes(bytes: &[u8]) -> &'static str {
    const ENDINGS: [&str; 6] = ["\n", "\r\n", "\r", "\u{85}", "\u{2028}", "\u{2029}"];
    let mut counts = [0_usize; 6];
    let mut index = 0;
    while index < bytes.len() {
        let (kind, width) = match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => (Some(1), 2),
            b'\r' => (Some(2), 1),
            b'\n' => (Some(0), 1),
            0xc2 if bytes.get(index + 1) == Some(&0x85) => (Some(3), 2),
            0xe2 if bytes.get(index + 1..index + 3) == Some(&[0x80, 0xa8]) => (Some(4), 3),
            0xe2 if bytes.get(index + 1..index + 3) == Some(&[0x80, 0xa9]) => (Some(5), 3),
            _ => (None, 1),
        };
        if let Some(kind) = kind {
            counts[kind] += 1;
        }
        index += width;
    }
    let best = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .map_or(0, |(index, _)| index);
    if counts[best] == 0 {
        "\n"
    } else {
        ENDINGS[best]
    }
}
enum WorkerTask {
    Layout(
        qe_layout::CacheTicket,
        Arc<dyn TextSnapshot>,
        Arc<DirectWriteLayout>,
    ),
    LongMap(
        LineIndex,
        Dip,
        Arc<dyn TextSnapshot>,
        Arc<DirectWriteLayout>,
        u64,
    ),
    KnownFirst(
        usize,
        Dip,
        Arc<dyn TextSnapshot>,
        Arc<DirectWriteLayout>,
        u64,
    ),
    Drop(Arc<dyn TextSnapshot>),
}

/// Converts cache's plain lookups to nonwaiting lookups on a mapped snapshot.
struct NonBlocking(Arc<dyn TextSnapshot>);
impl TextSnapshot for NonBlocking {
    fn revision(&self) -> u64 {
        self.0.revision()
    }
    fn len_bytes(&self) -> ByteOffset {
        self.0.status().known_bytes
    }
    fn len_utf16(&self) -> Utf16Offset {
        self.0.status().known_utf16
    }
    fn line_count(&self) -> usize {
        self.0.status().known_lines
    }
    fn byte_to_utf16(&self, b: ByteOffset) -> Option<Utf16Offset> {
        self.0.try_byte_to_utf16(b).ok()
    }
    fn utf16_to_byte(&self, u: Utf16Offset) -> Option<ByteOffset> {
        self.0.try_utf16_to_byte(u).ok()
    }
    fn byte_to_position(&self, b: ByteOffset) -> Option<TextPosition> {
        self.0.try_byte_to_position(b).ok()
    }
    fn position_to_byte(&self, p: TextPosition) -> Option<ByteOffset> {
        self.0.try_position_to_byte(p).ok()
    }
    fn line_range(&self, l: LineIndex) -> Option<Range<ByteOffset>> {
        self.0.try_line_range(l).ok()
    }
    fn visit_chunks(&self, r: Range<ByteOffset>, v: &mut dyn FnMut(&str)) -> bool {
        self.0.try_visit_chunks(r, v).is_ok()
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn status(&self) -> StoreStatus {
        self.0.status()
    }
    fn try_known_line(&self, l: LineIndex) -> Result<qe_api::KnownLine, LookupError> {
        self.0.try_known_line(l)
    }
    fn try_len_bytes(&self) -> Result<ByteOffset, LookupError> {
        self.0.try_len_bytes()
    }
    fn try_len_utf16(&self) -> Result<Utf16Offset, LookupError> {
        self.0.try_len_utf16()
    }
    fn try_line_count(&self) -> Result<usize, LookupError> {
        self.0.try_line_count()
    }
    fn try_byte_to_utf16(&self, b: ByteOffset) -> Result<Utf16Offset, LookupError> {
        self.0.try_byte_to_utf16(b)
    }
    fn try_utf16_to_byte(&self, u: Utf16Offset) -> Result<ByteOffset, LookupError> {
        self.0.try_utf16_to_byte(u)
    }
    fn try_byte_to_position(&self, b: ByteOffset) -> Result<TextPosition, LookupError> {
        self.0.try_byte_to_position(b)
    }
    fn try_position_to_byte(&self, p: TextPosition) -> Result<ByteOffset, LookupError> {
        self.0.try_position_to_byte(p)
    }
    fn try_line_range(&self, l: LineIndex) -> Result<Range<ByteOffset>, LookupError> {
        self.0.try_line_range(l)
    }
    fn try_visit_chunks(
        &self,
        r: Range<ByteOffset>,
        v: &mut dyn FnMut(&str),
    ) -> Result<(), LookupError> {
        self.0.try_visit_chunks(r, v)
    }
}

pub struct EditorView {
    store: PieceStore,
    mapped: Option<MappedStore>,
    history: UndoHistory,
    engine: Arc<DirectWriteLayout>,
    cache: RefCell<LineCache>,
    heights: HeightMap,
    tasks: Sender<WorkerTask>,
    results: Receiver<LayoutResult>,
    map_results: Receiver<(
        LineIndex,
        u64,
        u64,
        Result<HeightMap, qe_layout::LayoutError>,
    )>,
    known_results: Receiver<(u64, u64, Result<qe_api::LineLayout, String>)>,
    known_partial: Option<(u64, Arc<qe_api::LineLayout>)>,
    known_inflight: Option<(u64, u64)>,
    long_maps: HashMap<usize, (u64, HeightMap)>,
    map_inflight: HashSet<(usize, u64)>,
    layout_generation: u64,
    segment_inflight: HashSet<(usize, usize)>,
    bounds: Rect,
    caret: ByteOffset,
    anchor: ByteOffset,
    affinity: Affinity,
    scroll: f64,
    scroll_x: f32,
    focused: bool,
    visible: bool,
    dragging: bool,
    drag_point: Option<Point>,
    wrap: bool,
    prevent_editing: bool,
    pending: bool,
    inflight: HashSet<usize>,
    last_click_us: u64,
    click_count: u8,
    preferred_ending: &'static str,
    warning: Option<String>,
    clipboard: Box<dyn Clipboard>,
}

impl EditorView {
    pub fn from_text(text: &str) -> Result<Self, String> {
        let mut view = Self::from_store(PieceStore::new(text))?;
        view.preferred_ending = preferred_line_ending_bytes(text.as_bytes());
        Ok(view)
    }
    pub fn from_store(store: PieceStore) -> Result<Self, String> {
        let (engine, choice) = DirectWriteLayout::with_default_font().map_err(|e| e.to_string())?;
        let engine = Arc::new(engine);
        let (tasks, task_rx) = mpsc::channel::<WorkerTask>();
        let (result_tx, results) = mpsc::channel();
        let (map_tx, map_results) = mpsc::channel();
        let (known_tx, known_results) = mpsc::channel();
        let shared_rx = Arc::new(std::sync::Mutex::new(task_rx));
        for worker in 0..4 {
            let result_tx = result_tx.clone();
            let map_tx = map_tx.clone();
            let known_tx = known_tx.clone();
            let task_rx = shared_rx.clone();
            thread::Builder::new()
                .name(format!("qe-editor-layout-{worker}"))
                .spawn(move || {
                    loop {
                        let next = { task_rx.lock().expect("layout queue lock poisoned").recv() };
                        let Ok(task) = next else {
                            break;
                        };
                        match task {
                            WorkerTask::Layout(ticket, snapshot, engine) => {
                                let result = ticket.layout(&engine, snapshot.as_ref());
                                if result_tx.send((ticket, result)).is_err() {
                                    break;
                                }
                            }
                            WorkerTask::LongMap(line, width, snapshot, engine, generation) => {
                                let result = engine.long_line_map(snapshot.as_ref(), line, width);
                                if map_tx
                                    .send((line, snapshot.revision(), generation, result))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            WorkerTask::KnownFirst(end, width, snapshot, engine, generation) => {
                                let mut result = Err("known text has no UTF-8 edge".to_string());
                                for back in 0..4 {
                                    let Some(last) = end.checked_sub(back) else {
                                        break;
                                    };
                                    let mut text = String::new();
                                    if snapshot
                                        .try_visit_chunks(
                                            ByteOffset(0)..ByteOffset(last),
                                            &mut |part| text.push_str(part),
                                        )
                                        .is_ok()
                                    {
                                        let temporary = PieceStore::new(&text);
                                        result = engine
                                            .try_layout_line(&temporary, LineIndex(0), width)
                                            .map_err(|e| e.to_string());
                                        break;
                                    }
                                }
                                if known_tx
                                    .send((snapshot.revision(), generation, result))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            WorkerTask::Drop(snapshot) => drop(snapshot),
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        let heights =
            HeightMap::new(store.status().known_lines, Dip(17.0)).map_err(|e| e.to_string())?;
        Ok(Self {
            history: UndoHistory::for_store(&store),
            store,
            mapped: None,
            engine,
            cache: RefCell::new(LineCache::new(32 << 20)),
            heights,
            tasks,
            results,
            map_results,
            known_results,
            known_partial: None,
            known_inflight: None,
            long_maps: HashMap::new(),
            map_inflight: HashSet::new(),
            layout_generation: 0,
            segment_inflight: HashSet::new(),
            bounds: Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(800.0),
                height: Dip(600.0),
            },
            caret: ByteOffset(0),
            anchor: ByteOffset(0),
            affinity: Affinity::After,
            scroll: 0.0,
            scroll_x: 0.0,
            focused: false,
            visible: true,
            dragging: false,
            drag_point: None,
            wrap: true,
            prevent_editing: false,
            pending: false,
            inflight: HashSet::new(),
            last_click_us: 0,
            click_count: 0,
            preferred_ending: "\n",
            warning: choice
                .is_fallback()
                .then(|| format!("{} unavailable; using {}", choice.requested, choice.used)),
            clipboard: Box::<MemoryClipboard>::default(),
        })
    }
    /// The caller opens the mapped file on a worker before constructing this view.
    pub fn from_mapped(mapped: MappedStore) -> Result<Self, String> {
        let lines = mapped.status().known_lines.max(1);
        let mut view = Self::from_text("")?;
        view.heights = HeightMap::new(lines, Dip(17.0)).map_err(|e| e.to_string())?;
        view.mapped = Some(mapped);
        Ok(view)
    }
    fn source(&self) -> &dyn TextSnapshot {
        self.mapped
            .as_ref()
            .map_or(&self.store as &dyn TextSnapshot, |s| s as &dyn TextSnapshot)
    }
    fn snapshot(&self) -> Arc<dyn TextSnapshot> {
        match &self.mapped {
            Some(mapped) => Arc::new(NonBlocking(mapped.snapshot())),
            None => self.store.snapshot(),
        }
    }
    pub fn caret(&self) -> ByteOffset {
        self.caret
    }
    pub fn selection(&self) -> Range<ByteOffset> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
    pub fn text(&self) -> Option<String> {
        let mut text = String::new();
        self.source()
            .try_visit_chunks(
                ByteOffset(0)..self.source().status().known_bytes,
                &mut |s| text.push_str(s),
            )
            .ok()?;
        Some(text)
    }
    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.clipboard = clipboard;
    }
    pub fn set_preferred_ending(&mut self, ending: &'static str) {
        self.preferred_ending = ending;
    }
    pub fn pending_layout(&self) -> bool {
        self.pending
    }
    pub fn pending_task_count(&self) -> usize {
        self.inflight.len()
    }
    pub fn scroll_offset(&self) -> f64 {
        self.scroll
    }
    pub fn horizontal_offset(&self) -> f32 {
        self.scroll_x
    }
    pub fn line_count(&self) -> usize {
        self.source().status().known_lines
    }
    pub fn height_run_count(&self) -> usize {
        self.heights.run_count()
    }
    pub fn set_prevent_editing(&mut self, enabled: bool) {
        self.prevent_editing = enabled;
    }
    pub fn cancel_drag(&mut self) {
        self.dragging = false;
        self.drag_point = None;
    }
    pub fn is_at_save_point(&self) -> bool {
        self.mapped.is_none() && self.history.is_at_save_point(&self.store)
    }
    pub fn on_timer(&mut self, ctx: &mut ViewContext) {
        if self.pending || self.mapped.is_some() {
            self.poll_layout(ctx);
        }
        if self.dragging
            && let Some(at) = self.drag_point
        {
            let before = self.scroll;
            if at.y.0 < self.bounds.y.0 {
                self.scroll = (self.scroll - 17.0).max(0.0);
            }
            if at.y.0 > self.bounds.y.0 + self.bounds.height.0 {
                self.scroll = (self.scroll + 17.0)
                    .min((self.heights.total_height() - f64::from(self.bounds.height.0)).max(0.0));
            }
            if self.scroll != before {
                ctx.invalidate(self.bounds);
                if let Some((byte, affinity)) = self.hit(at) {
                    self.move_to(byte, affinity, true, ctx);
                }
                ctx.request_timer(16);
                return;
            }
        }
        if !self.pending && self.focused {
            self.visible = !self.visible;
            self.invalidate_line(self.caret, ctx);
            ctx.request_timer(530);
        }
    }
    pub fn set_wrap(&mut self, enabled: bool) -> Result<(), String> {
        if enabled != self.wrap {
            self.wrap = enabled;
            if enabled {
                self.scroll_x = 0.0;
            }
            self.engine = Arc::new(
                DirectWriteLayout::with_default_font()
                    .map_err(|e| e.to_string())?
                    .0
                    .with_wrap(enabled),
            );
            self.cache.borrow_mut().clear();
            self.long_maps.clear();
            self.layout_generation = self.layout_generation.wrapping_add(1);
            self.known_partial = None;
            self.known_inflight = None;
            self.heights = HeightMap::new(self.source().status().known_lines.max(1), Dip(17.0))
                .map_err(|e| e.to_string())?;
            self.scroll = 0.0;
        }
        Ok(())
    }
    pub fn set_font(&mut self, family: &str, points: f32) -> Result<(), String> {
        self.engine = Arc::new(
            DirectWriteLayout::with_font(
                family,
                points,
                windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
            )
            .map_err(|e| e.to_string())?
            .with_wrap(self.wrap),
        );
        self.cache.borrow_mut().clear();
        self.long_maps.clear();
        self.layout_generation = self.layout_generation.wrapping_add(1);
        self.known_partial = None;
        self.known_inflight = None;
        self.heights = HeightMap::new(self.source().status().known_lines.max(1), Dip(17.0))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn width(&self) -> Dip {
        if self.wrap {
            self.bounds.width
        } else {
            Dip(1_000_000.0)
        }
    }
    pub fn poll_layout(&mut self, ctx: &mut ViewContext) {
        while let Ok((ticket, result)) = self.results.try_recv() {
            if let Some(segment) = ticket.segment() {
                self.segment_inflight.remove(&(ticket.line().0, segment));
            } else {
                self.inflight.remove(&ticket.line().0);
            }
            match result {
                Ok(layout) => {
                    let height = layout.height;
                    let extent = self.engine.segment_extent(&layout);
                    if self.cache.borrow_mut().insert(&ticket, layout) {
                        if let Some(segment) = ticket.segment() {
                            if let Some((_, map)) = self.long_maps.get_mut(&ticket.line().0) {
                                let line_top = self.heights.line_to_scroll(ticket.line());
                                let along = if self.wrap {
                                    (self.scroll - line_top).max(0.0)
                                } else {
                                    f64::from(self.scroll_x)
                                };
                                let first = map.scroll_to_line(along);
                                let before = map.line_to_scroll(first);
                                if let Err(e) = map.set(LineIndex(segment), extent) {
                                    self.warning = Some(e.to_string());
                                }
                                let after = map.line_to_scroll(first);
                                if self.wrap {
                                    if segment < first.0 {
                                        self.scroll += after - before;
                                    }
                                    if let Err(e) = self
                                        .heights
                                        .set(ticket.line(), self.engine.long_line_height(map))
                                    {
                                        self.warning = Some(e.to_string());
                                    }
                                } else if height
                                    > self.heights.line_height(ticket.line()).unwrap_or(Dip(0.0))
                                    && let Err(e) = self.heights.set(ticket.line(), height)
                                {
                                    self.warning = Some(e.to_string());
                                }
                            }
                        } else if !self.long_maps.contains_key(&ticket.line().0)
                            && let Err(e) = self.heights.set(ticket.line(), height)
                        {
                            self.warning = Some(e.to_string());
                        }
                        ctx.invalidate(self.bounds);
                    }
                }
                Err(e) => self.warning = Some(e.to_string()),
            }
        }
        while let Ok((line, revision, generation, result)) = self.map_results.try_recv() {
            self.map_inflight.remove(&(line.0, generation));
            if generation != self.layout_generation || revision != self.source().revision() {
                continue;
            }
            match result {
                Ok(map) => {
                    let height = self.engine.long_line_height(&map);
                    self.long_maps.insert(line.0, (revision, map));
                    if let Err(e) = self.heights.set(line, height) {
                        self.warning = Some(e.to_string());
                    }
                    ctx.invalidate(self.bounds);
                }
                Err(e) => self.warning = Some(e.to_string()),
            }
        }
        while let Ok((revision, generation, result)) = self.known_results.try_recv() {
            if self.known_inflight == Some((revision, generation)) {
                self.known_inflight = None;
            }
            if generation != self.layout_generation || revision != self.source().revision() {
                continue;
            }
            if self
                .source()
                .status()
                .failure
                .as_ref()
                .is_some_and(|f| f.valid_up_to == ByteOffset(0))
            {
                continue;
            }
            match result {
                Ok(layout) => {
                    self.known_partial = Some((revision, Arc::new(layout)));
                    ctx.invalidate(self.bounds);
                }
                Err(e) => self.warning = Some(e),
            }
        }
        self.history.release_memory();
        self.pending = false;
        self.schedule(ctx);
        if self.focused && !self.pending {
            ctx.request_timer(530);
        }
    }
    fn schedule(&mut self, ctx: &mut ViewContext) {
        if let Some(failure) = self.source().status().failure {
            self.warning = Some(failure.message);
        }
        let known_lines = self.source().status().known_lines;
        if known_lines > self.heights.len()
            && let Err(e) = self.heights.insert_lines(
                LineIndex(self.heights.len()),
                known_lines - self.heights.len(),
            )
        {
            self.warning = Some(e.to_string());
        }
        let worker_snapshot = self
            .mapped
            .as_ref()
            .map_or_else(|| self.store.snapshot(), TextStore::snapshot);
        let snapshot: Arc<dyn TextSnapshot> = if self.mapped.is_some() {
            Arc::new(NonBlocking(worker_snapshot.clone()))
        } else {
            worker_snapshot.clone()
        };
        if self.mapped.is_some()
            && let Ok(known) = snapshot.try_known_line(LineIndex(0))
            && !known.complete
            && known.range.end.0 > 0
        {
            if self.known_partial.is_none() && self.known_inflight.is_none() {
                let end = known.range.end.0.min(qe_layout::SEGMENT_BYTES);
                if self
                    .tasks
                    .send(WorkerTask::KnownFirst(
                        end,
                        self.width(),
                        worker_snapshot.clone(),
                        self.engine.clone(),
                        self.layout_generation,
                    ))
                    .is_ok()
                {
                    self.known_inflight = Some((snapshot.revision(), self.layout_generation));
                }
            }
            if self.known_inflight.is_some() {
                self.pending = true;
            }
        }
        let count = (self.bounds.height.0 / 17.0).ceil() as usize;
        let first = self
            .heights
            .scroll_to_line(self.scroll)
            .0
            .saturating_sub(count);
        let last = (first + count * 4 + 3).min(snapshot.status().known_lines);
        for i in first..last {
            let line = LineIndex(i);
            if matches!(self.engine.is_long_line(snapshot.as_ref(), line), Ok(true)) {
                if !self.long_maps.contains_key(&i)
                    && !self.map_inflight.contains(&(i, self.layout_generation))
                    && self
                        .tasks
                        .send(WorkerTask::LongMap(
                            line,
                            self.width(),
                            worker_snapshot.clone(),
                            self.engine.clone(),
                            self.layout_generation,
                        ))
                        .is_ok()
                {
                    self.map_inflight.insert((i, self.layout_generation));
                }
                if self.map_inflight.contains(&(i, self.layout_generation)) {
                    self.pending = true;
                }
                let (segment_start, segment_end) = if let Some((_, map)) = self.long_maps.get(&i) {
                    let line_top = self.heights.line_to_scroll(line);
                    let along = if self.wrap {
                        (self.scroll - line_top).max(0.0)
                    } else {
                        f64::from(self.scroll_x)
                    };
                    let start = map.scroll_to_line(along).0.saturating_sub(1);
                    let limit = along
                        + f64::from(if self.wrap {
                            self.bounds.height.0
                        } else {
                            self.bounds.width.0
                        }) * 2.0;
                    let mut end = start;
                    while end < map.len()
                        && end < start + 128
                        && map.line_to_scroll(LineIndex(end)) <= limit
                    {
                        end += 1;
                    }
                    (start, end.max(start + 1).min(map.len()))
                } else {
                    (0, 1)
                };
                for segment in segment_start..segment_end {
                    if self
                        .cache
                        .borrow_mut()
                        .cached_segment(
                            &self.engine,
                            snapshot.as_ref(),
                            line,
                            segment,
                            self.width(),
                        )
                        .is_some()
                    {
                        continue;
                    }
                    if self.segment_inflight.contains(&(i, segment)) {
                        self.pending = true;
                        continue;
                    }
                    if snapshot.try_line_range(line).is_err() {
                        continue;
                    }
                    match self.cache.borrow_mut().segment_ticket(
                        &self.engine,
                        snapshot.as_ref(),
                        line,
                        segment,
                        self.width(),
                    ) {
                        Ok(ticket) => {
                            if self
                                .tasks
                                .send(WorkerTask::Layout(
                                    ticket,
                                    worker_snapshot.clone(),
                                    self.engine.clone(),
                                ))
                                .is_ok()
                            {
                                self.segment_inflight.insert((i, segment));
                                self.pending = true;
                            }
                        }
                        Err(e) => self.warning = Some(e.to_string()),
                    }
                }
                continue;
            }
            if self
                .cache
                .borrow_mut()
                .cached(&self.engine, snapshot.as_ref(), line, self.width())
                .is_some()
            {
                continue;
            }
            if self.inflight.contains(&i) {
                self.pending = true;
                continue;
            }
            if snapshot.try_line_range(line).is_err() {
                continue;
            }
            match self.cache.borrow_mut().ticket(
                &self.engine,
                snapshot.as_ref(),
                line,
                self.width(),
            ) {
                Ok(ticket) => {
                    if self
                        .tasks
                        .send(WorkerTask::Layout(
                            ticket,
                            worker_snapshot.clone(),
                            self.engine.clone(),
                        ))
                        .is_ok()
                    {
                        self.pending = true;
                        self.inflight.insert(i);
                    }
                }
                Err(e) => self.warning = Some(e.to_string()),
            }
        }
        if self.pending {
            ctx.request_timer(16);
        }
    }
    fn content_end(&self, line: LineIndex) -> Option<ByteOffset> {
        let range = self.source().try_line_range(line).ok()?;
        let ending = self.source().try_line_ending_len(line).ok()?;
        Some(ByteOffset(range.end.0 - ending))
    }
    fn move_to(
        &mut self,
        byte: ByteOffset,
        affinity: Affinity,
        shift: bool,
        ctx: &mut ViewContext,
    ) {
        if byte != self.caret {
            self.history.cursor_moved();
        }
        let old = self.caret;
        self.caret = byte;
        self.affinity = affinity;
        self.visible = true;
        if !shift {
            self.anchor = byte;
        }
        let old_scroll = self.scroll;
        let old_scroll_x = self.scroll_x;
        if let Ok(position) = self.source().try_byte_to_position(byte) {
            let line_top = self.heights.line_to_scroll(position.line);
            let snapshot = self.snapshot();
            let caret_rect = self
                .cache
                .borrow_mut()
                .cached(&self.engine, snapshot.as_ref(), position.line, self.width())
                .and_then(|layout| layout.caret_rect(byte, affinity));
            let top = line_top + caret_rect.map_or(0.0, |rect| f64::from(rect.y.0));
            let bottom = top
                + f64::from(
                    caret_rect
                        .map_or(
                            self.heights.line_height(position.line).unwrap_or(Dip(17.0)),
                            |rect| rect.height,
                        )
                        .0,
                );
            if top < self.scroll {
                self.scroll = top;
            } else if bottom > self.scroll + f64::from(self.bounds.height.0) {
                self.scroll = (bottom - f64::from(self.bounds.height.0)).max(0.0);
            }
            if !self.wrap {
                let x = if let Some((_, map)) = self.long_maps.get(&position.line.0) {
                    self.engine
                        .segment_at(snapshot.as_ref(), byte, affinity)
                        .ok()
                        .map(|(_, segment, range)| {
                            let start = map.line_to_scroll(LineIndex(segment));
                            let end = map.line_to_scroll(LineIndex(segment + 1));
                            self.cache
                                .borrow_mut()
                                .cached_segment(
                                    &self.engine,
                                    snapshot.as_ref(),
                                    position.line,
                                    segment,
                                    self.width(),
                                )
                                .and_then(|layout| {
                                    layout
                                        .caret_rect(byte, affinity)
                                        .map(|rect| start + f64::from(rect.x.0))
                                })
                                .unwrap_or_else(|| {
                                    start
                                        + (end - start) * (byte.0 - range.start.0) as f64
                                            / (range.end.0 - range.start.0).max(1) as f64
                                })
                        })
                } else {
                    caret_rect.map(|rect| f64::from(rect.x.0))
                };
                if let Some(x) = x {
                    if x < f64::from(self.scroll_x) {
                        self.scroll_x = x as f32;
                    } else if x > f64::from(self.scroll_x + self.bounds.width.0 - 2.0) {
                        self.scroll_x = (x - f64::from(self.bounds.width.0) + 2.0).max(0.0) as f32;
                    }
                }
            }
        }
        if self.scroll != old_scroll || self.scroll_x != old_scroll_x {
            ctx.invalidate(self.bounds);
            self.schedule(ctx);
        } else {
            self.invalidate_line(old, ctx);
            self.invalidate_line(byte, ctx);
        }
    }
    fn invalidate_line(&self, byte: ByteOffset, ctx: &mut ViewContext) {
        if let Ok(pos) = self.source().try_byte_to_position(byte) {
            let y =
                self.heights.line_to_scroll(pos.line) - self.scroll + f64::from(self.bounds.y.0);
            ctx.invalidate(Rect {
                x: self.bounds.x,
                y: Dip(y as f32),
                width: self.bounds.width,
                height: self.heights.line_height(pos.line).unwrap_or(Dip(17.0)),
            });
        }
    }
    fn replace(&mut self, value: &str, typing: bool, ctx: &mut ViewContext) {
        if self.prevent_editing || self.mapped.is_some() {
            return;
        }
        let range = self.selection();
        let old = self.store.snapshot();
        let start_line = self
            .source()
            .try_byte_to_position(range.start)
            .map_or(0, |p| p.line.0);
        match self
            .history
            .replace_tracked(&mut self.store, range.clone(), value, typing)
        {
            Ok(change) => {
                self.cache
                    .borrow_mut()
                    .invalidate(old.as_ref(), &change.change);
                if let Err(error) = self.tasks.send(WorkerTask::Drop(old)) {
                    self.warning = Some(format!("snapshot release failed: {error}"));
                }
                if change.change.line_count_delta > 0 {
                    if let Err(e) = self.heights.insert_lines(
                        LineIndex(start_line + 1),
                        change.change.line_count_delta as usize,
                    ) {
                        self.warning = Some(e.to_string());
                    }
                } else if change.change.line_count_delta < 0 {
                    let n = change.change.line_count_delta.unsigned_abs();
                    if let Err(e) = self
                        .heights
                        .remove_lines(start_line + 1..start_line + 1 + n)
                    {
                        self.warning = Some(e.to_string());
                    }
                }
                if change.change.line_count_delta == 0 {
                    if let Some((revision, map)) = self.long_maps.get_mut(&start_line) {
                        match self.engine.update_long_line_map(
                            map,
                            &self.store,
                            LineIndex(start_line),
                        ) {
                            Ok(()) => {
                                *revision = self.store.revision();
                                if let Err(e) = self
                                    .heights
                                    .set(LineIndex(start_line), self.engine.long_line_height(map))
                                {
                                    self.warning = Some(e.to_string());
                                }
                            }
                            Err(e) => self.warning = Some(e.to_string()),
                        }
                    }
                } else {
                    self.long_maps.clear();
                }
                self.caret = ByteOffset(range.start.0 + value.len());
                self.anchor = self.caret;
                self.affinity = Affinity::After;
                self.scroll = self
                    .scroll
                    .min((self.heights.total_height() - f64::from(self.bounds.height.0)).max(0.0));
                if change.change.line_count_delta == 0 {
                    self.invalidate_line(range.start, ctx);
                } else {
                    ctx.invalidate(self.bounds);
                }
                self.schedule(ctx);
            }
            Err(e) => self.warning = Some(format!("edit failed: {e:?}")),
        }
    }
    fn previous_stop(&mut self) -> Option<ByteOffset> {
        let pos = self.source().try_byte_to_position(self.caret).ok()?;
        if self.long_maps.contains_key(&pos.line.0) {
            return self.long_stop(false).or_else(|| {
                pos.line
                    .0
                    .checked_sub(1)
                    .and_then(|i| self.content_end(LineIndex(i)))
            });
        }
        let snapshot = self.snapshot();
        let layout =
            self.cache
                .borrow_mut()
                .cached(&self.engine, snapshot.as_ref(), pos.line, self.width());
        layout
            .and_then(|l| {
                l.rows
                    .iter()
                    .flat_map(|r| r.caret_stops.iter().map(|s| s.byte))
                    .filter(|b| *b < self.caret)
                    .max()
            })
            .or_else(|| self.grapheme_stop(false))
            .or_else(|| {
                pos.line
                    .0
                    .checked_sub(1)
                    .and_then(|i| self.content_end(LineIndex(i)))
            })
    }
    fn next_stop(&mut self) -> Option<ByteOffset> {
        let pos = self.source().try_byte_to_position(self.caret).ok()?;
        if self.long_maps.contains_key(&pos.line.0) {
            return self.long_stop(true).or_else(|| {
                self.source()
                    .try_line_range(LineIndex(pos.line.0 + 1))
                    .ok()
                    .map(|r| r.start)
            });
        }
        let snapshot = self.snapshot();
        let layout =
            self.cache
                .borrow_mut()
                .cached(&self.engine, snapshot.as_ref(), pos.line, self.width());
        layout
            .and_then(|l| {
                l.rows
                    .iter()
                    .flat_map(|r| r.caret_stops.iter().map(|s| s.byte))
                    .filter(|b| *b > self.caret)
                    .min()
            })
            .or_else(|| self.grapheme_stop(true))
            .or_else(|| {
                self.source()
                    .try_line_range(LineIndex(pos.line.0 + 1))
                    .ok()
                    .map(|r| r.start)
            })
    }
    fn long_stop(&mut self, forward: bool) -> Option<ByteOffset> {
        let snapshot = self.snapshot();
        let (line, segment, _) = self
            .engine
            .segment_at(snapshot.as_ref(), self.caret, self.affinity)
            .ok()?;
        let layout = self.cache.borrow_mut().cached_segment(
            &self.engine,
            snapshot.as_ref(),
            line,
            segment,
            self.width(),
        )?;
        let stops = layout
            .rows
            .iter()
            .flat_map(|row| row.caret_stops.iter().map(|stop| stop.byte));
        let found = if forward {
            stops.filter(|byte| *byte > self.caret).min()
        } else {
            stops.filter(|byte| *byte < self.caret).max()
        };
        if found.is_some() {
            return found;
        }
        let neighbor = if forward {
            segment.checked_add(1)?
        } else {
            segment.checked_sub(1)?
        };
        let layout = self.cache.borrow_mut().cached_segment(
            &self.engine,
            snapshot.as_ref(),
            line,
            neighbor,
            self.width(),
        )?;
        let stops = layout
            .rows
            .iter()
            .flat_map(|row| row.caret_stops.iter().map(|stop| stop.byte));
        if forward {
            stops.filter(|byte| *byte > self.caret).min()
        } else {
            stops.filter(|byte| *byte < self.caret).max()
        }
    }
    fn grapheme_stop(&self, forward: bool) -> Option<ByteOffset> {
        let pos = self.source().try_byte_to_position(self.caret).ok()?;
        let range = self.source().try_line_range(pos.line).ok()?;
        if range.end.0 - range.start.0 > 8192 {
            return None;
        }
        let mut text = String::new();
        self.source()
            .try_visit_chunks(range.start..range.end, &mut |s| text.push_str(s))
            .ok()?;
        let at = self.caret.0 - range.start.0;
        let stops = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .filter(|i| {
                !(text.as_bytes().get(i.saturating_sub(1)) == Some(&b'\r')
                    && text.as_bytes().get(*i) == Some(&b'\n'))
            });
        let next = if forward {
            stops.filter(|i| *i > at).min()?
        } else {
            stops.filter(|i| *i < at).max()?
        };
        Some(ByteOffset(range.start.0 + next))
    }
    fn hit(&mut self, at: Point) -> Option<(ByteOffset, Affinity)> {
        let y = (f64::from(at.y.0 - self.bounds.y.0) + self.scroll).max(0.0);
        let line = self.heights.scroll_to_line(y);
        let snapshot = self.snapshot();
        if let Some((_, map)) = self.long_maps.get(&line.0) {
            let line_top = self.heights.line_to_scroll(line);
            let along = if self.wrap {
                (y - line_top).max(0.0)
            } else {
                f64::from((at.x.0 - self.bounds.x.0 + self.scroll_x).max(0.0))
            };
            let segment = map.scroll_to_line(along).0;
            let segment_start = map.line_to_scroll(LineIndex(segment));
            let layout = self.cache.borrow_mut().cached_segment(
                &self.engine,
                snapshot.as_ref(),
                line,
                segment,
                self.width(),
            )?;
            let local = if self.wrap {
                Point {
                    x: Dip(at.x.0 - self.bounds.x.0),
                    y: Dip((along - segment_start) as f32),
                }
            } else {
                Point {
                    x: Dip((along - segment_start) as f32),
                    y: Dip((y - line_top) as f32),
                }
            };
            let result = layout.hit_test_point(local);
            return Some((result.byte, result.affinity));
        }
        let layout =
            self.cache
                .borrow_mut()
                .cached(&self.engine, snapshot.as_ref(), line, self.width())?;
        let top = self.heights.line_to_scroll(line) - self.scroll;
        let result = layout.hit_test_point(Point {
            x: Dip(at.x.0 - self.bounds.x.0 + self.scroll_x),
            y: Dip((f64::from(at.y.0 - self.bounds.y.0) - top) as f32),
        });
        Some((result.byte, result.affinity))
    }
    fn word_range(&self, at: ByteOffset) -> Option<Range<ByteOffset>> {
        let line = self.source().try_byte_to_position(at).ok()?.line;
        let range = self.source().try_line_range(line).ok()?;
        if range.end.0 - range.start.0 > 8192 {
            return None;
        }
        let mut text = String::new();
        self.source()
            .try_visit_chunks(range.start..range.end, &mut |s| text.push_str(s))
            .ok()?;
        let local = at.0 - range.start.0;
        let chars: Vec<_> = text.char_indices().collect();
        let index = chars
            .partition_point(|(i, _)| *i < local)
            .min(chars.len().saturating_sub(1));
        let word = chars.get(index)?.1.is_alphanumeric();
        let mut first = index;
        let mut last = index + 1;
        while first > 0 && chars[first - 1].1.is_alphanumeric() == word {
            first -= 1;
        }
        while last < chars.len() && chars[last].1.is_alphanumeric() == word {
            last += 1;
        }
        Some(
            ByteOffset(range.start.0 + chars[first].0)
                ..ByteOffset(range.start.0 + chars.get(last).map_or(text.len(), |(i, _)| *i)),
        )
    }
    fn vertical(&mut self, amount: isize, shift: bool, ctx: &mut ViewContext) {
        let Ok(position) = self.source().try_byte_to_position(self.caret) else {
            return;
        };
        let snapshot = self.snapshot();
        let layout = self.cache.borrow_mut().cached(
            &self.engine,
            snapshot.as_ref(),
            position.line,
            self.width(),
        );
        if let Some(layout) = layout
            && let Some(caret_rect) = layout.caret_rect(self.caret, self.affinity)
            && let Some(row_index) = layout.rows.iter().position(|row| row.y == caret_rect.y)
        {
            let target_index = row_index.saturating_add_signed(amount);
            if let Some(row) = layout.rows.get(target_index) {
                let hit = row.hit_test_x(caret_rect.x);
                self.move_to(hit.byte, hit.affinity, shift, ctx);
                return;
            }
        }
        let line = position
            .line
            .0
            .saturating_add_signed(amount)
            .min(self.source().status().known_lines.saturating_sub(1));
        let target = self
            .source()
            .try_position_to_byte(qe_api::TextPosition {
                line: LineIndex(line),
                column: position.column,
            })
            .ok()
            .or_else(|| self.content_end(LineIndex(line)));
        if let Some(target) = target {
            self.move_to(target, Affinity::After, shift, ctx);
        }
    }
    fn paragraph(&mut self, down: bool, shift: bool, ctx: &mut ViewContext) {
        let Ok(position) = self.source().try_byte_to_position(self.caret) else {
            return;
        };
        let next = if down {
            position.line.0.checked_add(1)
        } else if let Ok(current) = self.source().try_line_range(position.line) {
            if self.caret > current.start {
                Some(position.line.0)
            } else {
                position.line.0.checked_sub(1)
            }
        } else {
            None
        };
        if let Some(line) = next
            && let Ok(range) = self.source().try_line_range(LineIndex(line))
        {
            self.move_to(range.start, Affinity::After, shift, ctx);
        }
    }
    fn visual_edge(&mut self, home: bool) -> Option<ByteOffset> {
        let position = self.source().try_byte_to_position(self.caret).ok()?;
        let snapshot = self.snapshot();
        let layout = self.cache.borrow_mut().cached(
            &self.engine,
            snapshot.as_ref(),
            position.line,
            self.width(),
        )?;
        let row = layout.rows.iter().enumerate().find_map(|(index, row)| {
            if self.caret < row.range.start || self.caret > row.range.end {
                return None;
            }
            if self.caret == row.range.end
                && self.affinity == Affinity::After
                && let Some(next) = layout.rows.get(index + 1)
                && next.range.start == self.caret
            {
                return Some(next);
            }
            Some(row)
        })?;
        Some(if home { row.range.start } else { row.range.end })
    }
    fn enter(&mut self, ctx: &mut ViewContext) {
        self.replace(self.preferred_ending, true, ctx);
    }
    fn key(&mut self, vk: u16, mods: Modifiers, ctx: &mut ViewContext) -> Handled {
        if mods.ctrl {
            match vk {
                0x41 => {
                    self.anchor = ByteOffset(0);
                    self.move_to(
                        self.source().status().known_bytes,
                        Affinity::After,
                        true,
                        ctx,
                    );
                    return Handled::Yes;
                }
                0x43 | 0x58 => {
                    let mut text = String::new();
                    match self
                        .source()
                        .try_visit_chunks(self.selection(), &mut |s| text.push_str(s))
                    {
                        Ok(()) => match self.clipboard.write(&text) {
                            Ok(()) if vk == 0x58 => self.replace("", false, ctx),
                            Err(e) => self.warning = Some(e),
                            _ => {}
                        },
                        Err(e) => self.warning = Some(format!("copy failed: {e:?}")),
                    }
                    return Handled::Yes;
                }
                0x56 => {
                    match self.clipboard.read() {
                        Ok(text) => self.replace(&text, false, ctx),
                        Err(e) => self.warning = Some(e),
                    };
                    return Handled::Yes;
                }
                0x5a | 0x59 => {
                    if self.mapped.is_some() {
                        return Handled::Yes;
                    }
                    let old = self.snapshot();
                    let answer = if vk == 0x59 || mods.shift {
                        self.history.redo_tracked(&mut self.store)
                    } else {
                        self.history.undo_tracked(&mut self.store)
                    };
                    match answer {
                        Ok(changes) => {
                            let mut new_caret = None;
                            for change in changes {
                                new_caret = Some(ByteOffset(
                                    change.change.replaced.start.0 + change.change.new_len,
                                ));
                                self.cache
                                    .borrow_mut()
                                    .invalidate(old.as_ref(), &change.change);
                            }
                            if let Err(error) = self.tasks.send(WorkerTask::Drop(old)) {
                                self.warning = Some(format!("snapshot release failed: {error}"));
                            }
                            self.heights =
                                HeightMap::new(self.store.status().known_lines, Dip(17.0))
                                    .expect("valid height");
                            self.long_maps.clear();
                            self.layout_generation = self.layout_generation.wrapping_add(1);
                            self.caret = new_caret.unwrap_or(self.caret);
                            self.anchor = self.caret;
                            self.scroll = self.scroll.min(
                                (self.heights.total_height() - f64::from(self.bounds.height.0))
                                    .max(0.0),
                            );
                            ctx.invalidate(self.bounds);
                            self.schedule(ctx);
                        }
                        Err(e) => self.warning = Some(format!("undo failed: {e:?}")),
                    }
                    return Handled::Yes;
                }
                _ => {}
            }
        }
        match vk {
            0x08 | 0x2e => {
                if self.selection().is_empty() {
                    self.anchor = if vk == 0x08 {
                        self.previous_stop()
                    } else {
                        self.next_stop()
                    }
                    .unwrap_or(self.caret);
                }
                self.replace("", vk == 0x08, ctx);
                Handled::Yes
            }
            0x25 | 0x27 => {
                let stop = if mods.ctrl {
                    let mut at = if vk == 0x25 {
                        self.previous_stop()
                    } else {
                        self.next_stop()
                    };
                    if let Some(first) = at
                        && let Some(word) = self.word_range(first)
                    {
                        at = Some(if vk == 0x25 { word.start } else { word.end });
                    }
                    at
                } else if vk == 0x25 {
                    self.previous_stop()
                } else {
                    self.next_stop()
                };
                if let Some(at) = stop {
                    self.move_to(at, Affinity::After, mods.shift, ctx);
                }
                Handled::Yes
            }
            0x26 | 0x28 => {
                if mods.ctrl {
                    self.paragraph(vk == 0x28, mods.shift, ctx);
                } else {
                    self.vertical(if vk == 0x26 { -1 } else { 1 }, mods.shift, ctx);
                }
                Handled::Yes
            }
            0x21 | 0x22 => {
                let page = (self.bounds.height.0 / 17.0).floor().max(1.0) as isize;
                self.vertical(if vk == 0x21 { -page } else { page }, mods.shift, ctx);
                Handled::Yes
            }
            0x24 | 0x23 => {
                let at = if mods.ctrl {
                    if vk == 0x24 {
                        ByteOffset(0)
                    } else {
                        self.source().status().known_bytes
                    }
                } else if let Some(edge) = self.visual_edge(vk == 0x24) {
                    edge
                } else if let Ok(p) = self.source().try_byte_to_position(self.caret) {
                    if vk == 0x24 {
                        self.source()
                            .try_line_range(p.line)
                            .map_or(self.caret, |r| r.start)
                    } else {
                        self.content_end(p.line).unwrap_or(self.caret)
                    }
                } else {
                    self.caret
                };
                self.move_to(
                    at,
                    if vk == 0x23 && !mods.ctrl {
                        Affinity::Before
                    } else {
                        Affinity::After
                    },
                    mods.shift,
                    ctx,
                );
                Handled::Yes
            }
            0x0d => {
                self.enter(ctx);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

fn paint_line_layout(
    list: &mut DisplayList,
    layout: &qe_api::LineLayout,
    theme: &Theme,
    selection: Range<ByteOffset>,
    x: f32,
    y: f32,
    caret: Option<(ByteOffset, Affinity)>,
) {
    for row in &layout.rows {
        if selection.start < row.range.end && selection.end > row.range.start {
            let first = selection.start.max(row.range.start);
            let last = selection.end.min(row.range.end);
            if let (Some(a), Some(b)) = (
                layout.caret_rect(first, Affinity::After),
                layout.caret_rect(last, Affinity::Before),
            ) {
                let left = a.x.0.min(b.x.0);
                let right = a.x.0.max(b.x.0);
                list.push(DrawCommand::FillRect {
                    rect: Rect {
                        x: Dip(x + left),
                        y: Dip(y + row.y.0),
                        width: Dip((right - left).max(1.0)),
                        height: row.height,
                    },
                    color: qe_api::Color {
                        a: 64,
                        ..theme.text
                    },
                });
            }
        }
        for run in &row.glyph_runs {
            list.push(DrawCommand::GlyphRun {
                at: Point {
                    x: Dip(x + run.origin.x.0),
                    y: Dip(y + run.origin.y.0),
                },
                bidi_level: run.bidi_level,
                font_id: run.font_id,
                font_em_size: run.font_em_size,
                glyph_ids: run.glyph_ids.clone(),
                advances: run.advances.clone(),
                offsets: run.offsets.clone(),
                color: theme.text,
            });
        }
    }
    if let Some((byte, affinity)) = caret
        && let Some(rect) = layout.caret_rect(byte, affinity)
    {
        list.push(DrawCommand::FillRect {
            rect: Rect {
                x: Dip(x + rect.x.0),
                y: Dip(y + rect.y.0),
                width: rect.width,
                height: rect.height,
            },
            color: theme.text,
        });
    }
}

impl View for EditorView {
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        match &event.kind {
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                ..
            } => self.key(*virtual_key, *modifiers, ctx),
            InputKind::Char(c) if !c.is_control() || *c == '\t' => {
                self.replace(&c.to_string(), true, ctx);
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers,
            } => {
                self.click_count = if event.time_us.saturating_sub(self.last_click_us) <= 500_000 {
                    self.click_count % 3 + 1
                } else {
                    1
                };
                self.last_click_us = event.time_us;
                if let Some((byte, affinity)) = self.hit(*at) {
                    self.move_to(byte, affinity, modifiers.shift, ctx);
                    if self.click_count == 2
                        && let Some(range) = self.word_range(byte)
                    {
                        self.anchor = range.start;
                        self.caret = range.end;
                        self.invalidate_line(byte, ctx);
                    }
                    if self.click_count == 3
                        && let Ok(position) = self.source().try_byte_to_position(byte)
                        && let Ok(range) = self.source().try_line_range(position.line)
                    {
                        self.anchor = range.start;
                        self.caret = range.end;
                        self.invalidate_line(byte, ctx);
                    }
                }
                self.dragging = true;
                self.drag_point = Some(*at);
                ctx.capture_mouse(true);
                ctx.set_cursor(MouseCursor::IBeam);
                Handled::Yes
            }
            InputKind::MouseMove { at } if self.dragging => {
                self.drag_point = Some(*at);
                if at.y.0 < self.bounds.y.0 {
                    self.scroll = (self.scroll - 17.0).max(0.0);
                    ctx.invalidate(self.bounds);
                }
                if at.y.0 > self.bounds.y.0 + self.bounds.height.0 {
                    self.scroll = (self.scroll + 17.0).min(
                        (self.heights.total_height() - f64::from(self.bounds.height.0)).max(0.0),
                    );
                    ctx.invalidate(self.bounds);
                }
                if let Some((byte, affinity)) = self.hit(*at) {
                    self.move_to(byte, affinity, true, ctx);
                }
                Handled::Yes
            }
            InputKind::MouseUp {
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = false;
                self.drag_point = None;
                ctx.capture_mouse(false);
                Handled::Yes
            }
            InputKind::Wheel {
                delta_x, delta_y, ..
            } => {
                if !self.wrap {
                    self.scroll_x = (self.scroll_x + *delta_x * 24.0 / 120.0).max(0.0);
                }
                self.scroll = (self.scroll - f64::from(*delta_y) * 17.0 * 3.0 / 120.0).clamp(
                    0.0,
                    (self.heights.total_height() - f64::from(self.bounds.height.0)).max(0.0),
                );
                ctx.invalidate(self.bounds);
                self.schedule(ctx);
                Handled::Yes
            }
            InputKind::Resize { width, height } => {
                self.bounds.width = *width;
                self.bounds.height = *height;
                self.cache.borrow_mut().clear();
                self.long_maps.clear();
                self.layout_generation = self.layout_generation.wrapping_add(1);
                self.heights = HeightMap::new(self.source().status().known_lines.max(1), Dip(17.0))
                    .expect("valid height");
                self.schedule(ctx);
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Focus => {
                self.focused = true;
                self.visible = true;
                self.invalidate_line(self.caret, ctx);
                ctx.request_timer(530);
                Handled::Yes
            }
            InputKind::Blur => {
                self.focused = false;
                self.visible = false;
                self.cancel_drag();
                self.invalidate_line(self.caret, ctx);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, _measure: &dyn TextMeasurer) {
        list.push(DrawCommand::FillRect {
            rect: self.bounds,
            color: theme.text_background,
        });
        let snapshot = self.snapshot();
        let first = self.heights.scroll_to_line(self.scroll).0;
        let end = (first + (self.bounds.height.0 / 17.0).ceil() as usize + 2)
            .min(snapshot.status().known_lines);
        for i in first..end {
            let line = LineIndex(i);
            if let Some((_, map)) = self.long_maps.get(&i) {
                let line_top = self.heights.line_to_scroll(line);
                let along = if self.wrap {
                    (self.scroll - line_top).max(0.0)
                } else {
                    f64::from(self.scroll_x)
                };
                let start = map.scroll_to_line(along).0;
                let limit = along
                    + f64::from(if self.wrap {
                        self.bounds.height.0
                    } else {
                        self.bounds.width.0
                    }) * 2.0;
                let caret_segment = if self.focused
                    && self.visible
                    && snapshot
                        .try_byte_to_position(self.caret)
                        .ok()
                        .is_some_and(|p| p.line == line)
                {
                    self.engine
                        .segment_at(snapshot.as_ref(), self.caret, self.affinity)
                        .ok()
                        .map(|(_, segment, _)| segment)
                } else {
                    None
                };
                for segment in start..map.len().min(start + 128) {
                    let offset = map.line_to_scroll(LineIndex(segment));
                    if offset > limit {
                        break;
                    }
                    let Some(layout) = self.cache.borrow_mut().cached_segment(
                        &self.engine,
                        snapshot.as_ref(),
                        line,
                        segment,
                        self.width(),
                    ) else {
                        continue;
                    };
                    let x = if self.wrap {
                        self.bounds.x.0
                    } else {
                        self.bounds.x.0 + offset as f32 - self.scroll_x
                    };
                    let y = if self.wrap {
                        self.bounds.y.0 + (line_top + offset - self.scroll) as f32
                    } else {
                        self.bounds.y.0 + (line_top - self.scroll) as f32
                    };
                    let caret =
                        (caret_segment == Some(segment)).then_some((self.caret, self.affinity));
                    paint_line_layout(list, &layout, theme, self.selection(), x, y, caret);
                }
                continue;
            }
            if matches!(self.engine.is_long_line(snapshot.as_ref(), line), Ok(true)) {
                if let Some(layout) = self.cache.borrow_mut().cached_segment(
                    &self.engine,
                    snapshot.as_ref(),
                    line,
                    0,
                    self.width(),
                ) {
                    let top =
                        self.bounds.y.0 + (self.heights.line_to_scroll(line) - self.scroll) as f32;
                    paint_line_layout(
                        list,
                        &layout,
                        theme,
                        self.selection(),
                        self.bounds.x.0 - self.scroll_x,
                        top,
                        None,
                    );
                }
                continue;
            }
            let Some(layout) =
                self.cache
                    .borrow_mut()
                    .cached(&self.engine, snapshot.as_ref(), line, self.width())
            else {
                if i == 0
                    && let Some((_, layout)) = &self.known_partial
                    && !snapshot
                        .status()
                        .failure
                        .as_ref()
                        .is_some_and(|f| f.valid_up_to == ByteOffset(0))
                {
                    let top = self.bounds.y.0 - self.scroll as f32;
                    paint_line_layout(
                        list,
                        layout,
                        theme,
                        self.selection(),
                        self.bounds.x.0 - self.scroll_x,
                        top,
                        None,
                    );
                }
                continue;
            };
            let top = (self.heights.line_to_scroll(line) - self.scroll) as f32 + self.bounds.y.0;
            let selection = self.selection();
            for row in &layout.rows {
                if selection.start < row.range.end && selection.end > row.range.start {
                    let first = selection.start.max(row.range.start);
                    let last = selection.end.min(row.range.end);
                    if let (Some(a), Some(b)) = (
                        layout.caret_rect(first, Affinity::After),
                        layout.caret_rect(last, Affinity::Before),
                    ) {
                        let left = a.x.0.min(b.x.0);
                        let right = a.x.0.max(b.x.0);
                        list.push(DrawCommand::FillRect {
                            rect: Rect {
                                x: Dip(self.bounds.x.0 + left - self.scroll_x),
                                y: Dip(top + row.y.0),
                                width: Dip((right - left).max(1.0)),
                                height: row.height,
                            },
                            color: qe_api::Color {
                                a: 64,
                                ..theme.text
                            },
                        });
                    }
                }
                for run in &row.glyph_runs {
                    list.push(DrawCommand::GlyphRun {
                        at: Point {
                            x: Dip(self.bounds.x.0 + run.origin.x.0 - self.scroll_x),
                            y: Dip(top + run.origin.y.0),
                        },
                        bidi_level: run.bidi_level,
                        font_id: run.font_id,
                        font_em_size: run.font_em_size,
                        glyph_ids: run.glyph_ids.clone(),
                        advances: run.advances.clone(),
                        offsets: run.offsets.clone(),
                        color: theme.text,
                    });
                }
            }
            if self.focused
                && self.visible
                && let Ok(p) = snapshot.try_byte_to_position(self.caret)
                && p.line == line
                && let Some(rect) = layout.caret_rect(self.caret, self.affinity)
            {
                list.push(DrawCommand::FillRect {
                    rect: Rect {
                        x: Dip(self.bounds.x.0 + rect.x.0 - self.scroll_x),
                        y: Dip(top + rect.y.0),
                        width: rect.width,
                        height: rect.height,
                    },
                    color: theme.text,
                });
            }
        }
        if let Some(warning) = &self.warning {
            list.push(DrawCommand::TextRun {
                at: Point {
                    x: Dip(self.bounds.x.0 + 8.0),
                    y: Dip(self.bounds.y.0 + 16.0),
                },
                text: warning.clone(),
                font: self.engine.font_family().into(),
                font_size: Dip(theme.font_pt * 96.0 / 72.0),
                color: theme.warning,
            });
        }
    }
    fn layout(&mut self, bounds: Rect, _measure: &dyn TextMeasurer) {
        if bounds.width != self.bounds.width {
            self.cache.borrow_mut().clear();
            self.long_maps.clear();
            self.layout_generation = self.layout_generation.wrapping_add(1);
            self.heights = HeightMap::new(self.source().status().known_lines.max(1), Dip(17.0))
                .expect("valid height");
        }
        self.bounds = bounds;
    }
    fn widgets(&self) -> Option<Widget> {
        Some(Widget {
            id: WidgetId(900),
            bounds: self.bounds,
            role: WidgetRole::TextField,
            name: "Editor".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                focused: self.focused,
                read_only: self.prevent_editing || self.mapped.is_some(),
                ..Default::default()
            },
            children: vec![],
        })
    }
}
