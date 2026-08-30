//! Unified command palette model: a single, pure (no gpui) state machine.
//!
//! The palette opens at the ROOT, listing every [`Action`] grouped by
//! [`action_group`]. [`Action::NewGroup`] is the one multi-step command:
//! entering it walks steps Repo → Base → Layout → Done, each pick collapsing
//! into a token chip; ⌫ on an empty query pops the last token and returns to
//! that step. The same flow can be opened directly at the Repo step
//! (⇧⌘T / the sidebar ＋ button), optionally for the flyover terminal
//! (which skips Layout and launches right after Base, or right after Repo
//! for a non-git directory).

use std::path::Path;

use crate::pages::Action;
use crate::palette::fuzzy_match;
use crate::picker::{ForkEntry, ForkPicker, Picker, PickerEntry, ProfileEntry, ProfilePicker};

/// Which step the palette is on: the command root or one of the three
/// "New session" picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The command list: every [`Action`], grouped and fuzzy-filterable.
    Root,
    /// Step 1: which checkout the group spawns in.
    Repo,
    /// Step 2 (git repos): which fork source the worktree forks off of.
    Base,
    /// Step 3: which saved workspace profile the group opens with.
    Layout,
    /// The flow is complete: the summary card is showing.
    Done,
}

/// One chip in the command line: the command itself or an argument pick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// The `New session` command chip.
    Command(&'static str),
    /// A collapsed step pick (repo / base / layout).
    Arg { kind: &'static str, label: String },
}

/// Rail state for one of the three "New session" steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    /// Not reached yet.
    Pending,
    /// The current step.
    Active,
    /// Picked and collapsed into a token.
    Done,
}

/// One row of the root command list: a group header or an action.
#[derive(Clone, Debug, PartialEq)]
pub enum RootRow {
    /// A group caption row (not selectable).
    Header(&'static str),
    /// A runnable command.
    Action(Action),
}

/// Everything the flow collected for one new session.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSession {
    /// The group's name (derived from the repo path).
    pub name: String,
    /// The checkout the group spawns in.
    pub repo: PickerEntry,
    /// The fork source, or `None` for a non-git repo or the flyover path.
    pub base: Option<ForkEntry>,
    /// The workspace profile, or `None` for the default (single pane).
    pub layout: Option<ProfileEntry>,
    /// Whether the group opens in the flyover terminal.
    pub for_flyover: bool,
}

/// What confirming (↩) or popping (⌫ on an empty query) did.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Nothing happened (empty query at Root, or a completed flow).
    Nothing,
    /// A one-shot command runs.
    Run(Action),
    /// The repo pick completed (git repos: the caller supplies a
    /// [`ForkPicker`] via [`CommandPalette::provide_base`]; non-git: a
    /// [`ProfilePicker`] via [`CommandPalette::provide_layout`]).
    RepoChosen(PickerEntry),
    /// The fork-source pick completed (the caller supplies a
    /// [`ProfilePicker`] via [`CommandPalette::provide_layout`]).
    BaseChosen(ForkEntry),
    /// The flow completed: launch the session.
    Launch(NewSession),
    /// The palette closed (pop at the Repo step of a flow opened directly).
    Close,
}

/// The unified command palette: pure state, no gpui.
pub struct CommandPalette {
    /// Which step is showing.
    pub stage: Stage,
    /// Whether the resulting session opens in the flyover terminal.
    pub for_flyover: bool,
    /// Whether the palette was opened at the root (⌘K) rather than
    /// directly in the flow (⇧⌘T).
    pub opened_at_root: bool,
    /// The root command list's query.
    pub root_query: String,
    /// The root command list's highlight (indexes `root_rows`).
    pub root_selected: usize,
    /// The root command list's visible rows (headers + actions).
    pub root_rows: Vec<RootRow>,
    /// Step 1's model, once the flow is entered.
    pub repo: Option<Picker>,
    /// Step 2's model, once the repo is chosen (git repos).
    pub base: Option<ForkPicker>,
    /// Step 3's model, once the fork source (or repo, non-git) is chosen.
    pub layout: Option<ProfilePicker>,
    /// The group name (derived from the repo path).
    pub name: String,
    /// The picked repo, once chosen.
    pub repo_choice: Option<PickerEntry>,
    /// The picked fork source, once chosen.
    pub base_choice: Option<ForkEntry>,
    /// The picked workspace profile, once chosen.
    pub layout_choice: Option<ProfileEntry>,
}

impl CommandPalette {
    #[allow(dead_code)] // consumed by command_ui (in flight)
    /// Opens the palette at the ROOT: every action, grouped.
    pub fn root() -> Self {
        let mut palette = Self {
            stage: Stage::Root,
            for_flyover: false,
            opened_at_root: true,
            root_query: String::new(),
            root_selected: 0,
            root_rows: Vec::new(),
            repo: None,
            base: None,
            layout: None,
            name: String::new(),
            repo_choice: None,
            base_choice: None,
            layout_choice: None,
        };
        palette.rebuild_root("");
        palette
    }

    /// Opens the palette directly in the "New session" flow, at the Repo
    /// step. The flyover variant skips Layout and launches right after Base
    /// (or right after Repo for a non-git directory).
    pub fn new_session(for_flyover: bool) -> Self {
        let mut palette = Self {
            stage: Stage::Repo,
            for_flyover,
            opened_at_root: false,
            root_query: String::new(),
            root_selected: 0,
            root_rows: Vec::new(),
            repo: None,
            base: None,
            layout: None,
            name: String::new(),
            repo_choice: None,
            base_choice: None,
            layout_choice: None,
        };
        palette.rebuild_root("");
        palette
    }

    /// A flow that starts at the Layout step for a directory already chosen
    /// (a folder opened from outside the app): the repo token is committed,
    /// the layout picker is up, and ⌫ pops back to the repo step.
    pub fn for_directory(entry: PickerEntry, layout: ProfilePicker) -> Self {
        let mut palette = Self::new_session(false);
        palette.name = group_name(&entry.path);
        palette.repo_choice = Some(entry);
        palette.provide_layout(layout);
        palette
    }

    /// Hand the Repo step its directory list. The model never scans the
    /// filesystem itself; the app provides the picker whenever the flow
    /// lands on this step (`App::ensure_command_repo`), tests inject fixtures.
    pub fn provide_repo(&mut self, repo: Picker) {
        self.repo = Some(repo);
    }

    /// True when the flow sits on the Repo step without a list yet.
    pub fn needs_repo(&self) -> bool {
        self.stage == Stage::Repo && self.repo.is_none()
    }

    /// From the root: enter the "New session" flow at the Repo step.
    /// (Root → Repo.)
    pub fn enter_new_session(&mut self) {
        self.stage = Stage::Repo;
        self.opened_at_root = true;
        self.repo = None;
        self.base = None;
        self.layout = None;
        self.name = String::new();
        self.repo_choice = None;
        self.base_choice = None;
        self.layout_choice = None;
    }

    /// Rebuilds `root_rows` from `root_query`: with an empty query, every
    /// group in order with all of its actions (`CommandPalette` excluded);
    /// with a query, only fuzzy-matched labels, still grouped.
    fn rebuild_root(&mut self, query: &str) {
        self.root_query = query.to_string();
        self.root_rows = Vec::new();
        for group in ["Session", "Tiles", "Navigate", "Edit", "View", "App"] {
            let matched: Vec<Action> = Action::ALL
                .iter()
                .copied()
                .filter(|a| *a != Action::CommandPalette)
                .filter(|a| action_group(*a) == group && fuzzy_match(a.label(), query))
                .collect();
            if matched.is_empty() {
                continue;
            }
            self.root_rows.push(RootRow::Header(group));
            self.root_rows.extend(matched.into_iter().map(RootRow::Action));
        }
        self.root_selected = self.first_action_row();
    }

    /// The first Action row (the selection reset target after a refilter).
    fn first_action_row(&self) -> usize {
        self.root_rows
            .iter()
            .position(|r| matches!(r, RootRow::Action(_)))
            .unwrap_or(0)
    }

    /// The action currently highlighted on the root list, if any.
    fn selected_action(&self) -> Option<Action> {
        match self.root_rows.get(self.root_selected) {
            Some(RootRow::Action(action)) => Some(*action),
            _ => None,
        }
    }

    /// The current stage's query (`""` on Done).
    pub fn query(&self) -> &str {
        match self.stage {
            Stage::Root => &self.root_query,
            Stage::Repo => self.repo.as_ref().map(|p| p.query.as_str()).unwrap_or(""),
            Stage::Base => self.base.as_ref().map(|p| p.query.as_str()).unwrap_or(""),
            Stage::Layout => self.layout.as_ref().map(|p| p.query.as_str()).unwrap_or(""),
            Stage::Done => "",
        }
    }

    /// Replaces the current stage's query and refilters.
    pub fn set_query(&mut self, q: &str) {
        match self.stage {
            Stage::Root => self.rebuild_root(q),
            Stage::Repo => {
                if let Some(picker) = self.repo.as_mut() {
                    picker.set_query(q);
                }
            }
            Stage::Base => {
                if let Some(picker) = self.base.as_mut() {
                    picker.set_query(q);
                }
            }
            Stage::Layout => {
                if let Some(picker) = self.layout.as_mut() {
                    picker.set_query(q);
                }
            }
            Stage::Done => {},
        }
    }

    /// The current stage's highlight.
    pub fn selected(&self) -> usize {
        match self.stage {
            Stage::Root => self.root_selected,
            Stage::Repo => self.repo.as_ref().map(|p| p.selected).unwrap_or(0),
            Stage::Base => self.base.as_ref().map(|p| p.selected).unwrap_or(0),
            Stage::Layout => self.layout.as_ref().map(|p| p.selected).unwrap_or(0),
            Stage::Done => 0,
        }
    }

    /// Moves the current stage's highlight by `delta` rows. On the root it
    /// skips header rows; elsewhere the stage model handles it (the fork and
    /// profile pickers have no headers).
    pub fn move_selection(&mut self, delta: isize) {
        match self.stage {
            Stage::Root => {
                // Each unit of `delta` is one Action row in that direction;
                // headers are skipped and the ends clamp.
                let step = delta.signum();
                let mut remaining = delta.unsigned_abs();
                let mut i = self.root_selected;
                while remaining > 0 {
                    let mut j = i as isize;
                    let next = loop {
                        j += step;
                        if j < 0 || j >= self.root_rows.len() as isize {
                            break None;
                        }
                        if matches!(self.root_rows[j as usize], RootRow::Action(_)) {
                            break Some(j as usize);
                        }
                    };
                    match next {
                        Some(j) => i = j,
                        None => break,
                    }
                    remaining -= 1;
                }
                self.root_selected = i;
            }
            Stage::Repo => {
                if let Some(picker) = self.repo.as_mut() {
                    picker.move_selection(delta);
                }
            }
            Stage::Base => {
                if let Some(picker) = self.base.as_mut() {
                    picker.move_selection(delta);
                }
            }
            Stage::Layout => {
                if let Some(picker) = self.layout.as_mut() {
                    picker.move_selection(delta);
                }
            }
            Stage::Done => {},
        }
    }

    /// Highlights row `index` of the current stage (clamped to the list).
    pub fn select(&mut self, index: usize) {
        match self.stage {
            Stage::Root => {
                if !self.root_rows.is_empty() {
                    let index = index.min(self.root_rows.len() - 1);
                    if !matches!(self.root_rows[index], RootRow::Action(_)) {
                        return;
                    }
                    self.root_selected = index;
                }
            }
            Stage::Repo => {
                if let Some(picker) = self.repo.as_mut() {
                    picker.select(index);
                }
            }
            Stage::Base => {
                if let Some(picker) = self.base.as_mut() {
                    picker.select(index);
                }
            }
            Stage::Layout => {
                if let Some(picker) = self.layout.as_mut() {
                    picker.select(index);
                }
            }
            Stage::Done => {},
        }
    }

    /// The chips collected so far: the command once past Root, then one Arg
    /// per completed pick.
    pub fn tokens(&self) -> Vec<Token> {
        let mut tokens = Vec::new();
        if self.stage != Stage::Root {
            tokens.push(Token::Command("New session"));
        }
        if let Some(repo) = &self.repo_choice {
            tokens.push(Token::Arg { kind: "repo", label: repo.label.clone() });
        }
        if let Some(base) = &self.base_choice {
            tokens.push(Token::Arg { kind: "base", label: base.label.clone() });
        }
        if let Some(layout) = &self.layout_choice {
            tokens.push(Token::Arg { kind: "layout", label: layout.label.clone() });
        }
        tokens
    }

    /// The step rail: `("Repository", ..)`, `("Base", ..)`, `("Layout", ..)`
    /// with each step's state.
    pub fn steps(&self) -> [(&'static str, StepState); 3] {
        // A step is Done once it holds a pick, Active while it is the stage,
        // and Pending otherwise — so a skipped Base (non-git repo) never
        // shows a ✓ it did not earn.
        let state = |stage: Stage, picked: bool| {
            if self.stage == Stage::Done {
                // The receipt: every step reads as settled once the summary
                // card is up, skipped or not.
                StepState::Done
            } else if self.stage == stage {
                StepState::Active
            } else if picked {
                StepState::Done
            } else {
                StepState::Pending
            }
        };
        [
            ("Repository", state(Stage::Repo, self.repo_choice.is_some())),
            ("Base", state(Stage::Base, self.base_choice.is_some())),
            ("Layout", state(Stage::Layout, self.layout_choice.is_some())),
        ]
    }

    /// Whether the flow (and so the step rail) is showing.
    pub fn in_flow(&self) -> bool {
        matches!(self.stage, Stage::Repo | Stage::Base | Stage::Layout)
    }

    /// The search field's placeholder for the current stage.
    pub fn placeholder(&self) -> &'static str {
        match self.stage {
            Stage::Root => "Run a command…",
            Stage::Repo => "Search repos…",
            Stage::Base => "Filter branches & worktrees…",
            Stage::Layout => "Pick a layout…",
            Stage::Done => "",
        }
    }

    /// The status line under the search field.
    pub fn footer(&self) -> String {
        match self.stage {
            Stage::Root => {
                let commands = self.root_rows.iter().filter(|r| matches!(r, RootRow::Action(_))).count();
                format!("root · {commands} commands")
            }
            Stage::Repo => "New session · step 1 of 3".into(),
            Stage::Base => "New session · step 2 of 3".into(),
            Stage::Layout => "New session · step 3 of 3".into(),
            Stage::Done => "ready".into(),
        }
    }

    /// Supplies the fork picker after [`Outcome::RepoChosen`] for a git
    /// repo: stage moves to Base.
    pub fn provide_base(&mut self, base: ForkPicker) {
        self.base = Some(base);
        self.stage = Stage::Base;
    }

    /// Supplies the profile picker after [`Outcome::RepoChosen`] (non-git)
    /// or [`Outcome::BaseChosen`]: stage moves to Layout.
    pub fn provide_layout(&mut self, layout: ProfilePicker) {
        self.layout = Some(layout);
        self.stage = Stage::Layout;
    }

    /// ⌫ on an empty query: pop the last token and go back one step.
    /// Never pops when the query is non-empty.
    pub fn pop(&mut self) -> Outcome {
        if !self.query().is_empty() {
            return Outcome::Nothing;
        }
        match self.stage {
            Stage::Done => {
                self.stage = Stage::Layout;
                self.layout_choice = None;
                Outcome::Nothing
            }
            Stage::Layout => {
                // The last token is the base pick (or the repo pick when the
                // repo had no base step): pop it and return to that step.
                self.layout_choice = None;
                if self.base.is_some() {
                    self.stage = Stage::Base;
                    self.base_choice = None;
                } else {
                    self.stage = Stage::Repo;
                    self.repo_choice = None;
                }
                Outcome::Nothing
            }
            Stage::Base => {
                self.stage = Stage::Repo;
                self.base_choice = None;
                self.repo_choice = None;
                Outcome::Nothing
            }
            Stage::Repo => {
                if self.opened_at_root {
                    self.stage = Stage::Root;
                    self.repo = None;
                    self.repo_choice = None;
                    self.base_choice = None;
                    self.layout_choice = None;
                    let query = self.root_query.clone();
                    self.rebuild_root(&query);
                    Outcome::Nothing
                } else {
                    Outcome::Close
                }
            }
            Stage::Root => Outcome::Nothing,
        }
    }

    /// Confirming (↩) on the current stage's selection.
    pub fn enter(&mut self) -> Outcome {
        match self.stage {
            Stage::Root => {
                let Some(action) = self.selected_action() else {
                    return Outcome::Nothing;
                };
                if is_multi_step(action) {
                    self.enter_new_session();
                    Outcome::Nothing
                } else {
                    Outcome::Run(action)
                }
            }
            Stage::Repo => {
                let choice = self.repo.as_ref().and_then(Picker::selected_entry).cloned();
                let Some(choice) = choice else {
                    return Outcome::Nothing;
                };
                self.name = group_name(&choice.path);
                self.repo_choice = Some(choice.clone());
                Outcome::RepoChosen(choice)
            }
            Stage::Base => {
                let choice = self.base.as_ref().and_then(ForkPicker::selected_entry).cloned();
                let Some(choice) = choice else {
                    return Outcome::Nothing;
                };
                self.base_choice = Some(choice.clone());
                Outcome::BaseChosen(choice)
            }
            Stage::Layout => {
                let choice = self.layout.as_ref().and_then(ProfilePicker::selected_entry).cloned();
                let Some(choice) = choice else {
                    return Outcome::Nothing;
                };
                self.layout_choice = Some(choice);
                self.stage = Stage::Done;
                Outcome::Nothing
            }
            Stage::Done => Outcome::Launch(self.finish()),
        }
    }

    /// Collects the flow's picks into the launch payload.
    fn finish(&self) -> NewSession {
        NewSession {
            name: self.name.clone(),
            repo: self
                .repo_choice
                .clone()
                .expect("launch without a repo pick"),
            base: self.base_choice.clone(),
            layout: self.layout_choice.clone(),
            for_flyover: self.for_flyover,
        }
    }

    /// The Done card: `(headline, status)`.
    pub fn summary(&self) -> (String, String) {
        let base_label = self
            .base_choice
            .as_ref()
            .map(|b| b.label.clone())
            .unwrap_or_else(|| "main".into());
        let layout_label = self
            .layout_choice
            .as_ref()
            .map(|l| l.label.clone())
            .unwrap_or_else(|| "Default".into());
        let headline = format!("{} ⎇ {} · {}", self.name, base_label, layout_label);
        let status = if let Some(base) = &self.base_choice {
            if matches!(base.scope, crate::picker::ForkScope::RepoRoot | crate::picker::ForkScope::Worktree) {
                "Opens in the existing checkout · processes start on launch".into()
            } else {
                "New worktree will be created · processes start on launch".into()
            }
        } else if let Some(repo) = &self.repo_choice {
            if repo.is_git {
                "New worktree will be created · processes start on launch".into()
            } else {
                format!("Opens {}", repo.path.display())
            }
        } else {
            "New worktree will be created · processes start on launch".into()
        };
        (headline, status)
    }
}

/// The group an [`Action`] belongs to (drives root-list headers).
pub fn action_group(action: Action) -> &'static str {
    match action {
        Action::NewGroup
        | Action::CloseGroup
        | Action::TogglePin
        | Action::SaveWorkspace
        | Action::NewSection => "Session",
        Action::SplitRight
        | Action::SplitDown
        | Action::NewTab
        | Action::CloseTab
        | Action::ToggleCollapse
        | Action::ToggleFocusOthers => "Tiles",
        Action::PrevTile
        | Action::NextTile
        | Action::PrevTab
        | Action::NextTab
        | Action::FocusLeft
        | Action::FocusDown
        | Action::FocusUp
        | Action::FocusRight
        | Action::PrevSidebarTab
        | Action::NextSidebarTab
        | Action::PrevPage
        | Action::NextPage => "Navigate",
        Action::Copy | Action::Paste => "Edit",
        Action::ToggleSidebar
        | Action::ToggleFlyover
        | Action::FlyoverPopout
        | Action::ToggleToolPanel
        | Action::IncreaseFontSize
        | Action::DecreaseFontSize
        | Action::OpenSettings => "View",
        Action::Quit
        | Action::CommandPalette
        | Action::ScreenshotToClipboard
        | Action::ScreenshotToFile
        | Action::GoToSessions
        | Action::GoToPullRequests
        | Action::GoToCleanup => "App",
    }
}

/// A short glyph for each action (the root list's leading column).
pub fn action_glyph(action: Action) -> &'static str {
    match action {
        Action::NewGroup => "✎",
        Action::CloseGroup => "⨯",
        Action::TogglePin => "⚑",
        Action::SaveWorkspace => "⎙",
        Action::SplitRight => "◫",
        Action::SplitDown => "⬒",
        Action::NewTab => "＋",
        Action::CloseTab => "⌦",
        Action::ToggleCollapse => "⌃",
        Action::ToggleFocusOthers => "◎",
        Action::PrevTile => "⊙",
        Action::NextTile => "⊙",
        Action::PrevTab => "‹",
        Action::NextTab => "›",
        Action::FocusLeft => "←",
        Action::FocusDown => "↓",
        Action::FocusUp => "↑",
        Action::FocusRight => "→",
        Action::PrevSidebarTab => "⇤",
        Action::NextSidebarTab => "⇥",
        Action::PrevPage => "⇈",
        Action::NextPage => "⇊",
        Action::ToggleSidebar => "⌷",
        Action::ToggleFlyover => "⤓",
        Action::FlyoverPopout => "⇱",
        Action::ToggleToolPanel => "⌗",
        Action::IncreaseFontSize => "␣",
        Action::DecreaseFontSize => "␡",
        Action::OpenSettings => "⚙",
        Action::Copy => "⧉",
        Action::Paste => "⎘",
        Action::Quit => "⏻",
        Action::CommandPalette => "⌘",
        Action::ScreenshotToClipboard => "◫",
        Action::ScreenshotToFile => "⎙",
        Action::NewSection => "▤",
        Action::GoToSessions => "▣",
        Action::GoToPullRequests => "⇅",
        Action::GoToCleanup => "♻",
    }
}

/// Whether confirming `action` enters a multi-step flow (only "New group").
pub fn is_multi_step(action: Action) -> bool {
    action == Action::NewGroup
}

/// Human-friendly group name for a cwd: `~` for the home dir, else the last
/// path component.
pub fn group_name(path: &Path) -> String {
    if dirs::home_dir().is_some_and(|home| home == path) {
        return "~".into();
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picker::{ForkScope, PickerRow, PickerStore};
    use std::path::PathBuf;

    fn action_rows(rows: &[RootRow]) -> Vec<Action> {
        rows.iter().filter_map(|r| match r {
            RootRow::Action(a) => Some(*a),
            _ => None,
        }).collect()
    }

    #[test]
    fn root_rows_are_grouped_with_headers_and_exclude_command_palette() {
        let palette = CommandPalette::root();
        let mut seen_headers = Vec::new();
        let mut current = "";
        for row in &palette.root_rows {
            match row {
                RootRow::Header(h) => {
                    current = h;
                    seen_headers.push(*h);
                }
                RootRow::Action(a) => {
                    assert_eq!(action_group(*a), current, "action under wrong header");
                    assert!(*a != Action::CommandPalette, "CommandPalette listed");
                }
            }
        }
        assert_eq!(seen_headers, vec!["Session", "Tiles", "Navigate", "Edit", "View", "App"]);
        assert_eq!(action_rows(&palette.root_rows).len(), Action::ALL.len() - 1);
    }

    #[test]
    fn fuzzy_filter_keeps_headers_only_for_groups_with_matches() {
        let mut palette = CommandPalette::root();
        palette.set_query("split");
        let mut groups = Vec::new();
        for row in &palette.root_rows {
            match row {
                RootRow::Header(h) => groups.push(*h),
                RootRow::Action(a) => {
                    assert!(a.label().to_lowercase().contains("split"));
                }
            }
        }
        assert_eq!(groups, vec!["Tiles"], "only groups with matches keep headers");
    }

    #[test]
    fn move_selection_skips_headers_and_clamps() {
        let mut palette = CommandPalette::root();
        palette.root_selected = 0; // Header("Session")
        palette.move_selection(1);
        assert!(matches!(palette.root_rows[palette.root_selected], RootRow::Action(_)));
        palette.move_selection(isize::MAX);
        let last = palette.root_selected;
        assert_eq!(last, palette.root_rows.len() - 1);
        assert!(matches!(palette.root_rows[last], RootRow::Action(_)));
        palette.move_selection(-isize::MAX);
        let first = palette.root_selected;
        assert!(matches!(palette.root_rows[first], RootRow::Action(_)));
        assert!(first <= 2, "first action row is right after the Session header");
    }

    #[test]
    fn enter_at_root_on_a_one_shot_action_runs_it() {
        let mut palette = CommandPalette::root();
        let copy_row = palette
            .root_rows
            .iter()
            .position(|r| matches!(r, RootRow::Action(Action::Copy)))
            .expect("Copy is listed");
        palette.select(copy_row);
        assert_eq!(palette.enter(), Outcome::Run(Action::Copy));
        assert_eq!(palette.stage, Stage::Root);
    }

    #[test]
    fn enter_at_root_on_new_group_enters_the_flow() {
        let mut palette = CommandPalette::root();
        let new_group_row = palette
            .root_rows
            .iter()
            .position(|r| matches!(r, RootRow::Action(Action::NewGroup)))
            .expect("NewGroup listed at the root");
        palette.select(new_group_row);
        assert_eq!(palette.enter(), Outcome::Nothing);
        assert_eq!(palette.stage, Stage::Repo);
        assert!(palette.needs_repo(), "the app provides the directory scan");
        assert_eq!(palette.tokens(), vec![Token::Command("New session")]);
        assert!(palette.in_flow());
        assert_eq!(palette.footer(), "New session · step 1 of 3");
        assert_eq!(palette.placeholder(), "Search repos…");
    }

    /// A repo picker with two entries, no filesystem access.
    fn repo_picker() -> Picker {
        Picker::from_entries(
            vec![
                PickerEntry { path: PathBuf::from("/home/u/pwrde"), label: "~/src/pwrde".into(), is_git: true },
                PickerEntry { path: PathBuf::from("/tmp/notes"), label: "notes".into(), is_git: false },
            ],
            PickerStore::default(),
        )
    }

    fn fork_picker() -> ForkPicker {
        ForkPicker::new(
            PathBuf::from("/home/u/pwrde"),
            vec![
                ForkEntry {
                    label: "new worktree".into(),
                    from: Some("origin/main".into()),
                    path: None,
                    scope: ForkScope::Default,
                },
                ForkEntry {
                    label: "repo root".into(),
                    from: None,
                    path: Some(PathBuf::from("/home/u/pwrde")),
                    scope: ForkScope::RepoRoot,
                },
            ],
        )
    }

    fn profile_picker() -> ProfilePicker {
        ProfilePicker::new(Vec::new())
    }

    #[test]
    fn repo_to_base_to_layout_to_done_walk() {
        let mut palette = CommandPalette::new_session(false);
        assert_eq!(palette.stage, Stage::Repo);
        assert_eq!(palette.placeholder(), "Search repos…");
        assert_eq!(palette.footer(), "New session · step 1 of 3");
        assert_eq!(
            palette.steps(),
            [
                ("Repository", StepState::Active),
                ("Base", StepState::Pending),
                ("Layout", StepState::Pending),
            ]
        );
        assert_eq!(palette.tokens(), vec![Token::Command("New session")]);

        // Pick the repo (the picker seats its selection on the first entry,
        // past the section header).
        palette.repo = Some(repo_picker());
        assert!(matches!(palette.repo.as_ref().unwrap().rows[palette.selected()], PickerRow::Entry(_)));
        let notes = palette
            .repo
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .position(|r| matches!(r, PickerRow::Entry(e) if e.label == "notes"))
            .expect("the non-git notes entry is listed");
        palette.select(notes);
        match palette.enter() {
            Outcome::RepoChosen(entry) => {
                assert_eq!(entry.path, PathBuf::from("/tmp/notes"));
                assert_eq!(palette.name, "notes");
                assert_eq!(palette.repo_choice.as_ref().map(|e| e.label.as_str()), Some("notes"));
            }
            other => panic!("expected RepoChosen, got {other:?}"),
        }
        // Non-git repo: straight to Layout via provide_layout.
        assert_eq!(palette.tokens().len(), 2);
        assert!(matches!(&palette.tokens()[1], Token::Arg { kind, .. } if *kind == "repo"));

        palette.provide_layout(profile_picker());
        assert_eq!(palette.stage, Stage::Layout);
        assert_eq!(palette.footer(), "New session · step 3 of 3");
        assert_eq!(
            palette.steps(),
            [
                ("Repository", StepState::Done),
                ("Base", StepState::Pending),
                ("Layout", StepState::Active),
            ]
        );
        palette.select(0); // the Default row
        assert_eq!(palette.enter(), Outcome::Nothing);
        assert_eq!(palette.stage, Stage::Done);
        assert_eq!(palette.footer(), "ready");
        assert_eq!(palette.query(), "");
        assert_eq!(palette.placeholder(), "");
        assert_eq!(
            palette.steps(),
            [
                ("Repository", StepState::Done),
                ("Base", StepState::Done),
                ("Layout", StepState::Done),
            ]
        );

        let launched = match palette.enter() {
            Outcome::Launch(session) => session,
            other => panic!("expected Launch, got {other:?}"),
        };
        assert_eq!(launched.name, "notes");
        assert_eq!(launched.repo.path, PathBuf::from("/tmp/notes"));
        assert!(launched.base.is_none());
        assert!(launched.layout.is_some());
        assert!(!launched.for_flyover);

        // Git-repo variant: provide_base walks through Base too.
        let mut palette = CommandPalette::new_session(false);
        palette.repo = Some(repo_picker());
        match palette.enter() {
            Outcome::RepoChosen(_) => {}
            other => panic!("expected RepoChosen, got {other:?}"),
        }
        palette.provide_base(fork_picker());
        assert_eq!(palette.stage, Stage::Base);
        assert_eq!(palette.footer(), "New session · step 2 of 3");
        assert_eq!(
            palette.steps(),
            [
                ("Repository", StepState::Done),
                ("Base", StepState::Active),
                ("Layout", StepState::Pending),
            ]
        );
        palette.select(1); // the RepoRoot row
        match palette.enter() {
            Outcome::BaseChosen(entry) => assert_eq!(entry.scope, ForkScope::RepoRoot),
            other => panic!("expected BaseChosen, got {other:?}"),
        }
        palette.provide_layout(profile_picker());
        palette.select(0);
        palette.enter();
        assert_eq!(palette.stage, Stage::Done);
        assert_eq!(palette.base_choice.as_ref().map(|e| e.label.as_str()), Some("repo root"));
    }

    #[test]
    fn pop_returns_to_the_previous_stage_and_clears_the_choice() {
        // Repo → Base → Layout → Done, then pop all the way back to Root.
        let mut palette = CommandPalette::root();
        palette.enter_new_session();
        palette.repo = Some(repo_picker());
        palette.enter();
        palette.provide_base(fork_picker());
        palette.select(0);
        palette.enter();
        palette.provide_layout(profile_picker());
        palette.select(0);
        palette.enter();
        assert_eq!(palette.stage, Stage::Done);

        assert_eq!(palette.pop(), Outcome::Nothing);
        assert_eq!(palette.stage, Stage::Layout);
        assert!(palette.layout.is_some(), "the layout picker is kept");
        assert!(palette.layout_choice.is_none(), "the layout choice is cleared");

        assert_eq!(palette.pop(), Outcome::Nothing);
        assert_eq!(palette.stage, Stage::Base);
        assert!(palette.base_choice.is_none());

        assert_eq!(palette.pop(), Outcome::Nothing);
        assert_eq!(palette.stage, Stage::Repo);
        assert!(palette.base_choice.is_none());
        assert!(palette.repo_choice.is_none(), "popping the repo token clears the pick");

        assert_eq!(palette.pop(), Outcome::Nothing);
        assert_eq!(palette.stage, Stage::Root);
        assert!(palette.repo.is_none());
        assert!(palette.repo_choice.is_none());
        assert_eq!(
            palette.tokens(),
            Vec::<Token>::new(),
            "back at the root, no tokens remain"
        );

        // A non-empty query never pops.
        palette.enter_new_session();
        palette.repo = Some(repo_picker());
        palette.set_query("pw");
        assert_eq!(palette.pop(), Outcome::Nothing);
        assert_eq!(palette.stage, Stage::Repo);
        assert_eq!(palette.query(), "pw");
    }

    #[test]
    fn pop_at_repo_closes_unless_opened_at_root() {
        // Opened directly at the Repo step (⇧⌘T): popping closes the palette.
        let mut palette = CommandPalette::new_session(false);
        assert!(!palette.opened_at_root);
        assert_eq!(palette.pop(), Outcome::Close);

        // Opened from the root: popping returns to the root list.
        let mut palette = CommandPalette::root();
        palette.enter_new_session();
        assert!(palette.opened_at_root);
        assert_eq!(palette.pop(), Outcome::Nothing);
        assert_eq!(palette.stage, Stage::Root);
        assert!(!palette.root_rows.is_empty(), "root rows rebuilt");
    }

    /// The flyover flow: same steps, but it launches from the Base pick
    /// (main.rs spawns the tab), so the rail never reaches Layout.
    #[test]
    fn flyover_path_stops_at_the_base_pick() {
        let mut palette = CommandPalette::new_session(true);
        assert!(palette.for_flyover);
        palette.repo = Some(repo_picker());
        palette.select(1); // the git repo
        assert!(matches!(palette.enter(), Outcome::RepoChosen(_)));
        palette.provide_base(fork_picker());
        palette.select(0);
        assert!(matches!(palette.enter(), Outcome::BaseChosen(_)));
        assert_eq!(palette.tokens().len(), 3, "command + repo + base");
    }

    /// A directory handed in from outside starts at the Layout step with the
    /// repo token already committed.
    #[test]
    fn for_directory_starts_at_layout_with_the_repo_committed() {
        let entry = PickerEntry { path: PathBuf::from("/tmp/notes"), label: "notes".into(), is_git: false };
        let palette = CommandPalette::for_directory(entry, profile_picker());
        assert_eq!(palette.stage, Stage::Layout);
        assert_eq!(palette.name, "notes");
        assert!(matches!(palette.tokens().as_slice(), [Token::Command(_), Token::Arg { kind: "repo", .. }]));
    }
}

