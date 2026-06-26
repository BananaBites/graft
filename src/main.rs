use std::cmp::{max, min};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use anyhow::{Context, Result, anyhow};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::{Color, Constraint, Direction, Layout, Modifier, Style, Text};
use ratatui::widgets::{Block, Paragraph, Widget};
use ratatui::{Frame, Terminal};
use regex::Regex;
use unicode_width::UnicodeWidthStr;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const MODULE_REPO: &str = "https://github.com/BananaBites/graft.git";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Graph,
    FullDiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusKind {
    Commit,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Commit,
    File,
    Diff,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffChangeKind {
    None,
    Add,
    Delete,
    Modify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptMode {
    None,
    Filter,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterKind {
    Message,
    Branch,
    Tag,
    Author,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefFilterMode {
    History,
    Decorations,
}

#[derive(Debug, Clone)]
struct CommitInfo {
    hash: String,
    line: String,
    pre_lines: Vec<String>,
    branch: String,
    branch_color: usize,
    branch_badge: String,
}

#[derive(Debug, Clone)]
struct FileChange {
    status: String,
    path: String,
    old_path: String,
}

#[derive(Debug, Clone)]
struct GraphFilter {
    kind: FilterKind,
    value: String,
}

#[derive(Debug, Clone)]
struct PromptState {
    mode: PromptMode,
    input: String,
    suggest_index: usize,
}
impl Default for PromptState {
    fn default() -> Self {
        Self {
            mode: PromptMode::None,
            input: String::new(),
            suggest_index: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct ExpansionState {
    anchor_hash: String,
    a: String,
    b: String,
    compare: bool,
    files: Vec<FileChange>,
    loading: bool,
    err: Option<String>,
    token: u64,
}

#[derive(Debug, Clone)]
struct DiffState {
    a: String,
    b: String,
    path: String,
    title: String,
    content: String,
    lines: Vec<String>,
    change_lines: Vec<usize>,
    change_kinds: Vec<DiffChangeKind>,
    loading: bool,
    err: Option<String>,
    offset: usize,
    token: u64,
    full_file: bool,
    show_whitespace: bool,
}
impl Default for DiffState {
    fn default() -> Self {
        Self {
            a: String::new(),
            b: String::new(),
            path: String::new(),
            title: String::new(),
            content: String::new(),
            lines: vec![],
            change_lines: vec![],
            change_kinds: vec![],
            loading: false,
            err: None,
            offset: 0,
            token: 0,
            full_file: false,
            show_whitespace: false,
        }
    }
}

#[derive(Debug, Clone)]
struct RenderedLine {
    text: String,
    kind: LineKind,
    commit_idx: usize,
    file_idx: Option<usize>,
    selectable: bool,
}

#[derive(Debug, Clone)]
struct CliOptions {
    help: bool,
    version: bool,
    update_target: String,
    completion: String,
    path: Option<PathBuf>,
    delta_theme: String,
    delta_theme_set: bool,
    delta_syntax_theme: String,
    delta_syntax_theme_set: bool,
    delta_mode: String,
    delta_mode_set: bool,
    save_config: bool,
    show_delta_themes: bool,
    list_delta_syntax_themes: bool,
}
impl Default for CliOptions {
    fn default() -> Self {
        Self {
            help: false,
            version: false,
            update_target: String::new(),
            completion: String::new(),
            path: None,
            delta_theme: String::new(),
            delta_theme_set: false,
            delta_syntax_theme: String::new(),
            delta_syntax_theme_set: false,
            delta_mode: String::new(),
            delta_mode_set: false,
            save_config: false,
            show_delta_themes: false,
            list_delta_syntax_themes: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct AppConfig {
    delta_theme: String,
    delta_syntax_theme: String,
    delta_mode: String,
}

#[derive(Debug)]
enum Msg {
    GraphLoaded {
        commits: Vec<CommitInfo>,
        err: Option<String>,
    },
    RefsLoaded {
        branches: Vec<String>,
        tags: Vec<String>,
        err: Option<String>,
    },
    FilesLoaded {
        token: u64,
        anchor_hash: String,
        a: String,
        b: String,
        compare: bool,
        files: Vec<FileChange>,
        err: Option<String>,
    },
    DiffLoaded {
        token: u64,
        full: bool,
        a: String,
        b: String,
        path: String,
        content: String,
        err: Option<String>,
        full_file: bool,
    },
}

struct App {
    mode: AppMode,
    width: u16,
    height: u16,
    commits: Vec<CommitInfo>,
    loading: bool,
    err: Option<String>,
    selected_commit: usize,
    selected_file: Option<usize>,
    focus: FocusKind,
    expanded: Option<ExpansionState>,
    inline: Option<DiffState>,
    compare_a: String,
    compare_b: String,
    delta_theme: String,
    delta_syntax_theme: String,
    delta_mode: String,
    filters: Vec<GraphFilter>,
    ref_mode: RefFilterMode,
    branches: Vec<String>,
    tags: Vec<String>,
    prompt: PromptState,
    search_text: String,
    status: String,
    main_scroll: usize,
    header_height: usize,
    lines: Vec<RenderedLine>,
    lines_dirty: bool,
    full_meta: DiffState,
    next_token: u64,
    tx: Sender<Msg>,
}

impl App {
    fn new(opts: &CliOptions, tx: Sender<Msg>) -> Self {
        Self {
            mode: AppMode::Graph,
            width: 0,
            height: 0,
            commits: vec![],
            loading: true,
            err: None,
            selected_commit: 0,
            selected_file: None,
            focus: FocusKind::Commit,
            expanded: None,
            inline: None,
            compare_a: String::new(),
            compare_b: String::new(),
            delta_theme: opts.delta_theme.clone(),
            delta_syntax_theme: opts.delta_syntax_theme.clone(),
            delta_mode: opts.delta_mode.clone(),
            filters: vec![],
            ref_mode: RefFilterMode::History,
            branches: vec![],
            tags: vec![],
            prompt: PromptState::default(),
            search_text: String::new(),
            status: String::new(),
            main_scroll: 0,
            header_height: 2,
            lines: vec![],
            lines_dirty: true,
            full_meta: DiffState::default(),
            next_token: 0,
            tx,
        }
    }

    fn init_load(&self) {
        spawn_load_graph(self.tx.clone(), self.filters.clone(), self.ref_mode);
        spawn_load_refs(self.tx.clone());
    }

    fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::GraphLoaded { commits, err } => {
                self.loading = false;
                self.err = err;
                self.commits = commits;
                if self.err.is_none() && self.commits.is_empty() && !self.filters.is_empty() {
                    self.status = "no commits match active filters".into();
                }
                if self.selected_commit >= self.commits.len() {
                    self.selected_commit = self.commits.len().saturating_sub(1);
                }
                self.expanded = None;
                self.inline = None;
                self.focus = FocusKind::Commit;
                self.selected_file = None;
                self.lines_dirty = true;
                self.ensure_selection_visible();
            }
            Msg::RefsLoaded {
                branches,
                tags,
                err,
            } => {
                if err.is_none() {
                    self.branches = branches;
                    self.tags = tags;
                }
            }
            Msg::FilesLoaded {
                token,
                anchor_hash,
                a,
                b,
                compare,
                files,
                err,
            } => {
                if let Some(exp) = &mut self.expanded {
                    if exp.token != token {
                        return;
                    }
                    exp.loading = false;
                    exp.err = err;
                    exp.files = files;
                    exp.a = a;
                    exp.b = b;
                    exp.compare = compare;
                    exp.anchor_hash = anchor_hash;
                    self.lines_dirty = true;
                    if !exp.files.is_empty() {
                        self.focus = FocusKind::File;
                        self.selected_file = Some(0);
                    }
                    self.ensure_selection_visible();
                }
            }
            Msg::DiffLoaded {
                token,
                full,
                a,
                b,
                path,
                content,
                err,
                full_file,
            } => {
                if full {
                    if self.full_meta.token != token {
                        return;
                    }
                    self.full_meta.loading = false;
                    self.full_meta.err = err;
                    self.full_meta.full_file = full_file;
                    self.full_meta.a = a;
                    self.full_meta.b = b;
                    self.full_meta.path = path;
                    self.full_meta.content = content;
                    let display_content = if let Some(err) = &self.full_meta.err {
                        format!("{}\n\n{}", err, self.full_meta.content)
                    } else {
                        self.full_meta.content.clone()
                    };
                    self.full_meta.lines = split_lines_trim(&display_content);
                    self.full_meta.change_kinds = diff_change_kinds(&self.full_meta.lines);
                    self.full_meta.change_lines =
                        changed_diff_lines_from_kinds(&self.full_meta.change_kinds);
                    self.full_meta.offset = 0;
                } else if let Some(inline) = &mut self.inline {
                    if inline.token != token {
                        return;
                    }
                    inline.loading = false;
                    inline.err = err;
                    inline.a = a;
                    inline.b = b;
                    inline.path = path;
                    inline.content = content;
                    let mut combined = String::new();
                    if let Some(err) = &inline.err {
                        combined.push_str(err);
                        combined.push_str("\n\n");
                    }
                    combined.push_str(&inline.content);
                    inline.lines = normalize_newlines(&combined)
                        .split('\n')
                        .map(|s| s.to_string())
                        .collect();
                    inline.offset = 0;
                    self.lines_dirty = true;
                    self.ensure_selection_visible();
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if self.prompt.mode != PromptMode::None {
            return self.handle_prompt_key(key);
        }
        if self.mode == AppMode::FullDiff {
            match key_name(key).as_str() {
                "esc" => {
                    self.mode = AppMode::Graph;
                    self.ensure_selection_visible();
                }
                "f" => self.toggle_full_file_diff(),
                "W" => self.toggle_full_diff_whitespace(),
                "g" => self.full_meta.offset = 0,
                "G" => self.full_meta.offset = self.full_diff_max_offset(),
                "J" | "shift+down" => self.jump_full_diff_change(1),
                "K" | "shift+up" => self.jump_full_diff_change(-1),
                "ctrl+c" => return true,
                "up" | "k" => self.scroll_full_diff(-1),
                "down" | "j" => self.scroll_full_diff(1),
                "pgup" => self.scroll_full_diff(-(self.full_diff_height() as isize - 1).max(1)),
                "pgdown" => self.scroll_full_diff((self.full_diff_height() as isize - 1).max(1)),
                _ => {}
            }
            return false;
        }
        match key_name(key).as_str() {
            "ctrl+c" | "q" => return true,
            "r" => {
                self.loading = true;
                self.err = None;
                self.status = "reloading".into();
                self.expanded = None;
                self.inline = None;
                self.compare_a.clear();
                self.compare_b.clear();
                self.lines_dirty = true;
                spawn_load_graph(self.tx.clone(), self.filters.clone(), self.ref_mode);
                spawn_load_refs(self.tx.clone());
            }
            ":" => {
                self.prompt = PromptState {
                    mode: PromptMode::Filter,
                    input: String::new(),
                    suggest_index: 0,
                }
            }
            "/" => {
                self.prompt = PromptState {
                    mode: PromptMode::Search,
                    input: self.search_text.clone(),
                    suggest_index: 0,
                }
            }
            "n" => self.jump_search(1),
            "N" => self.jump_search(-1),
            "F" => {
                self.toggle_ref_filter_mode();
                spawn_load_graph(self.tx.clone(), self.filters.clone(), self.ref_mode);
            }
            "up" | "k" => self.move_selection(-1),
            "down" | "j" => self.move_selection(1),
            "pgup" => {
                if self.inline_scroll_focused() {
                    self.scroll_inline(-(self.inline_diff_height() as isize - 1).max(1));
                } else {
                    self.main_scroll = self
                        .main_scroll
                        .saturating_sub(self.body_height().saturating_sub(1).max(1));
                    self.clamp_scroll();
                }
            }
            "pgdown" => {
                if self.inline_scroll_focused() {
                    self.scroll_inline((self.inline_diff_height() as isize - 1).max(1));
                } else {
                    self.main_scroll += self.body_height().saturating_sub(1).max(1);
                    self.clamp_scroll();
                }
            }
            "home" | "g" => {
                self.selected_commit = 0;
                self.selected_file = None;
                self.focus = FocusKind::Commit;
                self.main_scroll = 0;
            }
            "end" | "G" => {
                self.selected_commit = self.commits.len().saturating_sub(1);
                self.selected_file = None;
                self.focus = FocusKind::Commit;
                self.ensure_selection_visible();
            }
            "enter" | " " => self.activate_selected(),
            "o" => self.open_selected_full_diff(),
            "c" => self.mark_compare(),
            "x" => {
                self.compare_a.clear();
                self.compare_b.clear();
                self.lines_dirty = true;
                if self.expanded.as_ref().map(|e| e.compare).unwrap_or(false) {
                    self.expanded = None;
                    self.inline = None;
                    self.focus = FocusKind::Commit;
                    self.selected_file = None;
                }
            }
            "esc" => {
                if self.inline.is_some() {
                    self.inline = None;
                    self.lines_dirty = true;
                } else if self.expanded.is_some() {
                    self.expanded = None;
                    self.inline = None;
                    self.focus = FocusKind::Commit;
                    self.selected_file = None;
                    self.lines_dirty = true;
                } else if !self.compare_a.is_empty() || !self.compare_b.is_empty() {
                    self.compare_a.clear();
                    self.compare_b.clear();
                    self.lines_dirty = true;
                }
            }
            "K" | "shift+up" => self.scroll_inline(-1),
            "J" | "shift+down" => self.scroll_inline(1),
            _ => {}
        }
        false
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> bool {
        match key_name(key).as_str() {
            "ctrl+c" => return true,
            "esc" => self.prompt = PromptState::default(),
            "enter" => self.apply_prompt(),
            "backspace" => {
                self.prompt.input.pop();
                self.prompt.suggest_index = 0;
            }
            "ctrl+h" => {
                self.prompt.input.pop();
                self.prompt.suggest_index = 0;
            }
            "ctrl+u" => {
                self.prompt.input.clear();
                self.prompt.suggest_index = 0;
            }
            "tab" => self.accept_filter_suggestion(),
            " " => {
                self.prompt.input.push(' ');
                self.prompt.suggest_index = 0;
            }
            _ => {
                if let KeyCode::Char(c) = key.code {
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                        self.prompt.input.push(c);
                        self.prompt.suggest_index = 0;
                    }
                }
            }
        }
        false
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, _x: u16, y: u16) {
        if self.mode == AppMode::FullDiff {
            match kind {
                MouseEventKind::ScrollUp => self.scroll_full_diff(-3),
                MouseEventKind::ScrollDown => self.scroll_full_diff(3),
                _ => {}
            }
            return;
        }
        match kind {
            MouseEventKind::ScrollUp => {
                if self.mouse_over_inline_diff(y as usize) {
                    self.scroll_inline(-3);
                } else {
                    self.main_scroll = self.main_scroll.saturating_sub(3);
                    self.clamp_scroll();
                }
            }
            MouseEventKind::ScrollDown => {
                if self.mouse_over_inline_diff(y as usize) {
                    self.scroll_inline(3);
                } else {
                    self.main_scroll += 3;
                    self.clamp_scroll();
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(idx) = self.line_index_at_y(y as usize) else {
                    return;
                };
                if idx >= self.lines.len() {
                    return;
                }
                let line = self.lines[idx].clone();
                match line.kind {
                    LineKind::Commit => {
                        self.selected_commit = line.commit_idx;
                        self.selected_file = None;
                        self.focus = FocusKind::Commit;
                        self.activate_selected();
                    }
                    LineKind::File => {
                        self.selected_commit = line.commit_idx;
                        self.selected_file = line.file_idx;
                        self.focus = FocusKind::File;
                        self.activate_selected();
                    }
                    LineKind::Diff => {
                        self.selected_commit = line.commit_idx;
                        self.selected_file = line.file_idx;
                        self.focus = FocusKind::File;
                        self.open_selected_full_diff();
                    }
                    LineKind::Text => {}
                }
            }
            _ => {}
        }
    }

    fn apply_prompt(&mut self) {
        let input = self.prompt.input.trim().to_string();
        let mode = self.prompt.mode;
        self.prompt = PromptState::default();
        match mode {
            PromptMode::Search => {
                self.search_text = input;
                if !self.search_text.is_empty() {
                    self.jump_search(1);
                }
            }
            PromptMode::Filter => self.apply_filter_input(&input),
            PromptMode::None => {}
        }
    }

    fn apply_filter_input(&mut self, input: &str) {
        if input.trim().is_empty() {
            self.status = "empty filter".into();
            return;
        }
        let fields: Vec<&str> = input.split_whitespace().collect();
        if fields.is_empty() {
            return;
        }
        let verb = fields[0].to_lowercase();
        if matches!(verb.as_str(), "clear" | "reset") {
            self.filters.clear();
            self.loading = true;
            self.expanded = None;
            self.inline = None;
            self.lines_dirty = true;
            self.status = "filters cleared".into();
            spawn_load_graph(self.tx.clone(), self.filters.clone(), self.ref_mode);
            return;
        }
        if matches!(verb.as_str(), "pop" | "rm" | "remove") {
            if let Some(removed) = self.filters.pop() {
                self.loading = true;
                self.expanded = None;
                self.inline = None;
                self.lines_dirty = true;
                self.status = format!(
                    "removed filter {}={}",
                    filter_kind_label(removed.kind),
                    removed.value
                );
                spawn_load_graph(self.tx.clone(), self.filters.clone(), self.ref_mode);
            } else {
                self.status = "no filters to remove".into();
            }
            return;
        }
        if verb == "mode" && fields.len() > 1 {
            match fields[1].to_lowercase().as_str() {
                "history" | "hist" | "h" => self.ref_mode = RefFilterMode::History,
                "decor" | "decoration" | "decorations" | "d" => {
                    self.ref_mode = RefFilterMode::Decorations
                }
                _ => {}
            }
            self.loading = true;
            self.lines_dirty = true;
            self.status = format!("ref filter mode: {}", ref_mode_label(self.ref_mode));
            spawn_load_graph(self.tx.clone(), self.filters.clone(), self.ref_mode);
            return;
        }
        let (kind, value) = self.parse_filter(input);
        if value.is_empty() {
            self.status = format!("missing value for {} filter", filter_kind_label(kind));
            return;
        }
        self.filters.push(GraphFilter {
            kind,
            value: value.clone(),
        });
        self.loading = true;
        self.expanded = None;
        self.inline = None;
        self.lines_dirty = true;
        self.selected_file = None;
        self.focus = FocusKind::Commit;
        self.status = format!("added filter {}={}", filter_kind_label(kind), value);
        spawn_load_graph(self.tx.clone(), self.filters.clone(), self.ref_mode);
    }

    fn parse_filter(&self, input: &str) -> (FilterKind, String) {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() {
            return (FilterKind::Message, String::new());
        }
        let verb = parts[0].to_lowercase();
        let rest = input
            .strip_prefix(parts[0])
            .unwrap_or("")
            .trim()
            .to_string();
        let (kind, mut value) = match verb.as_str() {
            "b" | "branch" | "branches" => (FilterKind::Branch, rest),
            "t" | "tag" | "tags" => (FilterKind::Tag, rest),
            "m" | "msg" | "message" | "grep" => (FilterKind::Message, rest),
            "a" | "author" => (FilterKind::Author, rest),
            "p" | "path" | "file" => (FilterKind::Path, rest),
            _ => (FilterKind::Message, input.trim().to_string()),
        };
        if matches!(kind, FilterKind::Branch | FilterKind::Tag) {
            let suggestions = self.filter_suggestions_for(kind, &value);
            if !suggestions.is_empty() {
                value = suggestions[min(self.prompt.suggest_index, suggestions.len() - 1)].clone();
            }
        }
        (kind, value.trim().to_string())
    }

    fn accept_filter_suggestion(&mut self) {
        if self.prompt.mode != PromptMode::Filter {
            return;
        }
        let Some((kind, value)) = filter_input_kind_and_value(&self.prompt.input) else {
            return;
        };
        if !matches!(kind, FilterKind::Branch | FilterKind::Tag) {
            return;
        }
        let suggestions = self.filter_suggestions_for(kind, &value);
        if suggestions.is_empty() {
            return;
        }
        let idx = min(self.prompt.suggest_index, suggestions.len() - 1);
        self.prompt.input = replace_filter_value(&self.prompt.input, &suggestions[idx]);
        self.prompt.suggest_index = (idx + 1) % suggestions.len();
    }

    fn filter_suggestions_for(&self, kind: FilterKind, query: &str) -> Vec<String> {
        let source = match kind {
            FilterKind::Branch => &self.branches,
            FilterKind::Tag => &self.tags,
            _ => return vec![],
        };
        source
            .iter()
            .filter(|s| fuzzy_match(query, s))
            .take(8)
            .cloned()
            .collect()
    }

    fn toggle_ref_filter_mode(&mut self) {
        self.ref_mode = if self.ref_mode == RefFilterMode::History {
            RefFilterMode::Decorations
        } else {
            RefFilterMode::History
        };
        self.loading = true;
        self.status = format!("ref filter mode: {}", ref_mode_label(self.ref_mode));
        self.expanded = None;
        self.inline = None;
        self.lines_dirty = true;
    }

    fn jump_search(&mut self, delta: isize) {
        if self.search_text.trim().is_empty() || self.commits.is_empty() {
            self.status = "no search query".into();
            return;
        }
        let query = self.search_text.to_lowercase();
        let start = self.selected_commit as isize;
        let len = self.commits.len() as isize;
        for step in 1..=self.commits.len() {
            let idx = (start + delta * step as isize + len * 2).rem_euclid(len) as usize;
            if strip_ansi(&self.commits[idx].line)
                .to_lowercase()
                .contains(&query)
            {
                self.selected_commit = idx;
                self.selected_file = None;
                self.focus = FocusKind::Commit;
                self.status = format!("search match: /{}", self.search_text);
                self.ensure_selection_visible();
                return;
            }
        }
        self.status = format!("search not found: /{}", self.search_text);
    }

    fn mark_compare(&mut self) {
        if self.focus != FocusKind::Commit || self.selected_commit >= self.commits.len() {
            return;
        }
        let hash = self.commits[self.selected_commit].hash.clone();
        if self.compare_a.is_empty() || (!self.compare_a.is_empty() && !self.compare_b.is_empty()) {
            self.compare_a = hash;
            self.compare_b.clear();
            self.lines_dirty = true;
            return;
        }
        if hash == self.compare_a {
            return;
        }
        self.compare_b = hash.clone();
        self.next_token += 1;
        let token = self.next_token;
        self.expanded = Some(ExpansionState {
            anchor_hash: hash.clone(),
            a: self.compare_a.clone(),
            b: hash.clone(),
            compare: true,
            files: vec![],
            loading: true,
            err: None,
            token,
        });
        self.inline = None;
        self.selected_file = None;
        self.lines_dirty = true;
        spawn_load_files(
            self.tx.clone(),
            token,
            hash.clone(),
            self.compare_a.clone(),
            hash,
            true,
        );
    }

    fn activate_selected(&mut self) {
        if self.focus == FocusKind::File {
            self.toggle_inline_diff();
        } else {
            self.toggle_commit_expansion();
        }
    }

    fn toggle_commit_expansion(&mut self) {
        if self.selected_commit >= self.commits.len() {
            return;
        }
        let hash = self.commits[self.selected_commit].hash.clone();
        if self
            .expanded
            .as_ref()
            .map(|e| e.anchor_hash == hash && !e.compare)
            .unwrap_or(false)
        {
            self.expanded = None;
            self.inline = None;
            self.selected_file = None;
            self.focus = FocusKind::Commit;
            self.lines_dirty = true;
            return;
        }
        self.next_token += 1;
        let token = self.next_token;
        self.expanded = Some(ExpansionState {
            anchor_hash: hash.clone(),
            a: String::new(),
            b: hash.clone(),
            compare: false,
            files: vec![],
            loading: true,
            err: None,
            token,
        });
        self.inline = None;
        self.selected_file = None;
        self.focus = FocusKind::Commit;
        self.lines_dirty = true;
        spawn_load_commit_files(self.tx.clone(), token, hash);
    }

    fn toggle_inline_diff(&mut self) {
        let Some(exp) = &self.expanded else {
            return;
        };
        let Some(idx) = self.selected_file else {
            return;
        };
        if idx >= exp.files.len() {
            return;
        }
        let fc = exp.files[idx].clone();
        let a = exp.a.clone();
        let b = exp.b.clone();
        if self
            .inline
            .as_ref()
            .map(|d| d.path == fc.path && d.a == a && d.b == b)
            .unwrap_or(false)
        {
            self.inline = None;
            self.lines_dirty = true;
            return;
        }
        self.next_token += 1;
        let token = self.next_token;
        let title = format!("{}..{} — {}", short(&a), short(&b), display_path(&fc));
        self.inline = Some(DiffState {
            a: a.clone(),
            b: b.clone(),
            path: fc.path.clone(),
            title,
            loading: true,
            token,
            ..DiffState::default()
        });
        self.lines_dirty = true;
        self.ensure_selection_visible();
        spawn_load_diff(
            self.tx.clone(),
            token,
            false,
            a,
            b,
            fc.path,
            self.diff_width(),
            false,
            false,
            self.delta_theme.clone(),
            self.delta_syntax_theme.clone(),
            self.delta_mode.clone(),
        );
    }

    fn open_selected_full_diff(&mut self) {
        let Some(exp) = &self.expanded else {
            return;
        };
        let Some(idx) = self.selected_file else {
            return;
        };
        if idx >= exp.files.len() {
            return;
        }
        let fc = exp.files[idx].clone();
        self.next_token += 1;
        let token = self.next_token;
        self.full_meta = DiffState {
            a: exp.a.clone(),
            b: exp.b.clone(),
            path: fc.path.clone(),
            title: format!(
                "{}..{} — {}",
                short(&exp.a),
                short(&exp.b),
                display_path(&fc)
            ),
            lines: vec!["loading diff…".into()],
            loading: true,
            token,
            ..DiffState::default()
        };
        self.mode = AppMode::FullDiff;
        spawn_load_diff(
            self.tx.clone(),
            token,
            true,
            exp.a.clone(),
            exp.b.clone(),
            fc.path,
            self.diff_width(),
            false,
            self.full_meta.show_whitespace,
            self.delta_theme.clone(),
            self.delta_syntax_theme.clone(),
            self.delta_mode.clone(),
        );
    }

    fn toggle_full_file_diff(&mut self) {
        if self.full_meta.path.is_empty()
            || self.full_meta.a.is_empty()
            || self.full_meta.b.is_empty()
        {
            return;
        }
        self.next_token += 1;
        self.full_meta.token = self.next_token;
        self.full_meta.full_file = !self.full_meta.full_file;
        self.full_meta.loading = true;
        self.full_meta.err = None;
        self.full_meta.offset = 0;
        self.full_meta.lines = vec!["loading diff…".into()];
        self.full_meta.change_lines.clear();
        self.full_meta.change_kinds.clear();
        spawn_load_diff(
            self.tx.clone(),
            self.full_meta.token,
            true,
            self.full_meta.a.clone(),
            self.full_meta.b.clone(),
            self.full_meta.path.clone(),
            self.diff_width(),
            self.full_meta.full_file,
            self.full_meta.show_whitespace,
            self.delta_theme.clone(),
            self.delta_syntax_theme.clone(),
            self.delta_mode.clone(),
        );
    }

    fn toggle_full_diff_whitespace(&mut self) {
        if self.full_meta.path.is_empty()
            || self.full_meta.a.is_empty()
            || self.full_meta.b.is_empty()
        {
            return;
        }
        self.next_token += 1;
        self.full_meta.token = self.next_token;
        self.full_meta.show_whitespace = !self.full_meta.show_whitespace;
        self.full_meta.loading = true;
        self.full_meta.err = None;
        self.full_meta.offset = 0;
        self.full_meta.lines = vec!["loading diff…".into()];
        self.full_meta.change_lines.clear();
        self.full_meta.change_kinds.clear();
        spawn_load_diff(
            self.tx.clone(),
            self.full_meta.token,
            true,
            self.full_meta.a.clone(),
            self.full_meta.b.clone(),
            self.full_meta.path.clone(),
            self.diff_width(),
            self.full_meta.full_file,
            self.full_meta.show_whitespace,
            self.delta_theme.clone(),
            self.delta_syntax_theme.clone(),
            self.delta_mode.clone(),
        );
    }

    fn jump_full_diff_change(&mut self, delta: isize) {
        if self.full_meta.change_lines.is_empty() {
            return;
        }
        let cur = self.full_meta.offset;
        let mut target = self.full_meta.change_lines[0];
        if delta > 0 {
            for &line in &self.full_meta.change_lines {
                if line > cur {
                    target = line;
                    break;
                }
            }
        } else {
            target = *self.full_meta.change_lines.last().unwrap();
            for &line in self.full_meta.change_lines.iter().rev() {
                if line < cur {
                    target = line;
                    break;
                }
            }
        }
        self.full_meta.offset = min(target, self.full_diff_max_offset());
    }

    fn move_selection(&mut self, delta: isize) {
        if self.lines_dirty {
            self.rebuild_lines();
        }
        if self.lines.is_empty() || delta == 0 {
            return;
        }

        let is_current = |line: &RenderedLine,
                          focus: FocusKind,
                          selected_commit: usize,
                          selected_file: Option<usize>| {
            (line.kind == LineKind::File
                && focus == FocusKind::File
                && line.file_idx == selected_file)
                || (line.kind == LineKind::Commit
                    && focus == FocusKind::Commit
                    && line.commit_idx == selected_commit)
        };

        let mut target_idx = None;
        if delta > 0 {
            let mut seen_current = false;
            for (i, line) in self.lines.iter().enumerate() {
                if !line.selectable {
                    continue;
                }
                if seen_current {
                    target_idx = Some(i);
                    break;
                }
                if is_current(line, self.focus, self.selected_commit, self.selected_file) {
                    seen_current = true;
                    target_idx = Some(i);
                }
            }
        } else {
            let mut prev_selectable = None;
            for (i, line) in self.lines.iter().enumerate() {
                if !line.selectable {
                    continue;
                }
                if is_current(line, self.focus, self.selected_commit, self.selected_file) {
                    target_idx = prev_selectable.or(Some(i));
                    break;
                }
                prev_selectable = Some(i);
            }
        }

        let Some(target_idx) = target_idx else {
            return;
        };
        let line = self.lines[target_idx].clone();
        if line.kind == LineKind::File {
            self.focus = FocusKind::File;
            self.selected_commit = line.commit_idx;
            self.selected_file = line.file_idx;
        } else {
            self.focus = FocusKind::Commit;
            self.selected_commit = line.commit_idx;
            self.selected_file = None;
        }
        self.ensure_selection_visible_in_lines();
    }

    fn rebuild_lines(&mut self) {
        let mut out = Vec::new();
        if self.loading {
            self.lines = vec![RenderedLine {
                text: "loading git graph…".into(),
                kind: LineKind::Text,
                commit_idx: 0,
                file_idx: None,
                selectable: false,
            }];
            self.lines_dirty = false;
            return;
        }
        if let Some(err) = &self.err {
            self.lines = vec![RenderedLine {
                text: err.clone(),
                kind: LineKind::Text,
                commit_idx: 0,
                file_idx: None,
                selectable: false,
            }];
            self.lines_dirty = false;
            return;
        }
        if self.commits.is_empty() {
            self.lines = vec![RenderedLine {
                text: "no commits found".into(),
                kind: LineKind::Text,
                commit_idx: 0,
                file_idx: None,
                selectable: false,
            }];
            self.lines_dirty = false;
            return;
        }
        for i in 0..self.commits.len() {
            let c = &self.commits[i];
            for graph_line in &c.pre_lines {
                out.push(RenderedLine {
                    text: fit_plain(&format!("    {}", graph_line), self.width as usize),
                    kind: LineKind::Text,
                    commit_idx: i,
                    file_idx: None,
                    selectable: false,
                });
            }
            let marker = "  ";
            let badge = if self.compare_a == c.hash {
                "A "
            } else if self.compare_b == c.hash {
                "B "
            } else {
                "  "
            };
            let mut text = format!("{}{}{}", marker, badge, c.line);
            text = fit_plain(&text, self.width as usize);
            out.push(RenderedLine {
                text,
                kind: LineKind::Commit,
                commit_idx: i,
                file_idx: None,
                selectable: true,
            });
            if self
                .expanded
                .as_ref()
                .map(|e| e.anchor_hash == c.hash)
                .unwrap_or(false)
            {
                out.extend(self.expansion_lines(i));
            }
        }
        self.lines = out;
        self.lines_dirty = false;
        self.clamp_scroll();
    }

    fn expansion_lines(&self, commit_idx: usize) -> Vec<RenderedLine> {
        let mut out = Vec::new();
        let prefix = "     ";
        let exp = self.expanded.as_ref().unwrap();
        let head = if exp.compare {
            format!("{}compare {}..{}", prefix, short(&exp.a), short(&exp.b))
        } else {
            format!("{}changed files for {}", prefix, short(&exp.b))
        };
        out.push(RenderedLine {
            text: fit_plain(&head, self.width as usize),
            kind: LineKind::Text,
            commit_idx,
            file_idx: None,
            selectable: false,
        });
        if exp.loading {
            out.push(RenderedLine {
                text: format!("{}loading files…", prefix),
                kind: LineKind::Text,
                commit_idx,
                file_idx: None,
                selectable: false,
            });
            return out;
        }
        if let Some(err) = &exp.err {
            out.push(RenderedLine {
                text: format!("{}{}", prefix, err),
                kind: LineKind::Text,
                commit_idx,
                file_idx: None,
                selectable: false,
            });
            return out;
        }
        if exp.files.is_empty() {
            out.push(RenderedLine {
                text: format!("{}no file changes", prefix),
                kind: LineKind::Text,
                commit_idx,
                file_idx: None,
                selectable: false,
            });
            return out;
        }
        let mut inline_file_idx = None;
        for (i, fc) in exp.files.iter().enumerate() {
            let marker = "  ";
            let caret = if self
                .inline
                .as_ref()
                .map(|d| d.path == fc.path)
                .unwrap_or(false)
            {
                inline_file_idx = Some(i);
                "▾"
            } else {
                "▸"
            };
            let text = format!(
                "{}{} {} {:<5} {}",
                prefix,
                marker,
                caret,
                fc.status,
                display_path(fc)
            );
            out.push(RenderedLine {
                text: fit_plain(&text, self.width as usize),
                kind: LineKind::File,
                commit_idx,
                file_idx: Some(i),
                selectable: true,
            });
        }
        if let Some(idx) = inline_file_idx {
            out.extend(self.inline_diff_lines(commit_idx, idx));
        }
        out
    }

    fn inline_diff_lines(&self, commit_idx: usize, file_idx: usize) -> Vec<RenderedLine> {
        let mut out = Vec::new();
        let Some(inline) = &self.inline else {
            return out;
        };
        let w = max(10, self.width as usize);
        out.push(RenderedLine {
            text: fit_plain(
                &format!(
                    "      ┌─ diff: {} ([/] scroll, o full screen)",
                    inline.title
                ),
                w,
            ),
            kind: LineKind::Diff,
            commit_idx,
            file_idx: Some(file_idx),
            selectable: false,
        });
        if inline.loading {
            out.push(RenderedLine {
                text: "      loading diff…".into(),
                kind: LineKind::Diff,
                commit_idx,
                file_idx: Some(file_idx),
                selectable: false,
            });
            return out;
        }
        let lines = self.inline_diff_content_lines();
        let h = self.inline_diff_viewport_height(lines.len());
        let start = min(inline.offset, lines.len().saturating_sub(h));
        let end = min(lines.len(), start + h);
        for line in &lines[start..end] {
            out.push(RenderedLine {
                text: line.clone(),
                kind: LineKind::Diff,
                commit_idx,
                file_idx: Some(file_idx),
                selectable: false,
            });
        }
        if lines.len() > h {
            out.push(RenderedLine {
                text: format!("      └─ diff lines {}-{}/{}", start + 1, end, lines.len()),
                kind: LineKind::Diff,
                commit_idx,
                file_idx: Some(file_idx),
                selectable: false,
            });
        }
        out
    }

    fn header_lines(&self) -> [String; 2] {
        let left = if self.loading {
            "graft — loading".to_string()
        } else if self.err.is_some() {
            "graft — error".to_string()
        } else {
            format!("graft — {} commits", self.commits.len())
        };
        let mut state = String::new();
        if self.focus == FocusKind::Commit && self.selected_commit < self.commits.len() {
            state.push_str(&format!(
                " selected {}",
                short(&self.commits[self.selected_commit].hash)
            ));
        } else if self.focus == FocusKind::File {
            if let (Some(exp), Some(i)) = (&self.expanded, self.selected_file) {
                if i < exp.files.len() {
                    state.push_str(&format!(" file {}", display_path(&exp.files[i])));
                }
            }
        }
        if !self.compare_a.is_empty() {
            state.push_str(&format!("  compare A={}", short(&self.compare_a)));
            if !self.compare_b.is_empty() {
                state.push_str(&format!(" B={}", short(&self.compare_b)));
            }
        }
        if !self.filters.is_empty() {
            state.push_str(&format!(
                "  filter[{}]: {}",
                ref_mode_label(self.ref_mode),
                filters_label(&self.filters)
            ));
        }
        if !self.search_text.is_empty() {
            state.push_str(&format!("  search: /{}", self.search_text));
        }
        [
            fit_plain(&(left + &state), self.width.saturating_sub(2) as usize),
            fit_plain(
                "↑/↓ navigate  g/G top/end  / search  : filter  F ref-mode  n/N next  enter expand  o diff  q quit",
                self.width as usize,
            ),
        ]
    }

    fn footer_line(&self) -> String {
        if self.prompt.mode != PromptMode::None {
            return self.prompt_line();
        }
        let total = self.lines.len();
        if total == 0 {
            return String::new();
        }
        let body_h = self.body_height();
        let end = min(total, self.main_scroll + body_h);
        let mut footer = format!(
            "lines {}-{}/{}",
            min(total, self.main_scroll + 1),
            end,
            total
        );
        if !self.status.is_empty() {
            footer.push_str(" — ");
            footer.push_str(&self.status);
        }
        fit_plain(&footer, self.width as usize)
    }

    fn prompt_line(&self) -> String {
        match self.prompt.mode {
            PromptMode::Search => {
                fit_plain(&format!("/{}", self.prompt.input), self.width as usize)
            }
            PromptMode::Filter => {
                let mut line = format!(":{}", self.prompt.input);
                if self.prompt.input.trim().is_empty() {
                    line.push_str("  examples: branch main · tag v1.0 · msg fix · author hannes · path main.go · clear · pop");
                } else if let Some((kind, value)) = filter_input_kind_and_value(&self.prompt.input)
                {
                    let suggestions = self.filter_suggestions_for(kind, &value);
                    if !suggestions.is_empty() {
                        line.push_str("  ");
                        line.push_str(&suggestions.join("  "));
                    }
                }
                fit_plain(&line, self.width as usize)
            }
            PromptMode::None => String::new(),
        }
    }

    fn inline_diff_content(&self) -> String {
        let Some(inline) = &self.inline else {
            return String::new();
        };
        let mut content = String::new();
        if let Some(err) = &inline.err {
            content.push_str(err);
            content.push_str("\n\n");
        }
        content.push_str(&inline.content);
        content
    }
    fn inline_diff_content_lines(&self) -> Vec<String> {
        normalize_newlines(&self.inline_diff_content())
            .split('\n')
            .map(|s| s.to_string())
            .collect()
    }
    fn inline_diff_height(&self) -> usize {
        if self.height == 0 {
            12
        } else {
            clamp_usize(self.height as usize / 2, 8, 24)
        }
    }
    fn inline_diff_viewport_height(&self, len: usize) -> usize {
        min(len, self.inline_diff_height())
    }
    fn inline_scroll_focused(&self) -> bool {
        self.inline.is_some() && self.focus == FocusKind::File
    }

    fn scroll_inline(&mut self, delta: isize) {
        if self.inline.is_none() || delta == 0 {
            return;
        }
        let len = self.inline_diff_content_lines().len();
        let h = self.inline_diff_viewport_height(len);
        let max_off = len.saturating_sub(h);
        if let Some(inline) = &mut self.inline {
            let next = clamp_isize(inline.offset as isize + delta, 0, max_off as isize) as usize;
            if next != inline.offset {
                inline.offset = next;
                self.lines_dirty = true;
            }
        }
    }

    fn full_diff_height(&self) -> usize {
        let footer = if self.full_meta.full_file { 2 } else { 1 };
        (self.height as usize).saturating_sub(1 + footer).max(1)
    }
    fn full_diff_max_offset(&self) -> usize {
        self.full_meta
            .lines
            .len()
            .saturating_sub(self.full_diff_height())
    }
    fn scroll_full_diff(&mut self, delta: isize) {
        self.full_meta.offset = clamp_isize(
            self.full_meta.offset as isize + delta,
            0,
            self.full_diff_max_offset() as isize,
        ) as usize;
    }

    fn full_diff_minimap_line(&self) -> String {
        let prefix = "map ";
        let rail_w = (self.width as usize).saturating_sub(prefix.width()).max(1);
        let total = self.full_meta.lines.len();
        if total == 0 {
            return format!("{}{}", prefix, "─".repeat(rail_w));
        }
        let view_start = min(self.full_meta.offset, total);
        let view_end = min(view_start + self.full_diff_height(), total);
        let mut out = String::from(prefix);
        for col in 0..rail_w {
            let bucket_start = col * total / rail_w;
            let mut bucket_end = (col + 1) * total / rail_w;
            if bucket_end <= bucket_start {
                bucket_end = bucket_start + 1;
            }
            let kind = self.full_diff_bucket_change_kind(bucket_start, bucket_end);
            if bucket_start < view_end && bucket_end > view_start {
                out.push('█');
            } else if kind != DiffChangeKind::None {
                out.push('▌');
            } else {
                out.push('─');
            }
        }
        out
    }

    fn full_diff_bucket_change_kind(&self, start: usize, end: usize) -> DiffChangeKind {
        let mut has_add = false;
        let mut has_delete = false;
        let mut has_modify = false;
        for i in start..min(end, self.full_meta.change_kinds.len()) {
            match self.full_meta.change_kinds[i] {
                DiffChangeKind::Add => has_add = true,
                DiffChangeKind::Delete => has_delete = true,
                DiffChangeKind::Modify => has_modify = true,
                DiffChangeKind::None => {}
            }
        }
        if has_modify || (has_add && has_delete) {
            DiffChangeKind::Modify
        } else if has_delete {
            DiffChangeKind::Delete
        } else if has_add {
            DiffChangeKind::Add
        } else {
            DiffChangeKind::None
        }
    }

    fn body_height(&self) -> usize {
        (self.height as usize)
            .saturating_sub(self.header_height + 1)
            .max(1)
    }
    fn diff_width(&self) -> usize {
        if self.width > 0 {
            self.width as usize
        } else {
            terminal_width_fallback()
        }
    }
    fn line_index_at_y(&self, y: usize) -> Option<usize> {
        let body_y = y.checked_sub(self.header_height)?;
        if body_y >= self.body_height() {
            None
        } else {
            Some(self.main_scroll + body_y)
        }
    }
    fn mouse_over_inline_diff(&self, y: usize) -> bool {
        self.line_index_at_y(y)
            .and_then(|idx| self.lines.get(idx))
            .map(|l| l.kind == LineKind::Diff)
            .unwrap_or(false)
    }
    fn ensure_selection_visible(&mut self) {
        if self.lines_dirty {
            self.rebuild_lines();
        }
        self.ensure_selection_visible_in_lines();
    }
    fn ensure_selection_visible_in_lines(&mut self) {
        let idx = self.lines.iter().position(|line| {
            (self.focus == FocusKind::File
                && line.kind == LineKind::File
                && line.file_idx == self.selected_file)
                || (self.focus == FocusKind::Commit
                    && line.kind == LineKind::Commit
                    && line.commit_idx == self.selected_commit)
        });
        let Some(idx) = idx else {
            self.clamp_scroll();
            return;
        };
        let body_h = self.body_height();
        if idx < self.main_scroll {
            self.main_scroll = idx;
        } else if idx >= self.main_scroll + body_h {
            self.main_scroll = idx - body_h + 1;
        }
        self.clamp_scroll();
    }
    fn clamp_scroll(&mut self) {
        let max_scroll = self.lines.len().saturating_sub(self.body_height());
        self.main_scroll = min(self.main_scroll, max_scroll);
    }
}

fn main() -> Result<()> {
    let mut opts = parse_cli(env::args().skip(1))?;
    if opts.help {
        print!("{}", usage_text());
        return Ok(());
    }
    if opts.version {
        println!("{}", version_text());
        return Ok(());
    }
    if !opts.completion.is_empty() {
        print!("{}", completion_script(&opts.completion)?);
        return Ok(());
    }
    if !opts.update_target.is_empty() {
        return run_update(&opts.update_target);
    }
    let cfg = load_app_config()?;
    apply_config_defaults(&mut opts, cfg);
    if !opts.delta_theme.is_empty()
        && (opts.delta_theme_set || opts.delta_syntax_theme.is_empty())
        && is_delta_syntax_theme(&opts.delta_theme)
    {
        opts.delta_syntax_theme = opts.delta_theme.clone();
        opts.delta_theme.clear();
    }
    if opts.save_config {
        let path = write_app_config(&opts)?;
        println!("wrote config: {}", path.display());
        println!("delete this file to reset, or edit it manually");
        return Ok(());
    }
    if let Some(path) = &opts.path {
        env::set_current_dir(path).with_context(|| format!("could not open {}", path.display()))?;
    }
    require_in_path("git", "graft requires git in PATH")?;
    if opts.show_delta_themes {
        return run_delta_passthrough(&["--show-themes"]);
    }
    if opts.list_delta_syntax_themes {
        return run_delta_passthrough(&["--list-syntax-themes"]);
    }
    run_tui(opts)
}

fn run_tui(opts: CliOptions) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let res = tui_loop(&mut terminal, opts);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    res
}

fn tui_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, opts: CliOptions) -> Result<()> {
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
    let mut app = App::new(&opts, tx.clone());
    app.init_load();
    let mut last_draw = Instant::now();
    terminal.draw(|f| render(f, &mut app))?;
    loop {
        let mut dirty = false;
        while let Ok(msg) = rx.try_recv() {
            app.handle_msg(msg);
            dirty = true;
        }

        if event::poll(Duration::from_millis(8))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.handle_key(key) {
                        break;
                    }
                    dirty = true;
                }
                Event::Mouse(mouse) => {
                    app.handle_mouse(mouse.kind, mouse.column, mouse.row);
                    dirty = true;
                }
                Event::Resize(w, h) => {
                    app.width = w;
                    app.height = h;
                    app.lines_dirty = true;
                    app.clamp_scroll();
                    dirty = true;
                }
                _ => {}
            }
        }

        if dirty || last_draw.elapsed() >= Duration::from_millis(100) {
            terminal.draw(|f| render(f, &mut app))?;
            last_draw = Instant::now();
        }
    }
    Ok(())
}

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if app.width != area.width || app.height != area.height {
        app.lines_dirty = true;
    }
    app.width = area.width;
    app.height = area.height;
    if app.mode == AppMode::FullDiff {
        render_full_diff(f, app, area);
    } else {
        render_graph(f, app, area);
    }
}

fn render_graph(f: &mut Frame, app: &mut App, area: Rect) {
    app.header_height = 2;
    if app.lines_dirty {
        app.rebuild_lines();
    }
    app.clamp_scroll();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    let header = app.header_lines();
    Paragraph::new(header[0].clone())
        .style(style_header())
        .render(chunks[0], f.buffer_mut());
    Paragraph::new(header[1].clone())
        .style(style_help())
        .render(chunks[1], f.buffer_mut());
    render_body_lines(f.buffer_mut(), chunks[2], app);
    Paragraph::new(app.footer_line())
        .style(style_help())
        .render(chunks[3], f.buffer_mut());
}

fn render_body_lines(buf: &mut Buffer, area: Rect, app: &App) {
    for y in 0..area.height {
        let idx = app.main_scroll + y as usize;
        if idx >= app.lines.len() {
            continue;
        }
        let line = &app.lines[idx];
        let selected = (line.kind == LineKind::Commit
            && app.focus == FocusKind::Commit
            && line.commit_idx == app.selected_commit)
            || (line.kind == LineKind::File
                && app.focus == FocusKind::File
                && line.file_idx == app.selected_file);
        let rect = Rect::new(area.x, area.y + y, area.width, 1);
        if selected {
            fill_rect(buf, rect, style_selected());
        }
        match line.kind {
            LineKind::Diff => render_ansi_line(buf, rect, &line.text, selected),
            LineKind::Commit => render_commit_line(buf, rect, app, line, selected),
            LineKind::File => {
                render_plain_line(
                    buf,
                    rect,
                    &line.text,
                    if selected {
                        style_selected()
                    } else {
                        Style::default()
                    },
                );
                if selected {
                    set_span_at(buf, rect, 5, ">", style_selected());
                }
            }
            LineKind::Text => {
                if line.text.contains('\x1b') {
                    render_ansi_line(buf, rect, &line.text, false);
                } else {
                    let style = if app.err.is_some() {
                        style_error()
                    } else {
                        style_dim()
                    };
                    render_plain_line(buf, rect, &line.text, style);
                }
            }
        }
    }
}

fn render_commit_line(
    buf: &mut Buffer,
    rect: Rect,
    app: &App,
    line: &RenderedLine,
    selected: bool,
) {
    render_ansi_line(buf, rect, &line.text, selected);
    if selected {
        set_span_at(buf, rect, 0, ">", style_selected());
    }
    let c = &app.commits[line.commit_idx];
    if !c.branch.is_empty() && c.branch_color < BRANCH_COLORS.len() {
        let mut branch_style = Style::default()
            .fg(Color::Indexed(BRANCH_COLORS[c.branch_color]))
            .add_modifier(Modifier::BOLD);
        if selected {
            branch_style = branch_style.bg(Color::Indexed(238));
        }
        if let Some(col) = visible_col_of_match(&line.text, "●") {
            style_span_at(buf, rect, col, "●", branch_style);
        }
        if let Some(col) = visible_col_of_match(&line.text, &c.hash) {
            style_span_at(buf, rect, col, &c.hash, branch_style);
        }
        if !c.branch_badge.is_empty() {
            if let Some(col) = visible_col_of_match(&line.text, &c.branch_badge) {
                style_span_at(buf, rect, col, &c.branch_badge, branch_style);
            }
        }
    }
    if app.compare_a == c.hash {
        let style = if selected {
            style_a().bg(Color::Indexed(238))
        } else {
            style_a()
        };
        set_span_at(buf, rect, 2, "A", style);
    }
    if app.compare_b == c.hash {
        let style = if selected {
            style_b().bg(Color::Indexed(238))
        } else {
            style_b()
        };
        set_span_at(buf, rect, 2, "B", style);
    }
}

fn render_full_diff(f: &mut Frame, app: &mut App, area: Rect) {
    let footer = if app.full_meta.full_file { 2 } else { 1 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(footer),
        ])
        .split(area);
    let mut title = "graft — full diff".to_string();
    if !app.full_meta.title.is_empty() {
        title.push_str(": ");
        title.push_str(&app.full_meta.title);
    }
    title.push_str(if app.full_meta.full_file {
        " [full file]"
    } else {
        " [hunks]"
    });
    Paragraph::new(fit_plain(&title, area.width.saturating_sub(2) as usize))
        .style(style_header())
        .render(chunks[0], f.buffer_mut());
    let lines = &app.full_meta.lines;
    let h = chunks[1].height as usize;
    app.full_meta.offset = min(app.full_meta.offset, lines.len().saturating_sub(h));
    for y in 0..chunks[1].height {
        let idx = app.full_meta.offset + y as usize;
        if idx < lines.len() {
            render_ansi_line(
                f.buffer_mut(),
                Rect::new(chunks[1].x, chunks[1].y + y, chunks[1].width, 1),
                &lines[idx],
                false,
            );
        }
    }
    let mode = if app.full_meta.full_file {
        "full file"
    } else {
        "hunks"
    };
    let ws = if app.full_meta.show_whitespace {
        "on"
    } else {
        "off"
    };
    if app.full_meta.full_file {
        let mini_rect = Rect::new(chunks[2].x, chunks[2].y, chunks[2].width, 1);
        render_minimap_line(f.buffer_mut(), mini_rect, app);
        let help_rect = Rect::new(chunks[2].x, chunks[2].y + 1, chunks[2].width, 1);
        Paragraph::new(fit_plain(&format!("Esc back  g/G top/end  f full file/hunks ({})  W whitespace ({})  Shift-J/K jump changes  ↑/↓ scroll", mode, ws), area.width as usize)).style(style_help()).render(help_rect, f.buffer_mut());
    } else {
        Paragraph::new(fit_plain(&format!("Esc back  g/G top/end  f full file/hunks ({})  W whitespace ({})  Shift-J/K jump changes  ↑/↓ scroll", mode, ws), area.width as usize)).style(style_help()).render(chunks[2], f.buffer_mut());
    }
}

fn render_minimap_line(buf: &mut Buffer, rect: Rect, app: &App) {
    let line = app.full_diff_minimap_line();
    render_plain_line(buf, rect, &line, style_help());
    let prefix_w = "map ".width() as u16;
    let rail_w = rect.width.saturating_sub(prefix_w) as usize;
    let total = app.full_meta.lines.len();
    if total == 0 {
        return;
    }
    let view_start = min(app.full_meta.offset, total);
    let view_end = min(view_start + app.full_diff_height(), total);
    for col in 0..rail_w {
        let bucket_start = col * total / rail_w;
        let mut bucket_end = (col + 1) * total / rail_w;
        if bucket_end <= bucket_start {
            bucket_end = bucket_start + 1;
        }
        let kind = app.full_diff_bucket_change_kind(bucket_start, bucket_end);
        let st = if bucket_start < view_end && bucket_end > view_start {
            match kind {
                DiffChangeKind::Add => style_change_add(),
                DiffChangeKind::Delete => style_change_del(),
                DiffChangeKind::Modify => style_change_mod(),
                DiffChangeKind::None => style_map_viewport(),
            }
        } else {
            match kind {
                DiffChangeKind::Add => style_change_add(),
                DiffChangeKind::Delete => style_change_del(),
                DiffChangeKind::Modify => style_change_mod(),
                DiffChangeKind::None => style_help(),
            }
        };
        if prefix_w + (col as u16) < rect.width {
            buf[(rect.x + prefix_w + col as u16, rect.y)].set_style(st);
        }
    }
}

fn render_plain_line(buf: &mut Buffer, rect: Rect, text: &str, style: Style) {
    Paragraph::new(Text::from(text.to_string()))
        .style(style)
        .block(Block::default())
        .render(rect, buf);
}

fn render_ansi_line(buf: &mut Buffer, rect: Rect, text: &str, selected: bool) {
    if selected {
        fill_rect(buf, rect, style_selected());
    }
    match text.into_text() {
        Ok(mut t) => {
            if selected {
                for line in &mut t.lines {
                    line.style = line.style.bg(Color::Indexed(238));
                    for span in &mut line.spans {
                        span.style = span.style.bg(Color::Indexed(238));
                    }
                }
            }
            Paragraph::new(t).render(rect, buf)
        }
        Err(_) => render_plain_line(
            buf,
            rect,
            &strip_ansi(text),
            if selected {
                style_selected()
            } else {
                Style::default()
            },
        ),
    }
}

fn fill_rect(buf: &mut Buffer, rect: Rect, style: Style) {
    for x in rect.x..rect.x + rect.width {
        buf[(x, rect.y)].set_style(style);
    }
}
fn set_span_at(buf: &mut Buffer, rect: Rect, col: usize, s: &str, style: Style) {
    for (i, ch) in s.chars().enumerate() {
        let x = rect.x + col as u16 + i as u16;
        if x < rect.x + rect.width {
            buf[(x, rect.y)]
                .set_symbol(&ch.to_string())
                .set_style(style);
        }
    }
}

fn style_span_at(buf: &mut Buffer, rect: Rect, col: usize, s: &str, style: Style) {
    for i in 0..s.chars().count() {
        let x = rect.x + col as u16 + i as u16;
        if x < rect.x + rect.width {
            buf[(x, rect.y)].set_style(style);
        }
    }
}

fn visible_col_of_match(text: &str, needle: &str) -> Option<usize> {
    let pos = text.find(needle)?;
    Some(strip_ansi(&text[..pos]).width())
}

fn style_header() -> Style {
    Style::default()
        .fg(Color::Indexed(230))
        .bg(Color::Indexed(62))
        .add_modifier(Modifier::BOLD)
}
fn style_help() -> Style {
    Style::default().fg(Color::Indexed(241))
}
fn style_selected() -> Style {
    Style::default().bg(Color::Indexed(238))
}
fn style_dim() -> Style {
    Style::default().fg(Color::Indexed(244))
}
fn style_error() -> Style {
    Style::default().fg(Color::Indexed(203))
}
fn style_a() -> Style {
    Style::default()
        .fg(Color::Indexed(81))
        .add_modifier(Modifier::BOLD)
}
fn style_b() -> Style {
    Style::default()
        .fg(Color::Indexed(215))
        .add_modifier(Modifier::BOLD)
}
fn style_change_add() -> Style {
    Style::default().fg(Color::Indexed(42))
}
fn style_change_del() -> Style {
    Style::default().fg(Color::Indexed(203))
}
fn style_change_mod() -> Style {
    Style::default().fg(Color::Indexed(220))
}
fn style_map_viewport() -> Style {
    Style::default()
        .fg(Color::Indexed(245))
        .add_modifier(Modifier::BOLD)
}
const BRANCH_COLORS: [u8; 12] = [81, 215, 141, 42, 203, 220, 39, 208, 135, 48, 204, 228];

fn spawn_load_graph(tx: Sender<Msg>, filters: Vec<GraphFilter>, ref_mode: RefFilterMode) {
    thread::spawn(move || {
        let msg = match load_graph(filters, ref_mode) {
            Ok(c) => Msg::GraphLoaded {
                commits: c,
                err: None,
            },
            Err(e) => Msg::GraphLoaded {
                commits: vec![],
                err: Some(e.to_string()),
            },
        };
        let _ = tx.send(msg);
    });
}
fn spawn_load_refs(tx: Sender<Msg>) {
    thread::spawn(move || {
        let msg = match load_refs() {
            Ok((b, t)) => Msg::RefsLoaded {
                branches: b,
                tags: t,
                err: None,
            },
            Err(e) => Msg::RefsLoaded {
                branches: vec![],
                tags: vec![],
                err: Some(e.to_string()),
            },
        };
        let _ = tx.send(msg);
    });
}
fn spawn_load_commit_files(tx: Sender<Msg>, token: u64, hash: String) {
    thread::spawn(move || {
        let msg =
            match commit_range(&hash).and_then(|(a, b)| changed_files(&a, &b).map(|f| (a, b, f))) {
                Ok((a, b, files)) => Msg::FilesLoaded {
                    token,
                    anchor_hash: hash,
                    a,
                    b,
                    compare: false,
                    files,
                    err: None,
                },
                Err(e) => Msg::FilesLoaded {
                    token,
                    anchor_hash: hash.clone(),
                    a: String::new(),
                    b: hash,
                    compare: false,
                    files: vec![],
                    err: Some(e.to_string()),
                },
            };
        let _ = tx.send(msg);
    });
}
fn spawn_load_files(
    tx: Sender<Msg>,
    token: u64,
    anchor: String,
    a: String,
    b: String,
    compare: bool,
) {
    thread::spawn(move || {
        let msg = match changed_files(&a, &b) {
            Ok(files) => Msg::FilesLoaded {
                token,
                anchor_hash: anchor,
                a,
                b,
                compare,
                files,
                err: None,
            },
            Err(e) => Msg::FilesLoaded {
                token,
                anchor_hash: anchor,
                a,
                b,
                compare,
                files: vec![],
                err: Some(e.to_string()),
            },
        };
        let _ = tx.send(msg);
    });
}
#[allow(clippy::too_many_arguments)]
fn spawn_load_diff(
    tx: Sender<Msg>,
    token: u64,
    full: bool,
    a: String,
    b: String,
    path: String,
    width: usize,
    full_file: bool,
    show_ws: bool,
    delta_theme: String,
    delta_syntax_theme: String,
    delta_mode: String,
) {
    thread::spawn(move || {
        let (content, err) = match delta_diff(
            &a,
            &b,
            &path,
            width,
            full_file,
            show_ws,
            &delta_theme,
            &delta_syntax_theme,
            &delta_mode,
        ) {
            Ok(c) => (c, None),
            Err(e) => (e.1, Some(e.0.to_string())),
        };
        let _ = tx.send(Msg::DiffLoaded {
            token,
            full,
            a,
            b,
            path,
            content,
            err,
            full_file,
        });
    });
}

fn load_graph(filters: Vec<GraphFilter>, ref_mode: RefFilterMode) -> Result<Vec<CommitInfo>> {
    ensure_git_repo()?;
    let args = graph_log_args(&filters, ref_mode);
    let out = run_capture("git", &args)?;
    let mut commits = parse_graph(&out);
    annotate_stable_branches(&mut commits);
    if ref_mode == RefFilterMode::Decorations {
        commits = filter_commits_by_decoration(commits, &filters);
    }
    Ok(commits)
}
fn ensure_git_repo() -> Result<()> {
    let out = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()?;
    if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true" {
        Ok(())
    } else {
        Err(anyhow!("not inside a Git repository"))
    }
}
fn graph_log_args(filters: &[GraphFilter], ref_mode: RefFilterMode) -> Vec<String> {
    let format = "%C(auto)%h%C(reset) %C(bold)%s%C(reset)%C(auto)%d%C(reset) %C(dim white)· %ar · %an%C(reset)";
    let mut args = vec![
        "log".into(),
        "--graph".into(),
        "--color=always".into(),
        "--decorate=short".into(),
        "--abbrev=12".into(),
        "--regexp-ignore-case".into(),
        format!("--pretty=format:{}", format),
        "--topo-order".into(),
    ];
    let mut revs = vec![];
    let mut paths = vec![];
    for f in filters {
        match f.kind {
            FilterKind::Message => args.push(format!("--grep={}", f.value)),
            FilterKind::Author => args.push(format!("--author={}", f.value)),
            FilterKind::Path => paths.push(f.value.clone()),
            FilterKind::Branch | FilterKind::Tag => {
                if ref_mode == RefFilterMode::History {
                    revs.push(f.value.clone());
                }
            }
        }
    }
    if revs.is_empty() {
        args.push("--all".into());
    } else {
        args.extend(revs);
    }
    if !paths.is_empty() {
        args.push("--".into());
        args.extend(paths);
    }
    args
}
fn load_refs() -> Result<(Vec<String>, Vec<String>)> {
    ensure_git_repo()?;
    Ok((
        non_empty_lines(&run_capture(
            "git",
            &[
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads",
                "refs/remotes",
            ],
        )?),
        non_empty_lines(&run_capture("git", &["tag", "--list"])?),
    ))
}

fn filter_commits_by_decoration(
    commits: Vec<CommitInfo>,
    filters: &[GraphFilter],
) -> Vec<CommitInfo> {
    let refs: Vec<String> = filters
        .iter()
        .filter(|f| matches!(f.kind, FilterKind::Branch | FilterKind::Tag))
        .map(|f| f.value.to_lowercase())
        .collect();
    if refs.is_empty() {
        return commits;
    }
    commits
        .into_iter()
        .filter(|c| {
            let line = strip_ansi(&c.line).to_lowercase();
            refs.iter().any(|r| line.contains(r))
        })
        .collect()
}
fn parse_graph(out: &str) -> Vec<CommitInfo> {
    let hash_re = Regex::new(r"^[0-9a-fA-F]{7,40}$").unwrap();
    let mut commits = vec![];
    let mut pending = vec![];
    for raw in normalize_newlines(out).lines() {
        let line = raw.trim_end_matches('\r').to_string();
        let plain = strip_ansi(&line);
        if plain.trim().is_empty() {
            continue;
        }
        let hash = plain
            .split_whitespace()
            .map(|f| f.trim_matches(|c: char| "()[],".contains(c)).to_string())
            .find(|f| hash_re.is_match(f));
        let Some(hash) = hash else {
            pending.push(line);
            continue;
        };
        commits.push(CommitInfo {
            hash,
            line: pretty_graph_node(&line),
            pre_lines: pending,
            branch: String::new(),
            branch_color: 0,
            branch_badge: String::new(),
        });
        pending = vec![];
    }
    commits
}

#[derive(Debug)]
struct BranchCandidate {
    reference: String,
    label: String,
    color: usize,
    badge: bool,
}
fn annotate_stable_branches(commits: &mut [CommitInfo]) {
    if commits.is_empty() {
        return;
    }
    let by_hash: HashMap<String, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.hash.clone(), i))
        .collect();
    for branch in branch_color_candidates() {
        let Ok(out) = run_capture(
            "git",
            &[
                "rev-list",
                "--first-parent",
                "--abbrev=12",
                "--abbrev-commit",
                &branch.reference,
            ],
        ) else {
            continue;
        };
        for hash in non_empty_lines(&out) {
            if let Some(&idx) = by_hash.get(&hash) {
                if commits[idx].branch.is_empty() {
                    commits[idx].branch = branch.label.clone();
                    commits[idx].branch_color = branch.color;
                    if branch.badge {
                        commits[idx].branch_badge = format!("[{}]", branch.label);
                        commits[idx].line = commits[idx].line.replacen(
                            &hash,
                            &format!("{} {}", hash, commits[idx].branch_badge),
                            1,
                        );
                    }
                }
            }
        }
    }
}
fn branch_color_candidates() -> Vec<BranchCandidate> {
    let Ok(out) = run_capture(
        "git",
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ],
    ) else {
        return vec![];
    };
    let refs = non_empty_lines(&out);
    let mut ref_by_label = HashMap::new();
    let mut labels = vec![];
    for r in refs {
        if r.ends_with("/HEAD") {
            continue;
        }
        let label = normalize_branch_label(&r);
        if !ref_by_label.contains_key(&label) {
            labels.push(label.clone());
        }
        if !ref_by_label.contains_key(&label) || !r.contains('/') {
            ref_by_label.insert(label, r);
        }
    }
    let mut ordered = vec![];
    if let Some(current) = current_branch_name() {
        ordered.push(normalize_branch_label(&current));
    }
    ordered.extend(
        ["main", "master", "develop", "dev", "trunk"]
            .iter()
            .map(|s| s.to_string()),
    );
    let mut seen = HashMap::<String, bool>::new();
    let mut reserved = HashMap::<usize, bool>::new();
    let mut cands = vec![];
    for label in ordered {
        if seen.contains_key(&label) {
            continue;
        }
        if let Some(reference) = ref_by_label.get(&label) {
            let color = cands.len() % BRANCH_COLORS.len();
            reserved.insert(color, true);
            seen.insert(label.clone(), true);
            cands.push(BranchCandidate {
                reference: reference.clone(),
                label,
                color,
                badge: true,
            });
        }
    }
    for label in labels {
        if seen.contains_key(&label) {
            continue;
        }
        let Some(reference) = ref_by_label.get(&label) else {
            continue;
        };
        let Some(color) = branch_color_for_label_avoiding(&label, &reserved) else {
            continue;
        };
        seen.insert(label.clone(), true);
        cands.push(BranchCandidate {
            reference: reference.clone(),
            label,
            color,
            badge: false,
        });
        if cands.len() >= 32 {
            break;
        }
    }
    cands
}
fn current_branch_name() -> Option<String> {
    run_capture("git", &["branch", "--show-current"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
fn normalize_branch_label(r: &str) -> String {
    r.strip_prefix("origin/")
        .or_else(|| r.strip_prefix("upstream/"))
        .unwrap_or(r)
        .to_string()
}
fn branch_color_for_label_avoiding(label: &str, reserved: &HashMap<usize, bool>) -> Option<usize> {
    if reserved.len() >= BRANCH_COLORS.len() {
        return None;
    }
    let h = label
        .chars()
        .fold(0usize, |h, c| h.wrapping_mul(33).wrapping_add(c as usize));
    let start = h % BRANCH_COLORS.len();
    for i in 0..BRANCH_COLORS.len() {
        let c = (start + i) % BRANCH_COLORS.len();
        if !reserved.contains_key(&c) {
            return Some(c);
        }
    }
    None
}

fn commit_range(hash: &str) -> Result<(String, String)> {
    let out = run_capture("git", &["rev-list", "--parents", "-n", "1", hash])?;
    let fields: Vec<&str> = out.split_whitespace().collect();
    if fields.is_empty() {
        Err(anyhow!("could not resolve commit {}", short(hash)))
    } else if fields.len() == 1 {
        Ok((EMPTY_TREE.into(), fields[0].into()))
    } else {
        Ok((fields[1].into(), fields[0].into()))
    }
}
fn changed_files(a: &str, b: &str) -> Result<Vec<FileChange>> {
    let out = run_capture_bytes("git", &["diff", "--name-status", "-M", "-z", a, b, "--"])?;
    Ok(parse_name_status(&out))
}
fn parse_name_status(out: &[u8]) -> Vec<FileChange> {
    let parts: Vec<String> = String::from_utf8_lossy(out)
        .split('\0')
        .map(|s| s.to_string())
        .collect();
    let mut files = vec![];
    let mut i = 0;
    while i < parts.len() {
        if parts[i].is_empty() {
            i += 1;
            continue;
        }
        let status = parts[i].clone();
        i += 1;
        if status.starts_with('R') || status.starts_with('C') {
            if i + 1 >= parts.len() {
                break;
            }
            let old_path = parts[i].clone();
            let path = parts[i + 1].clone();
            i += 2;
            files.push(FileChange {
                status,
                path,
                old_path,
            });
        } else {
            if i >= parts.len() {
                break;
            }
            let path = parts[i].clone();
            i += 1;
            files.push(FileChange {
                status,
                path,
                old_path: String::new(),
            });
        }
    }
    files
}

#[allow(clippy::too_many_arguments)]
fn delta_diff(
    a: &str,
    b: &str,
    path: &str,
    width: usize,
    full_file: bool,
    show_ws: bool,
    delta_theme: &str,
    delta_syntax_theme: &str,
    delta_mode: &str,
) -> std::result::Result<String, (anyhow::Error, String)> {
    let width = if width == 0 {
        terminal_width_fallback()
    } else {
        width
    };
    let width_arg = width.to_string();
    let context_arg = if full_file { "--unified=1000000" } else { "" };
    let whitespace_arg = if show_ws {
        "--ws-error-highlight=all"
    } else {
        ""
    };
    let whitespace_delta = if show_ws { "1" } else { "0" };
    let script = r##"set -o pipefail
    delta_args=(--side-by-side --line-numbers --paging=never --width "$4")
    if [ "$7" = "1" ]; then delta_args+=(--whitespace-error-style="magenta reverse"); fi
    if [ -n "$8" ]; then delta_args+=(--features "$8"); fi
    if [ -n "$9" ]; then delta_args+=(--syntax-theme "$9"); fi
    if [ "${10}" = "light" ]; then delta_args+=(--light --zero-style 'syntax "#ffffff"'); elif [ "${10}" = "dark" ]; then delta_args+=(--dark); fi
    git diff --color=always $5 $6 "$1" "$2" -- "$3" | delta "${delta_args[@]}""##;
    let out = Command::new("bash")
        .args([
            "-lc",
            script,
            "graft-diff",
            a,
            b,
            path,
            &width_arg,
            context_arg,
            whitespace_arg,
            whitespace_delta,
            delta_theme,
            delta_syntax_theme,
            delta_mode,
        ])
        .env("CLICOLOR_FORCE", "1")
        .env("TERM", "xterm-256color")
        .env("COLUMNS", &width_arg)
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).to_string()),
        Ok(o) => {
            if Command::new("delta")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_err()
            {
                let mut args = vec!["diff".to_string(), "--color=always".to_string()];
                if full_file {
                    args.push("--unified=1000000".into());
                }
                if show_ws {
                    args.push("--ws-error-highlight=all".into());
                }
                args.extend([a.into(), b.into(), "--".into(), path.into()]);
                match Command::new("git")
                    .args(&args)
                    .env("CLICOLOR_FORCE", "1")
                    .env("TERM", "xterm-256color")
                    .output()
                {
                    Ok(raw) if raw.status.success() => Ok(format!(
                        "delta is not installed; showing unified git diff\n\n{}",
                        String::from_utf8_lossy(&raw.stdout)
                    )),
                    Ok(raw) => Err((
                        anyhow!("delta is not installed, and git diff failed"),
                        String::from_utf8_lossy(&raw.stdout).to_string(),
                    )),
                    Err(e) => Err((e.into(), String::new())),
                }
            } else {
                Err((
                    anyhow!("delta diff failed"),
                    String::from_utf8_lossy(&o.stdout).to_string(),
                ))
            }
        }
        Err(e) => Err((e.into(), String::new())),
    }
}

fn parse_cli<I: IntoIterator<Item = String>>(args: I) -> Result<CliOptions> {
    let mut opts = CliOptions::default();
    let args: Vec<String> = args.into_iter().collect();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-h" | "--help" => opts.help = true,
            "--version" | "-version" => opts.version = true,
            "--update" => opts.update_target = "main".into(),
            "--show-delta-themes" => opts.show_delta_themes = true,
            "--save-config" => opts.save_config = true,
            "--delta-light" => {
                opts.delta_mode = "light".into();
                opts.delta_mode_set = true;
            }
            "--delta-dark" => {
                opts.delta_mode = "dark".into();
                opts.delta_mode_set = true;
            }
            "--list-delta-syntax-themes" => opts.list_delta_syntax_themes = true,
            "--delta-theme" => {
                i += 1;
                opts.delta_theme = args
                    .get(i)
                    .ok_or_else(|| anyhow!("missing theme for --delta-theme"))?
                    .clone();
                opts.delta_theme_set = true;
            }
            "--delta-syntax-theme" => {
                i += 1;
                opts.delta_syntax_theme = args
                    .get(i)
                    .ok_or_else(|| anyhow!("missing theme for --delta-syntax-theme"))?
                    .clone();
                opts.delta_syntax_theme_set = true;
            }
            "--completion" => {
                i += 1;
                opts.completion = args
                    .get(i)
                    .ok_or_else(|| anyhow!("missing shell for --completion"))?
                    .clone();
            }
            _ if arg.starts_with("--delta-theme=") => {
                opts.delta_theme = arg.trim_start_matches("--delta-theme=").into();
                opts.delta_theme_set = true;
            }
            _ if arg.starts_with("--delta-syntax-theme=") => {
                opts.delta_syntax_theme = arg.trim_start_matches("--delta-syntax-theme=").into();
                opts.delta_syntax_theme_set = true;
            }
            _ if arg.starts_with("--completion=") => {
                opts.completion = arg.trim_start_matches("--completion=").into()
            }
            _ if arg.starts_with('-') => {
                return Err(anyhow!("unknown option: {}\n{}", arg, usage_text()));
            }
            _ => {
                if opts.path.is_some() {
                    return Err(anyhow!("only one path can be specified"));
                }
                opts.path = Some(PathBuf::from(arg));
            }
        }
        i += 1;
    }
    Ok(opts)
}
fn usage_text() -> &'static str {
    "Usage: graft [options] [path]\n\nOptions:\n  --version                    Print version information and exit\n  --update                     Install latest main branch if it differs\n  --delta-theme <theme>        Activate a delta theme/feature for diff rendering\n  --delta-syntax-theme <theme> Activate a delta syntax theme explicitly\n  --delta-light                Use delta light mode\n  --delta-dark                 Use delta dark mode\n  --save-config                Save current theming options and exit\n  --show-delta-themes          Show available delta themes and exit\n  --list-delta-syntax-themes   List available delta syntax themes\n  --completion <shell>         Print shell completion for bash, zsh, or fish\n  -h, --help                   Show this help\n\nExamples:\n  graft\n  graft ~/src/my-repo\n  graft --version\n  graft --update\n  graft --completion bash\n"
}
fn version_text() -> String {
    format!("graft {}", env!("CARGO_PKG_VERSION"))
}
fn run_update(_target: &str) -> Result<()> {
    require_in_path("cargo", "cargo is required for updates")?;
    require_in_path("git", "git is required for updates")?;

    let latest = remote_main_commit()?;
    match installed_git_commit()? {
        Some(current) if same_commit(&current, &latest) => {
            println!("already up to date: main {}", short(&latest));
            return Ok(());
        }
        Some(current) => println!("updating: {} -> main {}", short(&current), short(&latest)),
        None => println!(
            "installed commit unknown; installing main {}",
            short(&latest)
        ),
    }

    let args = [
        "install",
        "--git",
        MODULE_REPO,
        "--branch",
        "main",
        "--locked",
        "--force",
        "graft",
    ];
    let status = Command::new("cargo").args(args).status()?;
    if status.success() {
        println!("updated to main {}", short(&latest));
        Ok(())
    } else {
        Err(anyhow!("update failed"))
    }
}

fn installed_git_commit() -> Result<Option<String>> {
    let Some(cargo_home) = cargo_home() else {
        return Ok(None);
    };
    for file in [".crates.toml", ".crates2.json"] {
        let path = cargo_home.join(file);
        let Ok(data) = fs::read_to_string(path) else {
            continue;
        };
        if let Some(commit) = parse_installed_commit(&data) {
            return Ok(Some(commit));
        }
    }
    Ok(None)
}

fn cargo_home() -> Option<PathBuf> {
    env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".cargo")))
}

fn parse_installed_commit(data: &str) -> Option<String> {
    for line in data.lines() {
        if !line.contains("graft") || !line.contains("git+") || !line.contains("BananaBites/graft")
        {
            continue;
        }
        let Some((_, after_hash)) = line.rsplit_once('#') else {
            continue;
        };
        let commit: String = after_hash
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if (7..=40).contains(&commit.len()) {
            return Some(commit);
        }
    }
    None
}

fn remote_main_commit() -> Result<String> {
    let out = Command::new("git")
        .args(["ls-remote", MODULE_REPO, "refs/heads/main"])
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "could not query main branch: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split_whitespace()
        .next()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("could not resolve main branch"))
}

fn same_commit(a: &str, b: &str) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

fn config_file_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .ok_or_else(|| anyhow!("could not find user config dir"))?
        .join("graft")
        .join("config"))
}
fn load_app_config() -> Result<AppConfig> {
    let path = config_file_path()?;
    let Ok(data) = fs::read_to_string(path) else {
        return Ok(AppConfig::default());
    };
    let mut cfg = AppConfig::default();
    for line in data.replace("\r\n", "\n").lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "delta-theme" => cfg.delta_theme = v.trim().into(),
            "delta-syntax-theme" => cfg.delta_syntax_theme = v.trim().into(),
            "delta-mode" if matches!(v.trim(), "light" | "dark") => {
                cfg.delta_mode = v.trim().into()
            }
            _ => {}
        }
    }
    Ok(cfg)
}
fn apply_config_defaults(opts: &mut CliOptions, cfg: AppConfig) {
    if !opts.delta_theme_set {
        opts.delta_theme = cfg.delta_theme;
    }
    if !opts.delta_syntax_theme_set {
        opts.delta_syntax_theme = cfg.delta_syntax_theme;
    }
    if !opts.delta_mode_set {
        opts.delta_mode = cfg.delta_mode;
    }
}
fn write_app_config(opts: &CliOptions) -> Result<PathBuf> {
    let path = config_file_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    let mut b = "# graft config\n# delete this file to reset\n".to_string();
    if !opts.delta_theme.is_empty() {
        b.push_str(&format!("delta-theme={}\n", opts.delta_theme));
    }
    if !opts.delta_syntax_theme.is_empty() {
        b.push_str(&format!("delta-syntax-theme={}\n", opts.delta_syntax_theme));
    }
    if !opts.delta_mode.is_empty() {
        b.push_str(&format!("delta-mode={}\n", opts.delta_mode));
    }
    fs::write(&path, b)?;
    Ok(path)
}
fn run_delta_passthrough(args: &[&str]) -> Result<()> {
    require_in_path("delta", "delta is not installed")?;
    let status = Command::new("delta").args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("delta failed"))
    }
}
fn is_delta_syntax_theme(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let Ok(out) = run_capture("delta", &["--list-syntax-themes"]) else {
        return false;
    };
    out.lines()
        .any(|line| line.split_whitespace().last() == Some(name))
}
fn completion_script(shell: &str) -> Result<&'static str> {
    match shell.to_lowercase().as_str() {
        "bash" => Ok(BASH_COMPLETION),
        "zsh" => Ok(ZSH_COMPLETION),
        "fish" => Ok(FISH_COMPLETION),
        _ => Err(anyhow!(
            "unsupported shell {:?}; expected bash, zsh, or fish",
            shell
        )),
    }
}
const BASH_COMPLETION: &str = r#"# bash completion for graft
_graft_completions() {
    local cur prev
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    if [[ "$prev" == "--completion" ]]; then COMPREPLY=( $(compgen -W "bash zsh fish" -- "$cur") ); return 0; fi
    if [[ "$cur" == -* ]]; then COMPREPLY=( $(compgen -W "--help --version --update --delta-theme --delta-syntax-theme --delta-light --delta-dark --save-config --show-delta-themes --list-delta-syntax-themes --completion" -- "$cur") ); return 0; fi
    mapfile -t COMPREPLY < <(compgen -d -S / -- "$cur")
    compopt -o filenames -o nospace 2>/dev/null
}
complete -o filenames -o nospace -F _graft_completions graft
"#;
const ZSH_COMPLETION: &str = "#compdef graft\n\n_arguments \\\n  '(-h --help)'{-h,--help}'[Show help]' \\\n  '--version[Print version information]' \\\n  '--update[Install latest main branch if it differs]' \\\n  '--delta-theme[Activate delta theme/feature]:theme:' \\\n  '--delta-syntax-theme[Activate delta syntax theme]:theme:' \\\n  '--delta-light[Use delta light mode]' \\\n  '--delta-dark[Use delta dark mode]' \\\n  '--save-config[Save current theming options and exit]' \\\n  '--show-delta-themes[Show available delta themes]' \\\n  '--list-delta-syntax-themes[List available delta syntax themes]' \\\n  '--completion[Print shell completion]:shell:(bash zsh fish)' \\\n  '*:directory:_files -/'\n";
const FISH_COMPLETION: &str = "complete -c graft -s h -l help -d 'Show help'\ncomplete -c graft -l version -d 'Print version information'\ncomplete -c graft -l update -d 'Install latest main branch if it differs'\ncomplete -c graft -l delta-theme -x -d 'Activate delta theme/feature'\ncomplete -c graft -l delta-syntax-theme -x -d 'Activate delta syntax theme'\ncomplete -c graft -l delta-light -d 'Use delta light mode'\ncomplete -c graft -l delta-dark -d 'Use delta dark mode'\ncomplete -c graft -l save-config -d 'Save current theming options and exit'\ncomplete -c graft -l show-delta-themes -d 'Show available delta themes'\ncomplete -c graft -l list-delta-syntax-themes -d 'List available delta syntax themes'\ncomplete -c graft -l completion -x -a 'bash zsh fish' -d 'Print shell completion'\ncomplete -c graft -a '(__fish_complete_directories)' -d 'Git repository path'\n";

fn key_name(k: KeyEvent) -> String {
    match k.code {
        KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL) => format!("ctrl+{}", c),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::Up if k.modifiers.contains(KeyModifiers::SHIFT) => "shift+up".into(),
        KeyCode::Down if k.modifiers.contains(KeyModifiers::SHIFT) => "shift+down".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::PageUp => "pgup".into(),
        KeyCode::PageDown => "pgdown".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        _ => String::new(),
    }
}
fn filter_input_kind_and_value(input: &str) -> Option<(FilterKind, String)> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    if parts.len() < 2 {
        return match parts[0].to_lowercase().as_str() {
            "b" | "branch" | "branches" => Some((FilterKind::Branch, String::new())),
            "t" | "tag" | "tags" => Some((FilterKind::Tag, String::new())),
            _ => None,
        };
    }
    let rest = input
        .strip_prefix(parts[0])
        .unwrap_or("")
        .trim()
        .to_string();
    match parts[0].to_lowercase().as_str() {
        "b" | "branch" | "branches" => Some((FilterKind::Branch, rest)),
        "t" | "tag" | "tags" => Some((FilterKind::Tag, rest)),
        _ => None,
    }
}
fn fuzzy_match(query: &str, value: &str) -> bool {
    let q = query.trim().to_lowercase();
    let v = value.to_lowercase();
    if q.is_empty() || v.contains(&q) {
        return true;
    }
    let mut it = q.chars();
    let mut want = it.next();
    for c in v.chars() {
        if Some(c) == want {
            want = it.next();
            if want.is_none() {
                return true;
            }
        }
    }
    false
}
fn replace_filter_value(input: &str, value: &str) -> String {
    input
        .split_whitespace()
        .next()
        .map(|p| format!("{} {}", p, value))
        .unwrap_or_else(|| value.to_string())
}
fn filters_label(filters: &[GraphFilter]) -> String {
    filters
        .iter()
        .map(|f| format!("{}={}", filter_kind_label(f.kind), f.value))
        .collect::<Vec<_>>()
        .join(" ")
}
fn filter_kind_label(k: FilterKind) -> &'static str {
    match k {
        FilterKind::Branch => "branch",
        FilterKind::Tag => "tag",
        FilterKind::Author => "author",
        FilterKind::Path => "path",
        FilterKind::Message => "msg",
    }
}
fn ref_mode_label(m: RefFilterMode) -> &'static str {
    if m == RefFilterMode::Decorations {
        "decor"
    } else {
        "history"
    }
}
fn changed_diff_lines_from_kinds(kinds: &[DiffChangeKind]) -> Vec<usize> {
    let mut out = vec![];
    let mut in_change = false;
    for (i, k) in kinds.iter().enumerate() {
        if *k != DiffChangeKind::None {
            if !in_change {
                out.push(i);
            }
            in_change = true;
        } else {
            in_change = false;
        }
    }
    out
}
fn diff_change_kinds(lines: &[String]) -> Vec<DiffChangeKind> {
    lines
        .iter()
        .map(|line| delta_side_by_side_change_kind(&strip_ansi(line)))
        .collect()
}

fn delta_side_by_side_change_kind(line: &str) -> DiffChangeKind {
    let Some((old_no, old_text, new_no, new_text)) = parse_delta_side_by_side_line(line) else {
        return DiffChangeKind::None;
    };
    if old_no.is_empty() && !new_no.is_empty() {
        DiffChangeKind::Add
    } else if !old_no.is_empty() && new_no.is_empty() {
        DiffChangeKind::Delete
    } else if !old_no.is_empty() && !new_no.is_empty() && old_text != new_text {
        DiffChangeKind::Modify
    } else {
        DiffChangeKind::None
    }
}

fn parse_delta_side_by_side_line(line: &str) -> Option<(&str, &str, &str, &str)> {
    let rest = line.strip_prefix('│')?;
    let (old_no_raw, rest) = rest.split_once('│')?;
    let old_no = old_no_raw.trim();
    if !old_no.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // Old content may itself contain box-drawing characters, even followed by
    // text that looks like a line-number column (`│ 586│`). Delta's real middle
    // delimiter is the last valid delimiter before the new line-number column.
    let mut best = None;
    let mut search_from = 0;
    while let Some(rel) = rest[search_from..].find('│') {
        let mid = search_from + rel;
        let after_mid = &rest[mid + '│'.len_utf8()..];
        if let Some((new_no_raw, new_text_raw)) = after_mid.split_once('│') {
            let new_no = new_no_raw.trim();
            if new_no.chars().all(|c| c.is_ascii_digit()) {
                let old_text = rest[..mid].trim_end_matches([' ', '\t']);
                let new_text = new_text_raw.trim_end_matches([' ', '\t']);
                best = Some((old_no, old_text, new_no, new_text));
            }
        }
        search_from = mid + '│'.len_utf8();
    }
    best
}

fn run_capture<S: AsRef<str>>(cmd: &str, args: &[S]) -> Result<String> {
    let out = run_capture_bytes(cmd, args)?;
    Ok(String::from_utf8_lossy(&out).to_string())
}
fn run_capture_bytes<S: AsRef<str>>(cmd: &str, args: &[S]) -> Result<Vec<u8>> {
    let args_vec: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();
    let out = Command::new(cmd).args(&args_vec).output()?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(anyhow!(
            "{} failed: {}",
            cmd,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
fn require_in_path(bin: &str, msg: &str) -> Result<()> {
    match Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => Ok(()),
        Err(_) => Err(anyhow!(msg.to_string())),
    }
}
fn non_empty_lines(out: &str) -> Vec<String> {
    normalize_newlines(out)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}
fn terminal_width_fallback() -> usize {
    env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(120)
}
fn split_lines_trim(s: &str) -> Vec<String> {
    let s = normalize_newlines(s).trim_end_matches('\n').to_string();
    if s.is_empty() {
        vec![]
    } else {
        s.split('\n').map(|s| s.to_string()).collect()
    }
}
fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}
fn pretty_graph_node(line: &str) -> String {
    if let Some(i) = line.find('*') {
        let mut s = line.to_string();
        s.replace_range(i..=i, "●");
        s
    } else {
        line.to_string()
    }
}
fn strip_ansi(s: &str) -> String {
    if !s.as_bytes().contains(&0x1b) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() {
                let b = bytes[i];
                i += 1;
                if (b'@'..=b'~').contains(&b) {
                    break;
                }
            }
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}
fn display_path(fc: &FileChange) -> String {
    if !fc.old_path.is_empty() && fc.old_path != fc.path {
        format!("{} → {}", fc.old_path, fc.path)
    } else {
        fc.path.clone()
    }
}
fn short(hash: &str) -> String {
    if hash.len() <= 12 {
        hash.into()
    } else {
        hash[..12].into()
    }
}
fn fit_plain(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if strip_ansi(s).width() <= width {
        return s.to_string();
    }
    if width == 1 {
        return "…".into();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in strip_ansi(s).chars() {
        let cw = ch.to_string().width();
        if w + cw > width - 1 {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}
fn clamp_usize(v: usize, lo: usize, hi: usize) -> usize {
    min(max(v, lo), hi)
}
fn clamp_isize(v: isize, lo: isize, hi: isize) -> isize {
    min(max(v, lo), hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn side_by_side_change_detection() {
        assert_eq!(
            delta_side_by_side_change_kind(
                "│  1 │one                                                   │  1 │one"
            ),
            DiffChangeKind::None
        );
        assert_ne!(
            delta_side_by_side_change_kind(
                "│  2 │two old                                               │    │"
            ),
            DiffChangeKind::None
        );
        assert_ne!(
            delta_side_by_side_change_kind(
                "│    │                                                      │  2 │two new"
            ),
            DiffChangeKind::None
        );
        assert_ne!(
            delta_side_by_side_change_kind(
                "│  2 │two old                                               │  2 │two new"
            ),
            DiffChangeKind::None
        );
        assert_eq!(
            delta_side_by_side_change_kind(
                "│    │continued old                                         │    │continued new"
            ),
            DiffChangeKind::None
        );
        assert_eq!(
            delta_side_by_side_change_kind(
                "│571 │            \"commit 94907c0 │\",                  │571 │            \"commit 94907c0 │\","
            ),
            DiffChangeKind::None
        );
        assert_eq!(
            delta_side_by_side_change_kind(
                "│571 │            \"commit 94907c0 │\",                  │571 │            \"commit abcdef0 │\","
            ),
            DiffChangeKind::Modify
        );
    }
    #[test]
    fn parse_graph_preserves_connector_lines() {
        let got = parse_graph(
            "* abc1234 commit on main\n| * def5678 commit on branch\n|/  \n* 123abcd merge base\n",
        );
        assert_eq!(got.len(), 3);
        assert_eq!(got[2].pre_lines, vec!["|/  ".to_string()]);
    }
}
