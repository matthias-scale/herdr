//! Named folders inside the shelves of the sections sidebar (MAT-240).
//!
//! Folder membership uses stable public tab identities and persists alongside
//! folder presentation in the client presentation file.
//!
//! A folder belongs to one shelf. A tab renders inside a folder only while it
//! sits in that shelf. A shelf transition removes its membership.

use serde::{Deserialize, Serialize};

use super::state::AppState;

/// The four lifecycle shelves of the sections layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SidebarShelf {
    Pinned,
    Active,
    Snoozed,
    Settled,
}

impl SidebarShelf {
    pub(crate) const ALL: [Self; 4] = [Self::Pinned, Self::Active, Self::Snoozed, Self::Settled];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Pinned => crate::ui::sidebar::PINNED_SECTION_TITLE,
            Self::Active => crate::ui::sidebar::ACTIVE_SECTION_TITLE,
            Self::Snoozed => crate::ui::sidebar::SNOOZED_SECTION_TITLE,
            Self::Settled => crate::ui::sidebar::SETTLED_SECTION_TITLE,
        }
    }

    pub(crate) fn from_title(title: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shelf| shelf.title() == title)
    }
}

/// One user-named folder. Names are unique across every shelf, compared
/// without regard to case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SidebarFolder {
    pub(crate) shelf: SidebarShelf,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) collapsed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) members: Vec<SidebarFolderTab>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarFolderError {
    EmptyName,
    DuplicateName,
    Missing,
}

impl SidebarFolderError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::EmptyName => "A folder needs a name",
            Self::DuplicateName => "A folder with that name already exists",
            Self::Missing => "That folder no longer exists",
        }
    }
}

/// What the folder-name prompt does once the operator presses Enter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarFolderPrompt {
    /// Create a folder in `shelf`, then move the tab into it when one is named.
    Create {
        shelf: SidebarShelf,
        tab: Option<SidebarFolderTab>,
    },
    Rename {
        name: String,
    },
}

/// A tab by stable identity, so a prompt that stays open across tab churn
/// never files the wrong tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SidebarFolderTab {
    pub(crate) workspace_id: String,
    pub(crate) tab_number: usize,
}

pub(crate) fn normalize_folder_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

pub(crate) fn folder_selection_key(name: &str) -> String {
    format!("folder:{name}")
}

impl AppState {
    pub(crate) fn sidebar_folder(&self, name: &str) -> Option<&SidebarFolder> {
        self.sidebar_folders
            .iter()
            .find(|folder| folder.name == name)
    }

    fn sidebar_folder_name_taken(&self, name: &str, except: Option<&str>) -> bool {
        let folded = name.to_lowercase();
        self.sidebar_folders.iter().any(|folder| {
            folder.name.to_lowercase() == folded && except != Some(folder.name.as_str())
        })
    }

    /// Folder names in `shelf`, in the order they render.
    pub(crate) fn sidebar_folder_names(&self, shelf: SidebarShelf) -> Vec<String> {
        self.sidebar_folders
            .iter()
            .filter(|folder| folder.shelf == shelf)
            .map(|folder| folder.name.clone())
            .collect()
    }

    pub(crate) fn create_sidebar_folder(
        &mut self,
        shelf: SidebarShelf,
        name: &str,
    ) -> Result<String, SidebarFolderError> {
        let name = normalize_folder_name(name).ok_or(SidebarFolderError::EmptyName)?;
        if self.sidebar_folder_name_taken(&name, None) {
            return Err(SidebarFolderError::DuplicateName);
        }
        self.sidebar_folders.push(SidebarFolder {
            shelf,
            name: name.clone(),
            collapsed: false,
            members: Vec::new(),
        });
        self.sidebar_folders_persistence_request = true;
        Ok(name)
    }

    /// Rename the registry entry; member identities do not depend on its name.
    pub(crate) fn rename_sidebar_folder(
        &mut self,
        old: &str,
        new: &str,
    ) -> Result<String, SidebarFolderError> {
        let new = normalize_folder_name(new).ok_or(SidebarFolderError::EmptyName)?;
        let Some(index) = self
            .sidebar_folders
            .iter()
            .position(|folder| folder.name == old)
        else {
            return Err(SidebarFolderError::Missing);
        };
        if new == old {
            return Ok(new);
        }
        if self.sidebar_folder_name_taken(&new, Some(old)) {
            return Err(SidebarFolderError::DuplicateName);
        }
        self.sidebar_folders[index].name = new.clone();
        self.sidebar_folders_persistence_request = true;
        Ok(new)
    }

    /// Delete a folder. Its tabs stay where they are and render loose in the
    /// shelf again.
    pub(crate) fn delete_sidebar_folder(&mut self, name: &str) -> bool {
        let before = self.sidebar_folders.len();
        self.sidebar_folders.retain(|folder| folder.name != name);
        if self.sidebar_folders.len() == before {
            return false;
        }
        self.sidebar_folders_persistence_request = true;
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
        true
    }

    /// File a tab into `folder`, or take it out of any folder with `None`.
    pub(crate) fn set_tab_sidebar_folder(
        &mut self,
        ws_idx: usize,
        tab_idx: usize,
        folder: Option<&str>,
    ) -> bool {
        if let Some(name) = folder {
            let Some(folder_state) = self.sidebar_folder(name) else {
                return false;
            };
            if self.sidebar_sections_layout
                && crate::ui::sidebar::sections_tab_shelf(self, ws_idx, tab_idx)
                    != Some(folder_state.shelf)
            {
                return false;
            }
        }
        let Some(tab) = self.sidebar_folder_tab(ws_idx, tab_idx) else {
            return false;
        };
        let current = self
            .sidebar_folders
            .iter()
            .find(|entry| entry.members.contains(&tab));
        if current.map(|entry| entry.name.as_str()) == folder {
            return true;
        }
        let mut changed = false;
        for entry in &mut self.sidebar_folders {
            let before = entry.members.len();
            entry.members.retain(|member| member != &tab);
            changed |= entry.members.len() != before;
        }
        if let Some(name) = folder {
            if let Some(entry) = self
                .sidebar_folders
                .iter_mut()
                .find(|entry| entry.name == name)
            {
                entry.members.push(tab);
                changed = true;
            }
        }
        self.sidebar_folders_persistence_request |= changed;
        true
    }

    /// Drop a tab's membership after a shelf transition or before closing it.
    pub(crate) fn remove_tab_sidebar_folder(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(tab) = self.sidebar_folder_tab(ws_idx, tab_idx) else {
            return;
        };
        for entry in &mut self.sidebar_folders {
            let before = entry.members.len();
            entry.members.retain(|member| member != &tab);
            self.sidebar_folders_persistence_request |= entry.members.len() != before;
        }
    }

    pub(crate) fn remove_tab_sidebar_folder_if_shelf_changed(
        &mut self,
        ws_idx: usize,
        tab_idx: usize,
    ) {
        let Some(tab) = self.sidebar_folder_tab(ws_idx, tab_idx) else {
            return;
        };
        let Some(folder) = self
            .sidebar_folders
            .iter()
            .find(|entry| entry.members.contains(&tab))
        else {
            return;
        };
        if crate::ui::sidebar::sections_tab_shelf(self, ws_idx, tab_idx) != Some(folder.shelf) {
            self.remove_tab_sidebar_folder(ws_idx, tab_idx);
        }
    }

    pub(crate) fn remove_pane_sidebar_folder_if_shelf_changed(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) {
        let tab_idx = self.workspaces.get(ws_idx).and_then(|workspace| {
            workspace
                .tabs
                .iter()
                .position(|tab| tab.panes.contains_key(&pane_id))
        });
        if let Some(tab_idx) = tab_idx {
            self.remove_tab_sidebar_folder_if_shelf_changed(ws_idx, tab_idx);
        }
    }

    pub(crate) fn prune_closed_sidebar_folder_tabs(&mut self) {
        if self
            .sidebar_folders
            .iter()
            .all(|folder| folder.members.is_empty())
        {
            return;
        }
        let live: std::collections::HashSet<(String, usize)> = self
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .map(move |tab| (workspace.id.clone(), tab.number))
            })
            .collect();
        for entry in &mut self.sidebar_folders {
            let before = entry.members.len();
            entry
                .members
                .retain(|member| live.contains(&(member.workspace_id.clone(), member.tab_number)));
            self.sidebar_folders_persistence_request |= entry.members.len() != before;
        }
    }

    pub(crate) fn toggle_sidebar_folder_collapsed(&mut self, name: &str) -> bool {
        let Some(folder) = self
            .sidebar_folders
            .iter_mut()
            .find(|folder| folder.name == name)
        else {
            return false;
        };
        folder.collapsed = !folder.collapsed;
        self.sidebar_folders_persistence_request = true;
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
        true
    }

    /// The folder a tab renders in while it sits in `shelf`.
    pub(crate) fn tab_sidebar_folder(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        shelf: SidebarShelf,
    ) -> Option<&str> {
        let workspace = self.workspaces.get(ws_idx)?;
        let tab_number = workspace.tabs.get(tab_idx)?.number;
        self.sidebar_folders
            .iter()
            .find(|folder| {
                folder.shelf == shelf
                    && folder.members.iter().any(|member| {
                        member.workspace_id == workspace.id && member.tab_number == tab_number
                    })
            })
            .map(|folder| folder.name.as_str())
    }

    pub(crate) fn sidebar_folder_tab_indices(
        &self,
        tab: &SidebarFolderTab,
    ) -> Option<(usize, usize)> {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == tab.workspace_id)?;
        let tab_idx = self.workspaces[ws_idx]
            .tabs
            .iter()
            .position(|candidate| candidate.number == tab.tab_number)?;
        Some((ws_idx, tab_idx))
    }

    pub(crate) fn sidebar_folder_tab(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<SidebarFolderTab> {
        let workspace = self.workspaces.get(ws_idx)?;
        let tab = workspace.tabs.get(tab_idx)?;
        Some(SidebarFolderTab {
            workspace_id: workspace.id.clone(),
            tab_number: tab.number,
        })
    }

    /// Open the folder-name prompt. It reuses the rename dialog, the way pods
    /// rename, so creating a folder looks like naming anything else.
    pub(crate) fn open_sidebar_folder_prompt(&mut self, prompt: SidebarFolderPrompt) {
        self.pending_workspace_create_cwd = None;
        self.rename_pane_target = None;
        self.name_input = match &prompt {
            SidebarFolderPrompt::Create { .. } => String::new(),
            SidebarFolderPrompt::Rename { name } => name.clone(),
        };
        self.name_input_replace_on_type = false;
        self.rename_target = Some(super::state::RenameTarget::Folder { prompt });
        self.open_client_overlay(super::state::ClientOverlay::RenamePane);
    }

    /// Apply the folder-name prompt. Errors surface as a toast; the prompt
    /// closes either way, like every other rename.
    pub(crate) fn apply_sidebar_folder_prompt(&mut self, prompt: SidebarFolderPrompt, name: &str) {
        let result = match prompt {
            SidebarFolderPrompt::Create { shelf, tab } => {
                self.create_sidebar_folder(shelf, name).map(|name| {
                    if let Some((ws_idx, tab_idx)) = tab
                        .as_ref()
                        .and_then(|tab| self.sidebar_folder_tab_indices(tab))
                    {
                        if crate::ui::sidebar::sections_tab_shelf(self, ws_idx, tab_idx)
                            == Some(shelf)
                        {
                            self.set_tab_sidebar_folder(ws_idx, tab_idx, Some(&name));
                        }
                    }
                })
            }
            SidebarFolderPrompt::Rename { name: old } => {
                self.rename_sidebar_folder(&old, name).map(|_| ())
            }
        };
        if let Err(error) = result {
            self.show_sidebar_folder_error(error);
        }
    }

    pub(crate) fn show_sidebar_folder_error(&mut self, error: SidebarFolderError) {
        self.toast = Some(super::state::ToastNotification {
            kind: super::state::ToastKind::NeedsAttention,
            title: "Folder unchanged".to_string(),
            context: error.message().to_string(),
            position: None,
            target: None,
        });
    }

    /// Open the folder picker for one tab: in the sections layout it lists the
    /// folders of the tab's shelf; in the default layout it is the existing
    /// subgroup picker, which files the tab by the same name.
    pub(crate) fn open_sidebar_folder_picker(
        &mut self,
        ws_idx: usize,
        tab_idx: usize,
        anchor: (u16, u16),
    ) -> bool {
        if self
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.tabs.get(tab_idx))
            .is_none()
        {
            return false;
        }
        let folder_shelf = if self.sidebar_sections_layout {
            let Some(shelf) = crate::ui::sidebar::sections_tab_shelf(self, ws_idx, tab_idx) else {
                return false;
            };
            Some(shelf)
        } else {
            None
        };
        self.sidebar_subgroup_picker = Some(super::state::SidebarSubgroupPickerState {
            ws_idx,
            tab_idx,
            anchor,
            filter: crate::ui::dropdown::DropdownFilterState::default(),
            folder_shelf,
        });
        true
    }

    pub(crate) fn take_sidebar_folders_persistence_request(
        &mut self,
    ) -> Option<Vec<SidebarFolder>> {
        std::mem::take(&mut self.sidebar_folders_persistence_request)
            .then(|| self.sidebar_folders.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_workspace() -> AppState {
        let mut app = AppState::test_new();
        app.sidebar_sections_layout = true;
        app.workspaces = vec![crate::workspace::Workspace::test_new("folders")];
        app.active = Some(0);
        app.ensure_test_terminals();
        app
    }

    #[test]
    fn folder_names_are_trimmed_and_unique_without_case() {
        let mut app = AppState::test_new();
        assert_eq!(
            app.create_sidebar_folder(SidebarShelf::Active, "  Notes  "),
            Ok("Notes".to_string())
        );
        assert_eq!(
            app.create_sidebar_folder(SidebarShelf::Pinned, "notes"),
            Err(SidebarFolderError::DuplicateName)
        );
        assert_eq!(
            app.create_sidebar_folder(SidebarShelf::Active, "  "),
            Err(SidebarFolderError::EmptyName)
        );
    }

    #[test]
    fn renaming_folder_carries_tab_membership() {
        let mut app = app_with_workspace();
        app.create_sidebar_folder(SidebarShelf::Active, "Drafts")
            .expect("create folder");
        app.workspaces[0].tabs[0].set_subgroup(Some("Original".to_string()));
        app.set_tab_sidebar_folder(0, 0, Some("Drafts"));

        assert_eq!(
            app.rename_sidebar_folder("Drafts", " Plans "),
            Ok("Plans".to_string())
        );
        assert_eq!(
            app.tab_sidebar_folder(0, 0, SidebarShelf::Active),
            Some("Plans")
        );
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Original"));
        assert!(app.sidebar_folder("Plans").is_some());
    }

    #[test]
    fn deleting_folder_returns_its_tabs_to_the_shelf() {
        let mut app = app_with_workspace();
        app.create_sidebar_folder(SidebarShelf::Active, "Drafts")
            .expect("create folder");
        app.set_tab_sidebar_folder(0, 0, Some("Drafts"));

        assert!(app.delete_sidebar_folder("Drafts"));
        assert_eq!(app.tab_sidebar_folder(0, 0, SidebarShelf::Active), None);
        assert!(app.sidebar_folder("Drafts").is_none());
    }

    #[test]
    fn create_prompt_files_the_named_tab() {
        let mut app = app_with_workspace();
        let shelf = crate::ui::sidebar::sections_tab_shelf(&app, 0, 0)
            .expect("test tab appears in a shelf");
        let tab = app.sidebar_folder_tab(0, 0).expect("stable tab identity");

        app.apply_sidebar_folder_prompt(
            SidebarFolderPrompt::Create {
                shelf,
                tab: Some(tab),
            },
            "Plans",
        );

        assert_eq!(app.tab_sidebar_folder(0, 0, shelf), Some("Plans"));
        assert_eq!(
            app.sidebar_folder("Plans").map(|folder| folder.shelf),
            Some(shelf)
        );
    }

    #[test]
    fn move_to_folder_new_creates_folder_and_files_tab() {
        let mut app = app_with_workspace();
        let shelf = crate::ui::sidebar::sections_tab_shelf(&app, 0, 0)
            .expect("test tab appears in a shelf");
        assert!(app.open_sidebar_folder_picker(0, 0, (5, 6)));
        assert_eq!(
            crate::ui::sidebar::sidebar_subgroup_picker_choices(&app)[0],
            crate::ui::SidebarSubgroupChoice::NewFolder
        );

        app.accept_sidebar_subgroup_picker(0);
        assert!(matches!(
            app.rename_target.as_ref(),
            Some(super::super::state::RenameTarget::Folder {
                prompt: SidebarFolderPrompt::Create {
                    shelf: prompt_shelf,
                    tab: Some(_),
                }
            }) if *prompt_shelf == shelf
        ));
        let prompt = match app.rename_target.take() {
            Some(super::super::state::RenameTarget::Folder { prompt }) => prompt,
            _ => panic!("folder create prompt expected"),
        };
        app.apply_sidebar_folder_prompt(prompt, "Plans");
        assert_eq!(app.tab_sidebar_folder(0, 0, shelf), Some("Plans"));
        assert_eq!(
            app.sidebar_folder("Plans").map(|folder| folder.shelf),
            Some(shelf)
        );
    }

    #[test]
    fn folder_picker_moves_tab_in_and_back_out() {
        let mut app = app_with_workspace();
        let shelf = crate::ui::sidebar::sections_tab_shelf(&app, 0, 0)
            .expect("test tab appears in a shelf");
        app.create_sidebar_folder(shelf, "Plans")
            .expect("create folder");

        assert!(app.open_sidebar_folder_picker(0, 0, (5, 6)));
        let move_in = crate::ui::sidebar::sidebar_subgroup_picker_choices(&app)
            .iter()
            .position(|choice| {
                *choice == crate::ui::SidebarSubgroupChoice::ExistingFolder("Plans".to_string())
            })
            .expect("folder choice");
        app.accept_sidebar_subgroup_picker(move_in);
        assert_eq!(app.tab_sidebar_folder(0, 0, shelf), Some("Plans"));

        assert!(app.open_sidebar_folder_picker(0, 0, (5, 6)));
        let move_out = crate::ui::sidebar::sidebar_subgroup_picker_choices(&app)
            .iter()
            .position(|choice| *choice == crate::ui::SidebarSubgroupChoice::NoFolder)
            .expect("no-folder choice");
        app.accept_sidebar_subgroup_picker(move_out);
        assert_eq!(app.tab_sidebar_folder(0, 0, shelf), None);
    }

    #[test]
    fn folder_moves_and_delete_preserve_default_layout_subgroup() {
        let mut app = app_with_workspace();
        let shelf = crate::ui::sidebar::sections_tab_shelf(&app, 0, 0).expect("shelf");
        app.workspaces[0].tabs[0].set_subgroup(Some("Original".to_string()));
        app.create_sidebar_folder(shelf, "Plans").expect("folder");
        assert!(app.set_tab_sidebar_folder(0, 0, Some("Plans")));
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Original"));
        assert!(app.set_tab_sidebar_folder(0, 0, None));
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Original"));
        assert!(app.set_tab_sidebar_folder(0, 0, Some("Plans")));
        assert!(app.delete_sidebar_folder("Plans"));
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Original"));
    }

    #[test]
    fn same_named_subgroup_does_not_join_folder() {
        let mut app = app_with_workspace();
        let shelf = crate::ui::sidebar::sections_tab_shelf(&app, 0, 0).expect("shelf");
        app.workspaces[0].tabs[0].set_subgroup(Some("Plans".to_string()));
        app.create_sidebar_folder(shelf, "Plans").expect("folder");
        assert_eq!(app.tab_sidebar_folder(0, 0, shelf), None);
        assert!(app
            .sidebar_folder("Plans")
            .expect("folder")
            .members
            .is_empty());
        assert!(app.delete_sidebar_folder("Plans"));
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Plans"));
    }

    #[test]
    fn leaving_shelf_removes_membership_before_return() {
        let mut app = app_with_workspace();
        app.workspaces[0].tabs[0].pinned = true;
        app.create_sidebar_folder(SidebarShelf::Pinned, "Review")
            .expect("folder");
        assert!(app.set_tab_sidebar_folder(0, 0, Some("Review")));
        app.workspaces[0].tabs[0].pinned = false;
        app.remove_tab_sidebar_folder_if_shelf_changed(0, 0);
        app.workspaces[0].tabs[0].pinned = true;
        assert_eq!(app.tab_sidebar_folder(0, 0, SidebarShelf::Pinned), None);
        assert!(app
            .sidebar_folder("Review")
            .expect("folder")
            .members
            .is_empty());
    }

    #[test]
    fn snooze_and_wake_do_not_restore_old_folder() {
        let mut app = app_with_workspace();
        let pane = app.workspaces[0].tabs[0].root_pane;
        app.create_sidebar_folder(SidebarShelf::Active, "Now")
            .expect("folder");
        assert!(app.set_tab_sidebar_folder(0, 0, Some("Now")));
        assert!(app.snooze_pane_at(0, pane, app.view_observed_unix_s + 900));
        assert!(app
            .sidebar_folder("Now")
            .expect("folder")
            .members
            .is_empty());
        assert!(app.unsnooze_pane_at(
            0,
            pane,
            crate::api::schema::PaneUnsnoozeReason::Explicit,
            std::time::Instant::now(),
        ));
        assert_eq!(app.tab_sidebar_folder(0, 0, SidebarShelf::Active), None);
    }

    #[test]
    fn settle_and_activity_do_not_restore_old_folder() {
        let mut app = app_with_workspace();
        let pane = app.workspaces[0].tabs[0].root_pane;
        app.create_sidebar_folder(SidebarShelf::Active, "Now")
            .expect("folder");
        assert!(app.set_tab_sidebar_folder(0, 0, Some("Now")));
        assert!(app.settle_pane_at(0, pane, 1_725_000_000));
        assert!(app
            .sidebar_folder("Now")
            .expect("folder")
            .members
            .is_empty());
        assert!(app.note_pane_activity_at(pane, std::time::Instant::now()));
        assert_eq!(app.tab_sidebar_folder(0, 0, SidebarShelf::Active), None);
    }

    #[test]
    fn closed_tab_is_pruned_from_persisted_membership() {
        let mut app = app_with_workspace();
        let shelf = crate::ui::sidebar::sections_tab_shelf(&app, 0, 0).expect("shelf");
        app.create_sidebar_folder(shelf, "Plans").expect("folder");
        assert!(app.set_tab_sidebar_folder(0, 0, Some("Plans")));
        app.workspaces.clear();
        app.prune_closed_sidebar_folder_tabs();
        let saved = app
            .take_sidebar_folders_persistence_request()
            .expect("save");
        assert!(saved[0].members.is_empty());
    }

    #[test]
    fn membership_follows_public_tab_number_after_reorder() {
        let mut app = app_with_workspace();
        let second = app.workspaces[0].test_add_tab(Some("second"));
        let identity = app.sidebar_folder_tab(0, second).expect("tab identity");
        app.create_sidebar_folder(SidebarShelf::Active, "Plans")
            .expect("folder");
        app.sidebar_folders[0].members.push(identity.clone());
        assert!(app.workspaces[0].move_tab(second, 0));
        assert_eq!(app.sidebar_folder_tab_indices(&identity), Some((0, 0)));
        assert_eq!(
            app.tab_sidebar_folder(0, 0, SidebarShelf::Active),
            Some("Plans")
        );
        assert_eq!(app.tab_sidebar_folder(0, 1, SidebarShelf::Active), None);
    }

    #[test]
    fn folder_registry_round_trips_as_json() {
        let tab = SidebarFolderTab {
            workspace_id: "w7".to_string(),
            tab_number: 3,
        };
        let folders = vec![
            SidebarFolder {
                shelf: SidebarShelf::Pinned,
                name: "Read later".to_string(),
                collapsed: true,
                members: vec![tab],
            },
            SidebarFolder {
                shelf: SidebarShelf::Settled,
                name: "Archive".to_string(),
                collapsed: false,
                members: Vec::new(),
            },
        ];

        let encoded = serde_json::to_string(&folders).expect("encode folders");
        let decoded: Vec<SidebarFolder> = serde_json::from_str(&encoded).expect("decode folders");
        assert_eq!(decoded, folders);
    }
}
