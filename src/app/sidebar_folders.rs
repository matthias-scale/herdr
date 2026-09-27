//! Named folders inside the shelves of the sections sidebar (MAT-240).
//!
//! A tab joins a folder through its persisted `subgroup` name, the same
//! session field the default layout nests subgroups by, so membership restores
//! with the tab exactly like its pin. The folder list itself (which shelf owns
//! each folder, their order, and whether each is folded) is sidebar
//! presentation and persists in the client presentation file.
//!
//! A folder belongs to one shelf. A tab renders inside a folder only while it
//! sits in that shelf: an unpinned or settled tab leaves the folder's shelf and
//! so renders loose in its new one.

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
/// without regard to case, so a tab's `subgroup` names one folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SidebarFolder {
    pub(crate) shelf: SidebarShelf,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) collapsed: bool,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarFolderTab {
    pub(crate) workspace_id: String,
    pub(crate) tab_id: String,
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
        });
        self.sidebar_folders_persistence_request = true;
        Ok(name)
    }

    /// Rename a folder and every tab filed under it, so membership follows.
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
        let shelf = self.sidebar_folders[index].shelf;
        let tab_shelves = crate::ui::sidebar::sections_tab_shelves(self);
        self.sidebar_folders[index].name = new.clone();
        let mut moved = false;
        for (ws_idx, workspace) in self.workspaces.iter_mut().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter_mut().enumerate() {
                if tab.subgroup() == Some(old)
                    && tab_shelves.get(&(ws_idx, tab_idx)) == Some(&shelf)
                {
                    tab.set_subgroup(Some(new.clone()));
                    moved = true;
                }
            }
        }
        if moved {
            self.mark_session_dirty();
        }
        self.sidebar_folders_persistence_request = true;
        Ok(new)
    }

    /// Delete a folder. Its tabs stay where they are and render loose in the
    /// shelf again.
    pub(crate) fn delete_sidebar_folder(&mut self, name: &str) -> bool {
        let Some(shelf) = self.sidebar_folder(name).map(|folder| folder.shelf) else {
            return false;
        };
        let tab_shelves = crate::ui::sidebar::sections_tab_shelves(self);
        let before = self.sidebar_folders.len();
        self.sidebar_folders.retain(|folder| folder.name != name);
        if self.sidebar_folders.len() == before {
            return false;
        }
        let mut released = false;
        for (ws_idx, workspace) in self.workspaces.iter_mut().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter_mut().enumerate() {
                if tab.subgroup() == Some(name)
                    && tab_shelves.get(&(ws_idx, tab_idx)) == Some(&shelf)
                {
                    tab.set_subgroup(None);
                    released = true;
                }
            }
        }
        if released {
            self.mark_session_dirty();
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
        let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|workspace| workspace.tabs.get_mut(tab_idx))
        else {
            return false;
        };
        if tab.subgroup() == folder {
            return true;
        }
        tab.set_subgroup(folder.map(str::to_string));
        self.mark_session_dirty();
        true
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

    /// The folder a tab renders in while it sits in `shelf`: its subgroup, when
    /// that names a folder owned by the same shelf.
    pub(crate) fn tab_sidebar_folder(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        shelf: SidebarShelf,
    ) -> Option<&str> {
        let name = self
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.tabs.get(tab_idx))
            .and_then(crate::workspace::Tab::subgroup)?;
        self.sidebar_folder(name)
            .filter(|folder| folder.shelf == shelf)
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
        let tab_idx = self.workspaces[ws_idx].tabs.iter().position(|candidate| {
            crate::workspace::public_tab_id_for_number(&tab.workspace_id, candidate.number)
                == tab.tab_id
        })?;
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
            tab_id: crate::workspace::public_tab_id_for_number(&workspace.id, tab.number),
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
        app.workspaces[0].tabs[0].set_subgroup(Some("Drafts".to_string()));

        assert_eq!(
            app.rename_sidebar_folder("Drafts", " Plans "),
            Ok("Plans".to_string())
        );
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Plans"));
        assert!(app.sidebar_folder("Plans").is_some());
    }

    #[test]
    fn deleting_folder_returns_its_tabs_to_the_shelf() {
        let mut app = app_with_workspace();
        app.create_sidebar_folder(SidebarShelf::Active, "Drafts")
            .expect("create folder");
        app.workspaces[0].tabs[0].set_subgroup(Some("Drafts".to_string()));

        assert!(app.delete_sidebar_folder("Drafts"));
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), None);
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

        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Plans"));
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
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Plans"));
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
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), Some("Plans"));

        assert!(app.open_sidebar_folder_picker(0, 0, (5, 6)));
        let move_out = crate::ui::sidebar::sidebar_subgroup_picker_choices(&app)
            .iter()
            .position(|choice| *choice == crate::ui::SidebarSubgroupChoice::NoFolder)
            .expect("no-folder choice");
        app.accept_sidebar_subgroup_picker(move_out);
        assert_eq!(app.workspaces[0].tabs[0].subgroup(), None);
    }

    #[test]
    fn folder_registry_round_trips_as_json() {
        let folders = vec![
            SidebarFolder {
                shelf: SidebarShelf::Pinned,
                name: "Read later".to_string(),
                collapsed: true,
            },
            SidebarFolder {
                shelf: SidebarShelf::Settled,
                name: "Archive".to_string(),
                collapsed: false,
            },
        ];

        let encoded = serde_json::to_string(&folders).expect("encode folders");
        let decoded: Vec<SidebarFolder> = serde_json::from_str(&encoded).expect("decode folders");
        assert_eq!(decoded, folders);
    }
}
