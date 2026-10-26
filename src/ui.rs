//! The same Ratatui renderer powers the live interface and headless SVG previews.
use crate::{
    collection::{self, Collection},
    model::*,
};
use anyhow::{Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use indexly::{
    QueryMode,
    tokenizer::{SimpleTokenizer, Tokenizer},
};
use ratatui::{
    Frame, Terminal,
    backend::TestBackend,
    buffer::Buffer,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Gauge, List, ListItem, ListState, Paragraph, Widget, Wrap},
};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use unicode_width::UnicodeWidthStr;

// The classic desktop palette: teal wallpaper, silver chrome, navy selections.
const BG: Color = Color::Rgb(0, 128, 128);
const PANEL: Color = Color::Rgb(192, 192, 192);
const PAPER: Color = Color::Rgb(255, 255, 255);
const BORDER: Color = Color::Rgb(128, 128, 128);
const SHADOW: Color = Color::Rgb(64, 64, 64);
const TEXT: Color = Color::Rgb(0, 0, 0);
const MUTED: Color = Color::Rgb(64, 64, 64);
const DIM: Color = Color::Rgb(96, 96, 96);
const ACCENT: Color = Color::Rgb(0, 0, 128);
const SECONDARY: Color = Color::Rgb(128, 0, 128);
const SELECTION: Color = Color::Rgb(0, 0, 128);
const MATCH: Color = Color::Rgb(255, 255, 128);
const RED: Color = Color::Rgb(128, 0, 0);

pub enum Launch {
    Browse(Option<String>),
    Build { source: PathBuf, name: String },
    Refresh(String),
}

enum Job {
    Open(String),
    Search {
        query: String,
        mode: QueryMode,
        request: u64,
    },
    Build {
        source: PathBuf,
        name: String,
        replace: bool,
        cancel: Arc<AtomicBool>,
    },
    Delete(String),
    Stop,
}
enum Update {
    Opened(std::result::Result<Manifest, String>),
    Searched(u64, std::result::Result<Found, String>),
    Progress(Progress),
    Built(std::result::Result<Manifest, String>),
    Deleted(String, std::result::Result<(), String>),
}

fn worker(home: PathBuf, jobs: mpsc::Receiver<Job>, updates: mpsc::Sender<Update>) {
    let mut current: Option<Collection> = None;
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Stop => break,
            Job::Open(name) => {
                current = None;
                let opened = Collection::open(&home, &name)
                    .map(|c| {
                        let manifest = c.manifest.clone();
                        current = Some(c);
                        manifest
                    })
                    .map_err(|e| format!("{e:#}"));
                let _ = updates.send(Update::Opened(opened));
            }
            Job::Search {
                query,
                mode,
                request,
            } => {
                let result = current
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Choose a collection first"))
                    .and_then(|c| c.search(&query, mode))
                    .map_err(|e| format!("{e:#}"));
                let _ = updates.send(Update::Searched(request, result));
            }
            Job::Build {
                source,
                name,
                replace,
                cancel,
            } => {
                current = None; // Release the reader lock before replacing this collection.
                let result = collection::build(&home, &name, &source, replace, &mut |progress| {
                    if cancel.load(Ordering::Relaxed) {
                        bail!("Import cancelled");
                    }
                    updates.send(Update::Progress(progress)).map_err(Into::into)
                })
                .and_then(|m| {
                    current = Some(Collection::open(&home, &name)?);
                    Ok(m)
                })
                .map_err(|e| format!("{e:#}"));
                let _ = updates.send(Update::Built(result));
            }
            Job::Delete(name) => {
                let was_active = current.as_ref().is_some_and(|c| c.manifest.name == name);
                if was_active {
                    current = None;
                }
                let result = collection::remove(&home, &name).map_err(|e| format!("{e:#}"));
                if result.is_err() && was_active {
                    current = Collection::open(&home, &name).ok();
                }
                let _ = updates.send(Update::Deleted(name, result));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Search,
    Results,
    Preview,
    Library,
    Info,
}

#[derive(Default)]
struct Input {
    value: String,
    cursor: usize,
}
impl Input {
    fn new(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor }
    }
    fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control() =>
            {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.value[..self.cursor]
                    .char_indices()
                    .last()
                    .map_or(0, |(i, _)| i);
                self.value.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.value.len() => {
                let next =
                    self.cursor + self.value[self.cursor..].chars().next().unwrap().len_utf8();
                self.value.drain(self.cursor..next);
            }
            KeyCode::Left => {
                self.cursor = self.value[..self.cursor]
                    .char_indices()
                    .last()
                    .map_or(0, |(i, _)| i);
            }
            KeyCode::Right if self.cursor < self.value.len() => {
                self.cursor += self.value[self.cursor..].chars().next().unwrap().len_utf8();
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            _ => {}
        }
    }
    fn paste(&mut self, text: &str) {
        let clean = collection::safe_text(text);
        self.value.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }
    fn visible(&self, width: usize) -> (String, usize) {
        let mut start = 0;
        while self.value[start..self.cursor].width() >= width.max(1) && start < self.cursor {
            start += self.value[start..].chars().next().unwrap().len_utf8();
        }
        (
            clip(&self.value[start..], width),
            self.value[start..self.cursor].width(),
        )
    }
}

struct ImportDialog {
    source: Input,
    name: Input,
    name_focus: bool,
    auto_name: bool,
}
enum Modal {
    Import(ImportDialog),
    Help,
    Issues,
    Delete { name: String, confirm: bool },
}

struct App {
    home: PathBuf,
    libraries: Vec<Manifest>,
    library_index: usize,
    active: Option<Manifest>,
    query: Input,
    found: Option<Found>,
    hit_index: usize,
    preview_scroll: u16,
    focus: Focus,
    info_action: usize,
    mode: QueryMode,
    request: u64,
    busy: bool,
    deleting: Option<String>,
    progress: Option<Progress>,
    cancel: Option<Arc<AtomicBool>>,
    quitting: bool,
    modal: Option<Modal>,
    message: String,
    error: bool,
    tick: usize,
}

impl App {
    fn new(home: PathBuf) -> Result<Self> {
        let libraries = collection::list(&home)?;
        Ok(Self {
            home,
            libraries,
            library_index: 0,
            active: None,
            query: Input::default(),
            found: None,
            hit_index: 0,
            preview_scroll: 0,
            focus: Focus::Search,
            info_action: 1,
            mode: QueryMode::Disjunctive,
            request: 0,
            busy: false,
            deleting: None,
            progress: None,
            cancel: None,
            quitting: false,
            modal: None,
            message: "Your documents. Your machine. Your search.".into(),
            error: false,
            tick: 0,
        })
    }
    fn status(&mut self, message: impl Into<String>, error: bool) {
        self.message = message.into();
        self.error = error;
    }
    fn open(&mut self, name: String, jobs: &mpsc::Sender<Job>) -> Result<()> {
        self.busy = true;
        self.request += 1;
        self.found = None;
        self.hit_index = 0;
        self.active = None;
        jobs.send(Job::Open(name))?;
        Ok(())
    }
    fn search(&mut self, jobs: &mpsc::Sender<Job>) -> Result<()> {
        if self.active.is_none() {
            self.status("Add or choose a collection to start searching.", true);
            return Ok(());
        }
        self.request += 1;
        self.busy = true;
        self.hit_index = 0;
        self.preview_scroll = 0;
        jobs.send(Job::Search {
            query: self.query.value.clone(),
            mode: self.mode,
            request: self.request,
        })?;
        Ok(())
    }
    fn build(
        &mut self,
        source: PathBuf,
        name: String,
        replace: bool,
        jobs: &mpsc::Sender<Job>,
    ) -> Result<()> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.busy = true;
        self.request += 1;
        self.found = None;
        self.active = None;
        self.modal = None;
        self.progress = Some(Progress {
            stage: "Preparing collection".into(),
            detail: source.display().to_string(),
            done: 0,
            total: 0,
        });
        jobs.send(Job::Build {
            source,
            name,
            replace,
            cancel,
        })?;
        Ok(())
    }
    fn hit(&self) -> Option<&Hit> {
        self.found.as_ref()?.hits.get(self.hit_index)
    }
    fn info_collection(&self) -> Option<&Manifest> {
        if matches!(self.focus, Focus::Library | Focus::Info) {
            self.libraries.get(self.library_index)
        } else {
            self.active
                .as_ref()
                .or_else(|| self.libraries.get(self.library_index))
        }
    }
    fn request_delete(&mut self) {
        if let Some(collection) = self.info_collection() {
            self.modal = Some(Modal::Delete {
                name: collection.name.clone(),
                confirm: false,
            });
        } else {
            self.status("Choose a collection to delete.", true);
        }
    }
    fn delete(&mut self, name: String, jobs: &mpsc::Sender<Job>) -> Result<()> {
        self.modal = None;
        self.request += 1;
        self.busy = true;
        self.deleting = Some(name.clone());
        jobs.send(Job::Delete(name))?;
        Ok(())
    }
    fn info_enter(&mut self, jobs: &mpsc::Sender<Job>) -> Result<()> {
        match self.info_action {
            0 => {
                if let Some(collection) = self.info_collection() {
                    self.build(
                        collection.source.clone(),
                        collection.name.clone(),
                        true,
                        jobs,
                    )?;
                }
            }
            1 => self.modal = Some(Modal::Issues),
            _ => self.request_delete(),
        }
        Ok(())
    }
    fn toggle_mode(&mut self, jobs: &mpsc::Sender<Job>) -> Result<()> {
        self.mode = if self.mode == QueryMode::Conjunctive {
            QueryMode::Disjunctive
        } else {
            QueryMode::Conjunctive
        };
        if !self.query.value.is_empty() {
            self.search(jobs)?;
        } else {
            self.status(format!("Search mode: {}", self.mode.label()), false);
        }
        Ok(())
    }
    fn receive(&mut self, update: Update, jobs: &mpsc::Sender<Job>) -> Result<bool> {
        match update {
            Update::Progress(progress) => self.progress = Some(progress),
            Update::Opened(result) => {
                self.busy = false;
                match result {
                    Ok(manifest) => {
                        self.library_index = self
                            .libraries
                            .iter()
                            .position(|m| m.name == manifest.name)
                            .unwrap_or(0);
                        self.status(
                            format!(
                                "{} documents ready · enter a few words to search",
                                manifest.files
                            ),
                            false,
                        );
                        self.active = Some(manifest);
                        if !self.query.value.is_empty() {
                            self.search(jobs)?;
                        }
                    }
                    Err(error) => self.status(error, true),
                }
            }
            Update::Searched(request, result) if request == self.request => {
                self.busy = false;
                match result {
                    Ok(found) => {
                        self.status(
                            format!(
                                "{} matching passages · {} ms",
                                found.matched_passages, found.elapsed_ms
                            ),
                            false,
                        );
                        self.found = Some(found);
                    }
                    Err(error) => self.status(error, true),
                }
            }
            Update::Searched(_, _) => {}
            Update::Deleted(name, result) => {
                self.busy = false;
                self.deleting = None;
                match result {
                    Ok(()) => {
                        let was_active = self.active.as_ref().is_some_and(|m| m.name == name);
                        self.libraries = collection::list(&self.home)?;
                        self.library_index = self
                            .library_index
                            .min(self.libraries.len().saturating_sub(1));
                        if was_active {
                            self.active = None;
                            self.found = None;
                            self.hit_index = 0;
                            self.preview_scroll = 0;
                            if let Some(next) = self.libraries.get(self.library_index) {
                                self.open(next.name.clone(), jobs)?;
                            } else {
                                self.query = Input::default();
                                self.focus = Focus::Library;
                            }
                        }
                        self.status(
                            format!("Deleted index '{name}'. Source documents remain in place."),
                            false,
                        );
                    }
                    Err(error) => self.status(error, true),
                }
            }
            Update::Built(result) => {
                self.busy = false;
                self.progress = None;
                self.cancel = None;
                self.libraries = collection::list(&self.home)?;
                match result {
                    Ok(manifest) => {
                        self.library_index = self
                            .libraries
                            .iter()
                            .position(|m| m.name == manifest.name)
                            .unwrap_or(0);
                        self.status(format!("Indexed {} documents · {} passages · {} import issues (w to inspect)", manifest.files, manifest.passages, manifest.issues.len()), false);
                        self.active = Some(manifest);
                        self.focus = Focus::Search;
                        if !self.quitting && !self.query.value.is_empty() {
                            self.search(jobs)?;
                        }
                    }
                    Err(error) => self.status(error, true),
                }
                if self.quitting {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn key(&mut self, key: KeyEvent, jobs: &mpsc::Sender<Job>) -> Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('c'))
        {
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::Relaxed);
                self.quitting = true;
                self.status("Cancelling import safely…", false);
                return Ok(false);
            }
            return Ok(true);
        }
        if self.progress.is_some() || self.deleting.is_some() {
            return Ok(false);
        }
        if key.code == KeyCode::F(1) {
            self.modal = Some(Modal::Help);
            return Ok(false);
        }
        if self.modal.is_some() {
            return self.modal_key(key, jobs);
        }
        if key.code == KeyCode::F(7) {
            self.toggle_mode(jobs)?;
            return Ok(false);
        }
        let direct = match key.code {
            KeyCode::F(2) => Some(Focus::Search),
            KeyCode::F(3) => Some(Focus::Results),
            KeyCode::F(4) => Some(Focus::Preview),
            KeyCode::F(5) => Some(Focus::Library),
            KeyCode::F(6) => Some(Focus::Info),
            _ => None,
        };
        if let Some(focus) = direct {
            self.focus = focus;
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') => self.add_dialog(),
                KeyCode::Char('l') => self.focus = Focus::Library,
                KeyCode::Char('f') => self.focus = Focus::Search,
                KeyCode::Char('e') => self.focus = Focus::Results,
                KeyCode::Char('p') => self.focus = Focus::Preview,
                KeyCode::Char('g') => self.focus = Focus::Info,
                KeyCode::Char('d') => self.request_delete(),
                KeyCode::Char('m' | 'o') => self.toggle_mode(jobs)?,
                _ if self.focus == Focus::Search => self.query.key(key),
                _ => {}
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Search => Focus::Results,
                    Focus::Results => Focus::Preview,
                    Focus::Preview => Focus::Library,
                    Focus::Library => Focus::Info,
                    Focus::Info => Focus::Search,
                }
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Search => Focus::Info,
                    Focus::Info => Focus::Library,
                    Focus::Library => Focus::Preview,
                    Focus::Preview => Focus::Results,
                    Focus::Results => Focus::Search,
                }
            }
            KeyCode::Esc => self.focus = Focus::Results,
            KeyCode::Enter if self.focus == Focus::Search => {
                self.search(jobs)?;
                self.focus = Focus::Results;
            }
            KeyCode::Down if self.focus == Focus::Search => self.focus = Focus::Results,
            _ if self.focus == Focus::Search => self.query.key(key),
            KeyCode::Char('/') => self.focus = Focus::Search,
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('a') => self.add_dialog(),
            KeyCode::Char('w') => self.modal = Some(Modal::Issues),
            KeyCode::Char('d') => self.request_delete(),
            KeyCode::Delete if matches!(self.focus, Focus::Library | Focus::Info) => {
                self.request_delete()
            }
            KeyCode::Char('r') => {
                if let Some(active) = self.info_collection() {
                    self.build(active.source.clone(), active.name.clone(), true, jobs)?;
                }
            }
            KeyCode::Char('c') => {
                if let Some(hit) = self.hit() {
                    let result = collection::copy_text(&hit.text);
                    match result {
                        Ok(()) => self.status("Passage copied to clipboard", false),
                        Err(e) => self.status(format!("{e:#}"), true),
                    }
                }
            }
            KeyCode::Enter if self.focus == Focus::Library => {
                if let Some(manifest) = self.libraries.get(self.library_index) {
                    self.open(manifest.name.clone(), jobs)?;
                    self.focus = Focus::Search;
                }
            }
            KeyCode::Enter if self.focus == Focus::Info => self.info_enter(jobs)?,
            KeyCode::Enter | KeyCode::Char('o') => {
                if let Some(hit) = self.hit() {
                    let result = collection::open_hit(hit);
                    match result {
                        Ok(()) => self.status("Opened original document", false),
                        Err(e) => self.status(format!("{e:#}"), true),
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus == Focus::Library => {
                self.library_index =
                    (self.library_index + 1).min(self.libraries.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') if self.focus == Focus::Library => {
                self.library_index = self.library_index.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus == Focus::Info => {
                self.info_action = (self.info_action + 1) % 3
            }
            KeyCode::Up | KeyCode::Char('k') if self.focus == Focus::Info => {
                self.info_action = (self.info_action + 2) % 3
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus == Focus::Preview => {
                self.preview_scroll = self.preview_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') if self.focus == Focus::Preview => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => self.preview_scroll = self.preview_scroll.saturating_add(8),
            KeyCode::PageUp => self.preview_scroll = self.preview_scroll.saturating_sub(8),
            KeyCode::Down | KeyCode::Char('j') => {
                self.hit_index = (self.hit_index + 1).min(
                    self.found
                        .as_ref()
                        .map_or(0, |f| f.hits.len().saturating_sub(1)),
                );
                self.preview_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.hit_index = self.hit_index.saturating_sub(1);
                self.preview_scroll = 0;
            }
            _ => {}
        }
        Ok(false)
    }

    fn add_dialog(&mut self) {
        self.modal = Some(Modal::Import(ImportDialog {
            source: Input::default(),
            name: Input::default(),
            name_focus: false,
            auto_name: true,
        }));
    }
    fn modal_key(&mut self, key: KeyEvent, jobs: &mpsc::Sender<Job>) -> Result<bool> {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return Ok(false);
        }
        if let Some(Modal::Delete { name, confirm }) = self.modal.as_mut() {
            let mut submit = None;
            let mut cancel = false;
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => *confirm = !*confirm,
                KeyCode::Left => *confirm = false,
                KeyCode::Right => *confirm = true,
                KeyCode::Char('y') | KeyCode::Char('Y') => submit = Some(name.clone()),
                KeyCode::Enter if *confirm => submit = Some(name.clone()),
                KeyCode::Enter | KeyCode::Char('n') | KeyCode::Char('N') => cancel = true,
                _ => {}
            }
            if cancel {
                self.modal = None;
            }
            if let Some(name) = submit {
                self.delete(name, jobs)?;
            }
            return Ok(false);
        }
        if let Some(Modal::Import(dialog)) = self.modal.as_mut() {
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => dialog.name_focus = !dialog.name_focus,
                KeyCode::Enter => {
                    let source = expand_path(&dialog.source.value);
                    let name = dialog.name.value.clone();
                    if !source.exists() {
                        self.status(
                            "That path does not exist. Enter a document or directory.",
                            true,
                        );
                    } else if let Err(e) = collection::validate_name(&name) {
                        self.status(e.to_string(), true);
                    } else if self.libraries.iter().any(|m| m.name == name) {
                        self.status("That collection exists. Choose another name, or close this dialog and press r to refresh.", true);
                    } else {
                        self.build(source, name, false, jobs)?;
                    }
                }
                _ if dialog.name_focus => {
                    dialog.auto_name = false;
                    dialog.name.key(key);
                }
                _ => {
                    dialog.source.key(key);
                    if dialog.auto_name {
                        dialog.name = Input::new(collection::suggested_name(&expand_path(
                            &dialog.source.value,
                        )));
                    }
                }
            }
        } else {
            self.modal = None;
        }
        Ok(false)
    }
}

fn expand_path(text: &str) -> PathBuf {
    let value = text.trim().trim_matches(['\'', '"']);
    if let Some(suffix) = value.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(suffix)
    } else {
        PathBuf::from(value)
    }
}

pub fn run(home: PathBuf, launch: Launch) -> Result<()> {
    let mut app = App::new(home.clone())?;
    let (jobs, receiver) = mpsc::channel();
    let (updates, events) = mpsc::channel();
    let background = thread::spawn(move || worker(home, receiver, updates));
    match launch {
        Launch::Browse(name) => {
            if let Some(name) = name.or_else(|| app.libraries.first().map(|m| m.name.clone())) {
                app.open(name, &jobs)?;
            }
        }
        Launch::Build { source, name } => app.build(source, name, false, &jobs)?,
        Launch::Refresh(name) => {
            let previous = collection::manifest(&app.home, &name)?;
            app.build(previous.source, name, true, &jobs)?;
        }
    }
    let result = ratatui::run(|terminal| -> std::io::Result<()> {
        crossterm::execute!(std::io::stdout(), event::EnableBracketedPaste)?;
        let result = (|| -> Result<()> {
            'running: loop {
                while let Ok(update) = events.try_recv() {
                    if app.receive(update, &jobs)? {
                        break 'running;
                    }
                }
                terminal.draw(|frame| draw(frame, &mut app))?;
                if event::poll(Duration::from_millis(80))? {
                    match event::read()? {
                        Event::Key(key) if key.kind != KeyEventKind::Release => {
                            if app.key(key, &jobs)? {
                                break;
                            }
                        }
                        Event::Paste(text) => {
                            if let Some(Modal::Import(dialog)) = app.modal.as_mut() {
                                if dialog.name_focus {
                                    dialog.name.paste(&text);
                                    dialog.auto_name = false;
                                } else {
                                    dialog.source.paste(&text);
                                    if dialog.auto_name {
                                        dialog.name = Input::new(collection::suggested_name(
                                            &expand_path(&dialog.source.value),
                                        ));
                                    }
                                }
                            } else if app.focus == Focus::Search && app.progress.is_none() {
                                app.query.paste(&text);
                            }
                        }
                        _ => {}
                    }
                }
                app.tick = app.tick.wrapping_add(1);
            }
            Ok(())
        })();
        let _ = crossterm::execute!(std::io::stdout(), event::DisableBracketedPaste);
        result.map_err(std::io::Error::other)
    });
    if let Some(cancel) = app.cancel {
        cancel.store(true, Ordering::Relaxed);
    }
    let _ = jobs.send(Job::Stop);
    let _ = background.join();
    result?;
    Ok(())
}

/// A raised classic desktop window, shared by panels and dialogs.
struct RetroWindow {
    title: Line<'static>,
    focused: bool,
    paper: bool,
    inset: bool,
}
impl RetroWindow {
    fn paper(mut self) -> Self {
        self.paper = true;
        self
    }
    fn inset(mut self) -> Self {
        self.inset = true;
        self
    }
    fn inner(&self, area: Rect) -> Rect {
        let top = if self.title.width() > 0 { 2 } else { 1 };
        Rect::new(
            area.x.saturating_add(2).min(area.right()),
            area.y.saturating_add(top).min(area.bottom()),
            area.width.saturating_sub(4),
            area.height.saturating_sub(top + 1),
        )
    }
}
impl Widget for RetroWindow {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        if !self.inset {
            for shadow in [
                Rect::new(area.right(), area.y + 1, 1, area.height),
                Rect::new(area.x + 1, area.bottom(), area.width, 1),
            ] {
                buffer.set_style(
                    shadow.intersection(buffer.area),
                    Style::default().bg(SHADOW),
                );
            }
        }
        Block::default()
            .style(Style::default().bg(PANEL).fg(TEXT))
            .render(area, buffer);
        let body = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        buffer.set_style(
            body,
            Style::default()
                .bg(if self.paper { PAPER } else { PANEL })
                .fg(TEXT),
        );
        let light = if self.inset { SHADOW } else { PAPER };
        let dark = if self.inset { PAPER } else { SHADOW };
        for x in area.x..area.right() {
            buffer[(x, area.y)].set_symbol("─").set_fg(light);
            buffer[(x, area.bottom() - 1)].set_symbol("─").set_fg(dark);
        }
        for y in area.y..area.bottom() {
            buffer[(area.x, y)].set_symbol("│").set_fg(light);
            buffer[(area.right() - 1, y)].set_symbol("│").set_fg(dark);
        }
        for (x, y, symbol, color) in [
            (area.x, area.y, "┌", light),
            (area.right() - 1, area.y, "┐", light),
            (area.x, area.bottom() - 1, "└", dark),
            (area.right() - 1, area.bottom() - 1, "┘", dark),
        ] {
            buffer[(x, y)].set_symbol(symbol).set_fg(color);
        }
        if self.title.width() > 0 && area.height >= 3 {
            let title = Rect::new(area.x + 1, area.y + 1, area.width - 2, 1);
            let color = if self.focused { ACCENT } else { BORDER };
            buffer.set_style(title, Style::default().bg(color).fg(PAPER));
            Paragraph::new(self.title)
                .style(Style::default().bg(color).fg(PAPER).bold())
                .render(title, buffer);
        }
    }
}
fn panel(title: impl Into<Line<'static>>, focused: bool) -> RetroWindow {
    RetroWindow {
        title: title.into(),
        focused,
        paper: false,
        inset: false,
    }
}

fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(TEXT)),
        area,
    );
    if area.width < 50 || area.height < 16 {
        frame.render_widget(
            Paragraph::new(
                "INDEXIFY\n\nEnlarge the terminal to at least 50 × 16.\nCtrl+Q to quit.",
            )
            .style(Style::default().fg(ACCENT))
            .centered(),
            area,
        );
        return;
    }
    let window = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let shell = panel(" [I] Indexify - Document Explorer ", true);
    let outer = shell.inner(window);
    frame.render_widget(shell, window);
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .split(outer);
    let menu = Layout::horizontal([Constraint::Min(20), Constraint::Length(24)]).split(rows[0]);
    frame.render_widget(
        Paragraph::new(if area.width >= 105 {
            "^N Add   ^D Delete   F7 AND/OR   F1 Help"
        } else {
            "^N Add  ^D Delete  F7 AND/OR  F1 Help"
        })
        .style(Style::default().fg(TEXT).bg(PANEL)),
        menu[0],
    );
    frame.render_widget(
        Paragraph::new("PDF / MD / DOCX  ^Q Exit")
            .style(Style::default().fg(TEXT).bg(PANEL))
            .right_aligned(),
        menu[1],
    );
    if rows[0].height > 1 {
        let navigation = [
            (Focus::Search, "F2 Search"),
            (Focus::Results, "F3 Results"),
            (Focus::Preview, "F4 Preview"),
            (Focus::Library, "F5 Library"),
            (Focus::Info, "F6 Info"),
        ]
        .into_iter()
        .map(|(focus, label)| {
            Span::styled(
                format!("[{label}] "),
                Style::default()
                    .fg(if app.focus == focus { PAPER } else { TEXT })
                    .bg(if app.focus == focus { ACCENT } else { PANEL }),
            )
        })
        .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(Line::from(navigation)),
            Rect {
                y: rows[0].y + 1,
                height: 1,
                ..rows[0]
            },
        );
    }
    if rows[0].height > 2 {
        frame.render_widget(
            Paragraph::new("─".repeat(rows[0].width as usize))
                .style(Style::default().fg(BORDER).bg(PANEL)),
            Rect {
                y: rows[0].y + 2,
                height: 1,
                ..rows[0]
            },
        );
    }
    let columns = if area.width >= 105 {
        Layout::horizontal([Constraint::Length(25), Constraint::Min(35)])
            .spacing(2)
            .split(rows[1])
    } else {
        Layout::horizontal([Constraint::Length(0), Constraint::Min(35)]).split(rows[1])
    };
    if columns[0].width > 0 {
        draw_library(frame, app, columns[0]);
    }
    let work = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Min(4),
    ])
    .split(columns[1]);
    draw_search(frame, app, work[0]);
    let context = app
        .active
        .as_ref()
        .map_or("YOUR DOCUMENT LIBRARY".into(), |m| {
            format!(
                "{}  /  {} documents  /  {} passages",
                m.name.to_uppercase(),
                m.files,
                m.passages
            )
        });
    frame.render_widget(
        Paragraph::new(clip(&context, work[1].width as usize))
            .style(Style::default().fg(MUTED).bg(PANEL)),
        work[1],
    );
    if app.found.as_ref().is_some_and(|f| !f.hits.is_empty()) {
        let panes = if work[2].width >= 70 {
            Layout::horizontal([Constraint::Percentage(43), Constraint::Percentage(57)])
                .spacing(1)
                .split(work[2])
        } else {
            Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)])
                .spacing(1)
                .split(work[2])
        };
        draw_results(frame, app, panes[0]);
        draw_preview(frame, app, panes[1]);
    } else {
        draw_empty(frame, app, work[2]);
    }
    let status = if let Some(name) = &app.deleting {
        format!("{} Deleting index '{name}'", spinner(app.tick))
    } else if app.busy {
        format!(
            "{}  {}",
            spinner(app.tick),
            if app.progress.is_some() {
                "Importing documents"
            } else {
                "Searching your library"
            }
        )
    } else {
        app.message.clone()
    };
    frame.render_widget(
        Paragraph::new(clip(
            &collection::safe_text(&status),
            rows[2].width as usize,
        ))
        .style(
            Style::default()
                .fg(if app.error { RED } else { MUTED })
                .bg(PANEL),
        ),
        Rect {
            height: 1,
            ..rows[2]
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            key(" Tab ", "next  "),
            key(" ↑↓ ", "select  "),
            key(" ↵ ", "action  "),
            key(" ^D ", "delete  "),
            key(" ^Q ", "quit"),
        ]))
        .style(Style::default().fg(MUTED).bg(PANEL)),
        Rect {
            y: rows[2].y + 2,
            height: 1,
            ..rows[2]
        },
    );
    if let Some(progress) = &app.progress {
        draw_progress(frame, progress, app.tick, app.quitting);
    }
    if let Some(modal) = &app.modal {
        draw_modal(frame, modal, app);
    }
    if area.width < 105
        && matches!(app.focus, Focus::Library | Focus::Info)
        && app.modal.is_none()
        && app.progress.is_none()
    {
        let picker = popup(area, 50, 30);
        frame.render_widget(Clear, picker);
        draw_library(frame, app, picker);
    }
}

fn key(label: &'static str, caption: &'static str) -> Span<'static> {
    Span::styled(
        format!("[{} {}] ", label.trim(), caption.trim()),
        Style::default().fg(TEXT).bg(PANEL),
    )
}

fn draw_library(frame: &mut Frame, app: &App, area: Rect) {
    let info_height = if area.height >= 22 { 12 } else { 8 };
    let rows = Layout::vertical([Constraint::Min(4), Constraint::Length(info_height)])
        .spacing(1)
        .split(area);
    let block = panel(" F5 Collections ", app.focus == Focus::Library).paper();
    let inner = block.inner(rows[0]);
    frame.render_widget(block, rows[0]);
    if app.libraries.is_empty() {
        frame.render_widget(
            Paragraph::new("\nNothing here yet.\n\nCtrl+N to add your\nfirst collection.")
                .style(Style::default().fg(MUTED)),
            inner,
        );
    } else {
        let items: Vec<_> = app
            .libraries
            .iter()
            .enumerate()
            .map(|(position, m)| {
                let active = app.active.as_ref().is_some_and(|a| a.name == m.name);
                let selected = position == app.library_index;
                let foreground = if selected { PAPER } else { TEXT };
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            if active { "● " } else { "○ " },
                            Style::default().fg(if selected {
                                PAPER
                            } else if active {
                                ACCENT
                            } else {
                                DIM
                            }),
                        ),
                        Span::styled(m.name.clone(), Style::default().fg(foreground).bold()),
                    ]),
                    Line::styled(
                        format!("  {} documents", m.files),
                        Style::default().fg(if selected { PAPER } else { MUTED }),
                    ),
                    Line::styled(
                        format!("  {} passages", m.passages),
                        Style::default().fg(if selected { PANEL } else { DIM }),
                    ),
                    Line::raw(""),
                ])
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(app.library_index));
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(Style::default().bg(SELECTION).fg(PAPER))
                .highlight_symbol("▎"),
            inner,
            &mut state,
        );
    }
    draw_info(frame, app, rows[1]);
}

fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let info = panel(" F6 Collection info ", app.focus == Focus::Info);
    let inner = info.inner(area);
    frame.render_widget(info, area);
    let mut details = Vec::new();
    if let Some(collection) = app.info_collection() {
        if inner.height >= 8 {
            details.push(Line::styled("SNAPSHOT", Style::default().fg(ACCENT).bold()));
            details.push(Line::raw(""));
        }
        details.push(Line::styled(
            format!(
                "{} · {}",
                age(collection.built_at),
                human_bytes(collection.index_bytes)
            ),
            Style::default().fg(TEXT),
        ));
        details.push(Line::styled(
            format!("{} import issues", collection.issues.len()),
            Style::default().fg(if collection.issues.is_empty() {
                MUTED
            } else {
                SECONDARY
            }),
        ));
        if inner.height >= 8 {
            details.push(Line::raw(""));
        }
        for (index, label) in [
            "[r] Refresh snapshot",
            "[w] Import report",
            "[d] Delete index",
        ]
        .iter()
        .enumerate()
        {
            let selected = app.focus == Focus::Info && app.info_action == index;
            details.push(Line::styled(
                *label,
                Style::default()
                    .fg(if selected { PAPER } else { TEXT })
                    .bg(if selected { ACCENT } else { PANEL }),
            ));
        }
    } else {
        details.push(Line::styled(
            "No collection selected.",
            Style::default().fg(MUTED),
        ));
        details.push(Line::raw(""));
        details.push(Line::styled(
            "Ctrl+N adds a collection.",
            Style::default().fg(ACCENT),
        ));
    }
    frame.render_widget(Paragraph::new(details), inner);
}

fn draw_search(frame: &mut Frame, app: &App, area: Rect) {
    let label = format!(
        " F2 Search · {} ({} words) ",
        app.mode.label(),
        if app.mode == QueryMode::Conjunctive {
            "all"
        } else {
            "any"
        }
    );
    let block = panel(label, app.focus == Focus::Search).paper();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (visible, cursor) = app.query.visible(inner.width.saturating_sub(1) as usize);
    frame.render_widget(
        Paragraph::new(if app.query.value.is_empty() {
            "Search a few remembered words…".into()
        } else {
            visible
        })
        .style(Style::default().fg(if app.query.value.is_empty() {
            DIM
        } else {
            TEXT
        })),
        inner,
    );
    if app.focus == Focus::Search
        && app.modal.is_none()
        && app.progress.is_none()
        && inner.width > 0
    {
        frame.set_cursor_position((
            inner.x + (cursor as u16).min(inner.width.saturating_sub(1)),
            inner.y,
        ));
    }
}

fn draw_results(frame: &mut Frame, app: &App, area: Rect) {
    let found = app.found.as_ref().unwrap();
    let title = format!(" F3 Best matches · {} ", found.hits.len());
    let block = panel(title, app.focus == Focus::Results).paper();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let terms: HashSet<_> = SimpleTokenizer.tokenize(&found.query).into_iter().collect();
    let width = inner.width.saturating_sub(3) as usize;
    let items: Vec<_> = found
        .hits
        .iter()
        .enumerate()
        .map(|(position, hit)| {
            let name = file_name(&hit.passage.source);
            let selected = position == app.hit_index;
            ListItem::new(vec![
                Line::styled(
                    clip(&format!("{:02}  {name}", hit.rank), width),
                    Style::default()
                        .fg(if selected { PAPER } else { TEXT })
                        .bold(),
                ),
                Line::styled(
                    clip(
                        &format!("    {}  ·  {}", hit.passage.format, hit.passage.location),
                        width,
                    ),
                    Style::default().fg(if selected { PAPER } else { ACCENT }),
                ),
                highlight(
                    &clip(&hit.text, width),
                    &terms,
                    if selected { PAPER } else { MUTED },
                ),
                Line::raw(""),
            ])
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.hit_index));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(Style::default().bg(SELECTION).fg(PAPER))
            .highlight_symbol("▎ "),
        inner,
        &mut state,
    );
}

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = panel(" F4 Passage preview ", app.focus == Focus::Preview).paper();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(hit) = app.hit() else {
        return;
    };
    let compact = inner.height < 10;
    let parts = Layout::vertical([
        Constraint::Length(if compact { 2 } else { 4 }),
        Constraint::Min(1),
        Constraint::Length(if compact { 1 } else { 2 }),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                clip(&file_name(&hit.passage.source), inner.width as usize),
                Style::default().fg(TEXT).bold(),
            ),
            Line::styled(
                clip(
                    &format!("{}  /  {}", hit.passage.format, hit.passage.location),
                    inner.width as usize,
                ),
                Style::default().fg(ACCENT),
            ),
            Line::styled(
                clip(&hit.passage.heading, inner.width as usize),
                Style::default().fg(SECONDARY),
            ),
        ]),
        parts[0],
    );
    let terms: HashSet<_> = SimpleTokenizer
        .tokenize(&app.found.as_ref().unwrap().query)
        .into_iter()
        .collect();
    let lines = wrap_lines(&hit.text, parts[1].width as usize, &terms);
    let maximum = lines
        .len()
        .saturating_sub(parts[1].height as usize)
        .min(u16::MAX as usize) as u16;
    let path = collection::safe_text(&hit.passage.source.display().to_string());
    app.preview_scroll = app.preview_scroll.min(maximum);
    frame.render_widget(
        Paragraph::new(lines).scroll((app.preview_scroll, 0)),
        parts[1],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(clip(&path, inner.width as usize), Style::default().fg(DIM)),
            Line::styled(
                "↵ open original   c copy passage",
                Style::default().fg(MUTED),
            ),
        ]),
        parts[2],
    );
}

fn draw_empty(frame: &mut Frame, app: &App, area: Rect) {
    let block = panel(
        match app.focus {
            Focus::Results => " F3 Best matches ",
            Focus::Preview => " F4 Passage preview ",
            _ => " Explore ",
        },
        matches!(app.focus, Focus::Results | Focus::Preview),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (title, subtitle, extra) = if app.busy {
        (
            "Finding your next answer…",
            "Your library is being prepared.",
            "",
        )
    } else if app.active.is_none() {
        (
            "A home for your documents.",
            "Index a PDF, Markdown file, Word document, or an entire directory.",
            "Press Ctrl+N to create your first collection.",
        )
    } else if app
        .found
        .as_ref()
        .is_some_and(|f| !f.query.trim().is_empty())
    {
        (
            "No matching passages.",
            "Try fewer words, or F7 to switch between AND and OR.",
            "Search matches words, not exact phrases or meanings.",
        )
    } else {
        (
            "Find the passage. Keep the context.",
            "Search across your collection, then preview the part that matters.",
            "Type a few words above and press Enter.",
        )
    };
    let top = inner.y + inner.height.saturating_sub(8) / 2;
    let content = Rect {
        y: top,
        height: inner.height.saturating_sub(top - inner.y),
        ..inner
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("[ DOCUMENT LIBRARY ]", Style::default().fg(ACCENT)),
            Line::raw(""),
            Line::styled(title, Style::default().fg(TEXT).bold()),
            Line::raw(""),
            Line::styled(subtitle, Style::default().fg(MUTED)),
            Line::raw(""),
            Line::styled(extra, Style::default().fg(ACCENT)),
        ])
        .centered()
        .wrap(Wrap { trim: true }),
        content,
    );
}

fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn draw_progress(frame: &mut Frame, progress: &Progress, tick: usize, cancelling: bool) {
    let area = popup(frame.area(), 76, 12);
    frame.render_widget(Clear, area);
    let block = panel(" Building your collection ", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(format!(
            "{}  {}",
            spinner(tick),
            if cancelling {
                "Cancelling safely…"
            } else {
                &progress.stage
            }
        ))
        .style(Style::default().fg(ACCENT).bold()),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(clip(
            &collection::safe_text(&progress.detail),
            rows[1].width as usize,
        ))
        .style(Style::default().fg(MUTED)),
        rows[1],
    );
    let ratio = if progress.total > 0 {
        progress.done as f64 / progress.total as f64
    } else {
        0.0
    };
    let label = if progress.total > 0 {
        format!("{} / {} documents", progress.done, progress.total)
    } else {
        "Discovering supported documents".into()
    };
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(ACCENT).bg(BORDER))
            .ratio(ratio.clamp(0.0, 1.0))
            .label(label),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new("Extract text → split passages → build search index")
            .style(Style::default().fg(MUTED)),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new("PDF · Markdown · DOCX                         Ctrl+Q cancel")
            .style(Style::default().fg(DIM)),
        rows[4],
    );
}

fn draw_modal(frame: &mut Frame, modal: &Modal, app: &App) {
    let area = popup(
        frame.area(),
        82,
        match modal {
            Modal::Issues => 24,
            Modal::Help => 21,
            _ => 17,
        },
    );
    frame.render_widget(Clear, area);
    let block = panel(
        match modal {
            Modal::Import(_) => " Add a collection ",
            Modal::Help => " Make yourself at home ",
            Modal::Issues => " Import report ",
            Modal::Delete { .. } => " Delete index ",
        },
        true,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match modal {
        Modal::Delete { name, confirm } => {
            let rows = Layout::vertical([
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Min(1),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new(format!("Delete index '{name}'?"))
                    .style(Style::default().fg(RED).bold())
                    .wrap(Wrap { trim: true }),
                rows[0],
            );
            frame.render_widget(Paragraph::new("This removes the saved search index, extracted text snapshots, and collection metadata.")
                .style(Style::default().fg(TEXT)).wrap(Wrap { trim: true }), rows[1]);
            frame.render_widget(
                Paragraph::new("Your PDF, MD and DOCX source documents remain in place.")
                    .style(Style::default().fg(TEXT))
                    .wrap(Wrap { trim: true }),
                rows[2],
            );
            let buttons =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .spacing(2)
                    .split(rows[3]);
            for (index, label) in ["[ Cancel ]", "[ Delete index ]"].iter().enumerate() {
                let selected = if index == 0 { !confirm } else { *confirm };
                frame.render_widget(
                    Paragraph::new(*label).centered().style(
                        Style::default()
                            .fg(if selected { PAPER } else { TEXT })
                            .bg(if selected { ACCENT } else { PANEL })
                            .bold(),
                    ),
                    buttons[index],
                );
            }
            frame.render_widget(
                Paragraph::new("Tab / arrows choose · Enter select · Esc cancel")
                    .style(Style::default().fg(MUTED))
                    .wrap(Wrap { trim: true }),
                rows[4],
            );
        }
        Modal::Import(dialog) => {
            let rows = Layout::vertical([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new(
                    "One document or an entire directory. Everything stays on your machine.",
                )
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: true }),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new("DOCUMENT OR DIRECTORY").style(Style::default().fg(MUTED)),
                rows[1],
            );
            input_field(
                frame,
                &dialog.source,
                "~/Documents/research",
                rows[2],
                !dialog.name_focus,
            );
            frame.render_widget(
                Paragraph::new("COLLECTION NAME").style(Style::default().fg(MUTED)),
                rows[3],
            );
            input_field(frame, &dialog.name, "research", rows[4], dialog.name_focus);
            frame.render_widget(
                Paragraph::new("PDF · MD · DOCX     Tab switch field   Enter index   Esc cancel")
                    .style(Style::default().fg(ACCENT)),
                rows[5],
            );
        }
        Modal::Help => {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled("SEARCH & EXPLORE", Style::default().fg(ACCENT).bold()),
                    Line::raw(
                        "F2 / Ctrl+F  Search    F3 / Ctrl+E  Results    F4 / Ctrl+P  Preview",
                    ),
                    Line::raw("F5 / Ctrl+L  Collections       F6 / Ctrl+G  Collection info"),
                    Line::raw(
                        "/                Focus search       Enter    Search / open original",
                    ),
                    Line::raw(
                        "Tab / Shift+Tab  Switch panels      ↑↓ / jk  Select / scroll preview",
                    ),
                    Line::raw("F7 / Ctrl+O      AND / OR matching  c        Copy selected passage"),
                    Line::raw(
                        "PageUp/PageDown  Scroll preview     Esc      Leave search / close dialog",
                    ),
                    Line::raw(""),
                    Line::styled("YOUR LIBRARY", Style::default().fg(ACCENT).bold()),
                    Line::raw("Ctrl+N / a       Add a collection   Ctrl+L   Focus collection list"),
                    Line::raw("r  Refresh snapshot   w  Import report   Ctrl+D / d  Delete index"),
                    Line::raw("In Collection info: arrows select an action; Enter runs it."),
                    Line::raw(
                        "Ctrl+Q / Ctrl+C  Quit               q        Quit outside text fields",
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Type in the search field, then Enter. Press any key to close.",
                        Style::default().fg(MUTED),
                    ),
                ])
                .style(Style::default().fg(TEXT))
                .wrap(Wrap { trim: true }),
                inner,
            );
        }
        Modal::Issues => {
            let mut lines = vec![
                Line::styled(
                    "Unsuccessful documents were excluded from this snapshot.",
                    Style::default().fg(MUTED),
                ),
                Line::raw(""),
            ];
            if let Some(active) = app.info_collection() {
                if active.issues.is_empty() {
                    lines.push(Line::styled(
                        "All supported documents were imported successfully.",
                        Style::default().fg(ACCENT),
                    ));
                }
                for issue in &active.issues {
                    lines.push(Line::styled(
                        collection::safe_text(&issue.source.display().to_string()),
                        Style::default().fg(SECONDARY),
                    ));
                    lines.push(Line::styled(
                        collection::safe_text(&issue.message),
                        Style::default().fg(TEXT),
                    ));
                    lines.push(Line::raw(""));
                }
                lines.push(Line::styled(format!("{} unsupported files skipped. Hidden directories and symlinks are excluded.", active.skipped), Style::default().fg(MUTED)));
            } else {
                lines.push(Line::raw("Choose a collection to view its import report."));
            }
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        }
    }
}

fn input_field(frame: &mut Frame, input: &Input, hint: &str, area: Rect, focused: bool) {
    let block = panel("", focused).paper().inset();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (visible, cursor) = input.visible(inner.width.saturating_sub(1) as usize);
    frame.render_widget(
        Paragraph::new(if input.value.is_empty() {
            hint.into()
        } else {
            visible
        })
        .style(Style::default().fg(if input.value.is_empty() { DIM } else { TEXT })),
        inner,
    );
    if focused && inner.width > 0 {
        frame.set_cursor_position((inner.x + (cursor as u16).min(inner.width - 1), inner.y));
    }
}

fn spinner(tick: usize) -> &'static str {
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][tick % 10]
}
fn file_name(path: &Path) -> String {
    collection::safe_text(&path.file_name().unwrap_or_default().to_string_lossy())
}
fn clip(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut size = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if size + w + 1 > width {
            break;
        }
        out.push(c);
        size += w;
    }
    out.push('…');
    out
}
fn highlight(text: &str, terms: &HashSet<String>, foreground: Color) -> Line<'static> {
    let mut spans = Vec::new();
    let mut start = 0;
    let mut word = false;
    for (i, c) in text.char_indices() {
        let next = c.is_alphanumeric();
        if next != word {
            if i > start {
                push_highlight(&mut spans, &text[start..i], word, terms, foreground);
            }
            start = i;
            word = next;
        }
    }
    if start < text.len() {
        push_highlight(&mut spans, &text[start..], word, terms, foreground);
    }
    Line::from(spans)
}
fn push_highlight(
    spans: &mut Vec<Span<'static>>,
    value: &str,
    word: bool,
    terms: &HashSet<String>,
    foreground: Color,
) {
    let style = if word && terms.contains(&value.to_lowercase()) {
        Style::default().fg(TEXT).bg(MATCH).bold()
    } else {
        Style::default().fg(foreground)
    };
    spans.push(Span::styled(value.to_owned(), style));
}
fn wrap_lines(text: &str, width: usize, terms: &HashSet<String>) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.width() + 1 + word.width() > width {
            lines.push(highlight(&current, terms, TEXT));
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        for c in word.chars() {
            if current.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > width
                && !current.is_empty()
            {
                lines.push(highlight(&current, terms, TEXT));
                current.clear();
            }
            current.push(c);
        }
    }
    if !current.is_empty() {
        lines.push(highlight(&current, terms, TEXT));
    }
    lines
}
fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KiB", bytes as f64 / 1024.0)
    }
}
fn age(timestamp: u64) -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(timestamp);
    if seconds < 60 {
        "just now".into()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86400)
    }
}

/// Export actual Ratatui cells, rather than maintaining a separate visual mockup.
pub fn snapshot(
    home: &Path,
    name: &str,
    query: &str,
    output: &Path,
    width: u16,
    height: u16,
) -> Result<()> {
    let mut app = App::new(home.to_owned())?;
    let mut collection = Collection::open(home, name)?;
    app.library_index = app
        .libraries
        .iter()
        .position(|m| m.name == name)
        .unwrap_or(0);
    app.active = Some(collection.manifest.clone());
    app.query = Input::new(query.into());
    if !query.is_empty() {
        app.found = Some(collection.search(query, QueryMode::Disjunctive)?);
        app.focus = Focus::Results;
    }
    app.message = app
        .found
        .as_ref()
        .map_or("Your library is ready.".into(), |f| {
            format!(
                "{} matching passages · {} ms",
                f.matched_passages, f.elapsed_ms
            )
        });
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| draw(frame, &mut app))?;
    let buffer = terminal.backend().buffer();
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}"><rect width="100%" height="100%" fill="#008080"/><g font-family="Menlo,DejaVu Sans Mono,monospace" font-size="15">"##,
        width as u32 * 9 + 32,
        height as u32 * 20 + 32,
        width as u32 * 9 + 32,
        height as u32 * 20 + 32
    );
    let mut cells = Vec::new();
    let mut glyphs = String::new();
    for y in 0..height {
        for x in 0..width {
            let cell = &buffer[(x, y)];
            let fg = color_hex(cell.fg, "#000000");
            let bg = color_hex(cell.bg, "#c0c0c0");
            let px = 16 + x as u32 * 9;
            let py = 16 + y as u32 * 20;
            svg.push_str(&format!(
                r#"<rect x="{px}" y="{py}" width="9" height="20" fill="{bg}"/>"#
            ));
            if cell.symbol() != " " {
                let escaped = cell
                    .symbol()
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                let weight = if cell.modifier.contains(Modifier::BOLD) {
                    "bold"
                } else {
                    "normal"
                };
                glyphs.push_str(&format!(
                    r#"<text x="{px}" y="{}" fill="{fg}" font-weight="{weight}">{escaped}</text>"#,
                    py + 15
                ));
            }
            cells.push(serde_json::json!({"x":x,"y":y,"text":cell.symbol(),"fg":fg,"bg":bg,"bold":cell.modifier.contains(Modifier::BOLD)}));
        }
    }
    svg.push_str(&glyphs);
    svg.push_str("</g></svg>");
    fs::write(output, svg)?;
    fs::write(
        output.with_extension("cells.json"),
        serde_json::to_vec(&serde_json::json!({"width":width,"height":height,"cells":cells}))?,
    )?;
    Ok(())
}
fn color_hex(color: Color, fallback: &str) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => fallback.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_input_edits_on_character_boundaries() {
        let mut input = Input::new("naïve 東京".into());
        input.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        input.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.value, "naïve 京");
        input.key(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::NONE));
        assert_eq!(input.value, "naïve é京");
        assert!(input.visible(4).0.width() <= 4);
    }
    #[test]
    fn layouts_render_at_small_and_large_sizes() {
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(home.path().into()).unwrap();
        for (width, height) in [(30, 10), (50, 16), (80, 24), (144, 42)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.add_dialog();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.modal = None;
            for focus in [Focus::Library, Focus::Info] {
                app.focus = focus;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            }
            app.modal = Some(Modal::Delete {
                name: "example".into(),
                confirm: false,
            });
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.modal = None;
        }
    }

    #[test]
    fn navigation_reaches_info_and_shortcuts_work_inside_search() {
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(home.path().into()).unwrap();
        let (jobs, _receiver) = mpsc::channel();
        app.query = Input::new("keep this query".into());
        for expected in [
            Focus::Results,
            Focus::Preview,
            Focus::Library,
            Focus::Info,
            Focus::Search,
        ] {
            app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &jobs)
                .unwrap();
            assert_eq!(app.focus, expected);
        }
        for expected in [
            Focus::Info,
            Focus::Library,
            Focus::Preview,
            Focus::Results,
            Focus::Search,
        ] {
            app.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), &jobs)
                .unwrap();
            assert_eq!(app.focus, expected);
        }
        for (number, letter, expected) in [
            (2, 'f', Focus::Search),
            (3, 'e', Focus::Results),
            (4, 'p', Focus::Preview),
            (5, 'l', Focus::Library),
            (6, 'g', Focus::Info),
        ] {
            app.focus = Focus::Search;
            app.key(KeyEvent::new(KeyCode::F(number), KeyModifiers::NONE), &jobs)
                .unwrap();
            assert_eq!(app.focus, expected);
            app.focus = Focus::Search;
            app.key(
                KeyEvent::new(KeyCode::Char(letter), KeyModifiers::CONTROL),
                &jobs,
            )
            .unwrap();
            assert_eq!(app.focus, expected);
            assert_eq!(app.query.value, "keep this query");
        }
    }

    fn fake_manifest(name: &str) -> Manifest {
        Manifest {
            version: 1,
            name: name.into(),
            source: PathBuf::from("/tmp/example.md"),
            built_at: 0,
            files: 1,
            passages: 1,
            skipped: 0,
            issues: vec![],
            index_bytes: 0,
        }
    }

    #[test]
    fn mode_shortcuts_toggle_and_rerun_without_editing_query() {
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(home.path().into()).unwrap();
        app.active = Some(fake_manifest("example"));
        app.query = Input::new("graph search".into());
        let (jobs, receiver) = mpsc::channel();
        app.key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE), &jobs)
            .unwrap();
        assert_eq!(app.mode, QueryMode::Conjunctive);
        assert!(
            matches!(receiver.recv().unwrap(), Job::Search { query, mode: QueryMode::Conjunctive, .. } if query == "graph search")
        );
        app.key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            &jobs,
        )
        .unwrap();
        assert_eq!(app.mode, QueryMode::Disjunctive);
        assert!(
            matches!(receiver.recv().unwrap(), Job::Search { query, mode: QueryMode::Disjunctive, .. } if query == "graph search")
        );
        assert_eq!(app.query.value, "graph search");
        assert_eq!(app.focus, Focus::Search);
    }

    #[test]
    fn delete_requires_confirmation_and_targets_the_highlighted_collection() {
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(home.path().into()).unwrap();
        app.libraries = vec![fake_manifest("alpha"), fake_manifest("beta")];
        app.active = Some(app.libraries[0].clone());
        app.library_index = 1;
        app.focus = Focus::Info;
        let (jobs, receiver) = mpsc::channel();
        app.info_enter(&jobs).unwrap();
        assert!(matches!(app.modal, Some(Modal::Issues)));
        app.modal = None;
        app.key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            &jobs,
        )
        .unwrap();
        assert!(
            matches!(&app.modal, Some(Modal::Delete { name, confirm: false }) if name == "beta")
        );
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &jobs)
            .unwrap();
        assert!(app.modal.is_none());
        assert!(receiver.try_recv().is_err());
        app.request_delete();
        app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &jobs)
            .unwrap();
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &jobs)
            .unwrap();
        assert!(matches!(receiver.recv().unwrap(), Job::Delete(name) if name == "beta"));
    }

    #[test]
    fn deleting_recovers_from_other_readers_and_keeps_source_documents() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("notes.md");
        fs::write(&source, "Cobalt passage for a deletion test.").unwrap();
        let home = temporary.path().join("library");
        collection::build(&home, "notes", &source, false, &mut |_| Ok(())).unwrap();
        let mut app = App::new(home.clone()).unwrap();
        let (jobs, receiver) = mpsc::channel();
        let (updates, events) = mpsc::channel();
        let background = thread::spawn(move || worker(home, receiver, updates));
        app.open("notes".into(), &jobs).unwrap();
        app.receive(events.recv_timeout(Duration::from_secs(3)).unwrap(), &jobs)
            .unwrap();
        let external = Collection::open(&app.home, "notes").unwrap();
        app.delete("notes".into(), &jobs).unwrap();
        app.receive(events.recv_timeout(Duration::from_secs(3)).unwrap(), &jobs)
            .unwrap();
        assert!(app.error);
        assert!(app.active.is_some());
        app.query = Input::new("cobalt".into());
        app.search(&jobs).unwrap();
        app.receive(events.recv_timeout(Duration::from_secs(3)).unwrap(), &jobs)
            .unwrap();
        assert_eq!(app.found.as_ref().unwrap().hits.len(), 1);
        drop(external);
        app.delete("notes".into(), &jobs).unwrap();
        app.receive(events.recv_timeout(Duration::from_secs(3)).unwrap(), &jobs)
            .unwrap();
        assert!(app.libraries.is_empty());
        assert!(app.active.is_none() && app.found.is_none());
        assert!(source.is_file());
        jobs.send(Job::Stop).unwrap();
        background.join().unwrap();
    }
}
