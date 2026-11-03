//! What the tests share: a private folder, a store whose wakes are counted, and a pump.
#![allow(dead_code)]
use qe_api::{ByteOffset, InputEvent, InputKind, View, ViewContext};
use qe_docs::{DocEvent, DocId, DocumentStore, Question, StoreConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const WAIT: Duration = Duration::from_secs(30);

pub fn test_folder(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("qe-docs")
        .join(name);
    if folder.exists() {
        // A read-only mark from a stopped test must not stop the clean-up.
        clear_read_only(&folder);
        std::fs::remove_dir_all(&folder).expect("clear the test folder");
    }
    std::fs::create_dir_all(&folder).expect("make the test folder");
    folder
}

fn clear_read_only(folder: &Path) {
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            clear_read_only(&path);
        } else if let Ok(meta) = std::fs::metadata(&path) {
            let mut permissions = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
    }
}

pub struct Harness {
    pub store: DocumentStore,
    pub events: Vec<DocEvent>,
    pub wakes: Arc<AtomicU64>,
    pub root: PathBuf,
}

/// A config with its own root, its watch off, and no autosave until a test asks.
pub fn config(root: &Path) -> (StoreConfig, Arc<AtomicU64>) {
    let wakes = Arc::new(AtomicU64::new(0));
    let counter = wakes.clone();
    let wake: qe_docs::Wake = Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    let mut config = StoreConfig::new(root, wake);
    config.watch_folders = false;
    config.session_delay = Duration::from_millis(1);
    (config, wakes)
}

impl Harness {
    pub fn new(root: &Path) -> Self {
        let (config, wakes) = config(root);
        Self::with_config(config, wakes)
    }
    pub fn with_config(config: StoreConfig, wakes: Arc<AtomicU64>) -> Self {
        let root = config.root.clone();
        let mut harness = Self {
            store: DocumentStore::new(config),
            events: Vec::new(),
            wakes,
            root,
        };
        harness.store.begin_restore();
        harness.until("the restore ends", |h| {
            h.events
                .iter()
                .any(|e| matches!(e, DocEvent::RestoreFinished { .. }))
        });
        harness
    }
    /// One round of what the host does on a wake.
    pub fn pump(&mut self) {
        self.store.poll(Instant::now());
        self.events.extend(self.store.take_events());
    }
    /// Pumps until `done`, and fails the test by name after the wait.
    pub fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let end = Instant::now() + WAIT;
        loop {
            self.pump();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < end,
                "gave up waiting: {what}; events: {:#?}",
                self.events
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    /// Pumps for a while with no aim, to show that something does not happen.
    pub fn settle(&mut self, time: Duration) {
        let end = Instant::now() + time;
        while Instant::now() < end {
            self.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    pub fn loaded(&mut self, doc: DocId) {
        self.until("the document loads", |h| !h.store.is_loading(doc));
    }
    pub fn open(&mut self, path: &Path) -> DocId {
        let opened = self
            .store
            .open_paths(
                &[path.to_path_buf()],
                qe_docs::OpenTarget::LastUsedWindow,
                None,
            )
            .unwrap();
        let doc = opened[0];
        self.loaded(doc);
        doc
    }
    pub fn open_with(&mut self, path: &Path, choice: qe_api::EncodingChoice) -> DocId {
        let opened = self
            .store
            .open_paths(
                &[path.to_path_buf()],
                qe_docs::OpenTarget::LastUsedWindow,
                Some(choice),
            )
            .unwrap();
        let doc = opened[0];
        self.loaded(doc);
        doc
    }
    /// Real key order: each character goes through the view's input queue.
    pub fn type_text(&mut self, doc: DocId, text: &str) {
        let mut ctx = ViewContext::default();
        for character in text.chars() {
            let view = self.store.view_mut(doc).unwrap();
            view.handle(
                &InputEvent {
                    time_us: 0,
                    kind: InputKind::Char(character),
                },
                &mut ctx,
            );
            self.store.note_view_changed(doc);
        }
    }
    pub fn undo(&mut self, doc: DocId) {
        let mut ctx = ViewContext::default();
        let view = self.store.view_mut(doc).unwrap();
        view.run(qe_editor::EditorCommand::Undo, &mut ctx);
        self.store.note_view_changed(doc);
    }
    pub fn text(&self, doc: DocId) -> String {
        self.store.view(doc).unwrap().text().unwrap()
    }
    pub fn caret_to_end(&mut self, doc: DocId) {
        let mut ctx = ViewContext::default();
        let view = self.store.view_mut(doc).unwrap();
        let end = view.store_status().known_bytes;
        view.select(end..end, &mut ctx);
    }
    pub fn select(&mut self, doc: DocId, from: usize, to: usize) {
        let mut ctx = ViewContext::default();
        let view = self.store.view_mut(doc).unwrap();
        view.select_directed(ByteOffset(from), ByteOffset(to), &mut ctx);
    }
    /// The first open question of a kind, waiting for it if need be.
    pub fn question(&mut self, what: &str) -> Question {
        self.until(what, |h| !h.store.open_questions().is_empty());
        self.store.open_questions().remove(0)
    }
    /// Pumps until no timer stands and no copy is being written.
    pub fn quiet(&mut self) {
        self.until("everything settles", |h| {
            let ids = h.store.all_documents();
            h.store.next_deadline().is_none() && !ids.iter().any(|d| h.store.is_autosaving(*d))
        });
    }
    pub fn saved(&mut self, doc: DocId) {
        self.until("the save ends", |h| {
            h.events
                .iter()
                .any(|e| matches!(e, DocEvent::SaveFinished { doc: d, .. } if *d == doc))
        });
    }
    pub fn clear_events(&mut self) {
        self.events.clear();
    }
    pub fn count(&self, test: impl Fn(&DocEvent) -> bool) -> usize {
        self.events.iter().filter(|e| test(e)).count()
    }
    /// Runs the quit to its end and gives the folder up.
    pub fn quit(mut self) {
        self.store.begin_quit();
        self.until("the quit ends", |h| {
            h.events.iter().any(|e| matches!(e, DocEvent::ReadyToQuit))
        });
    }
}

pub fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

pub fn set_read_only(path: &Path, on: bool) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(on);
    std::fs::set_permissions(path, permissions).unwrap();
}

/// The names under the session's document folder.
pub fn autosave_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(root.join("session").join("docs")) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}
