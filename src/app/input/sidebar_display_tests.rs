//! MAT-333: the view dropdown's display options persist through the config
//! file, and Search text opens the agent finder without losing a character.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

use super::{app_for_mouse_test, mouse};
use crate::app::state::{InputOwner, SidebarDisplayOption, SidebarGroupMode};

struct ConfigFixture {
    _env: crate::config::TestConfigEnvGuard,
    directory: std::path::PathBuf,
    path: std::path::PathBuf,
}

impl ConfigFixture {
    fn new(seed: &str) -> Self {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-sidebar-display-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("temp config directory");
        let path = directory.join("config.toml");
        std::fs::write(&path, seed).expect("seed config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        Self {
            _env: env,
            directory,
            path,
        }
    }

    fn read(&self) -> (String, crate::config::Config) {
        let content = std::fs::read_to_string(&self.path).expect("saved config");
        let config = toml::from_str(&content).expect("valid config");
        (content, config)
    }
}

impl Drop for ConfigFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).ok();
    }
}

const SEED: &str = "# per-machine config\n[ui.sidebar]\nshow_ask_subtitles = true\n\n[ui.sound]\nenabled = false\n";

#[test]
fn sidebar_display_config_edit_round_trips_all_four_combinations() {
    let fixture = ConfigFixture::new(SEED);
    let mut app = app_for_mouse_test();
    assert!(
        !app.state.sidebar_show_provider_icons,
        "fresh default hides icons"
    );
    assert!(
        !app.state.sidebar_show_machine_names,
        "fresh default hides hosts"
    );
    let (untouched, _) = fixture.read();
    assert_eq!(untouched, SEED, "loading never backfills the new keys");

    let filter = app.state.sidebar_work_filter.clone();
    let generation = app.state.agent_finder_generation;
    for (icons, machines) in [(true, false), (true, true), (false, true), (false, false)] {
        for (option, wanted) in [
            (SidebarDisplayOption::ProviderIcons, icons),
            (SidebarDisplayOption::MachineNames, machines),
        ] {
            if app.state.sidebar_display_option_enabled(option) != wanted {
                assert!(app.toggle_sidebar_display_option(option));
            }
        }
        assert_eq!(app.state.sidebar_show_provider_icons, icons);
        assert_eq!(app.state.sidebar_show_machine_names, machines);

        let (content, saved) = fixture.read();
        assert_eq!(saved.ui.sidebar.show_provider_icons, icons, "{content}");
        assert_eq!(saved.ui.sidebar.show_machine_names, machines, "{content}");
        assert!(saved.ui.sidebar.show_ask_subtitles, "{content}");
        assert!(!saved.ui.sound.enabled, "{content}");
        assert!(content.starts_with("# per-machine config\n"), "{content}");
        assert!(app.state.sidebar_show_ask_subtitles);

        // A fresh start from the saved file sees the same pair.
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let restarted =
            crate::app::App::new(&saved, true, None, api_rx, crate::api::EventHub::default());
        assert_eq!(restarted.state.sidebar_show_provider_icons, icons);
        assert_eq!(restarted.state.sidebar_show_machine_names, machines);

        assert_eq!(app.state.sidebar_work_filter, filter, "search untouched");
        assert_eq!(app.state.agent_finder_generation, generation);
    }
}

#[test]
fn sidebar_display_failed_save_keeps_the_option_and_reports_it() {
    let mut env = crate::config::TestConfigEnvGuard::acquire();
    let directory = std::env::temp_dir().join(format!(
        "herdr-sidebar-display-fail-{}",
        crate::config::test_unique_suffix()
    ));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let blocker = directory.join("not-a-directory");
    std::fs::write(&blocker, "").expect("blocking file");
    env.set(
        crate::config::CONFIG_PATH_ENV_VAR,
        blocker.join("config.toml"),
    );
    let mut app = app_for_mouse_test();
    assert!(!app.toggle_sidebar_display_option(SidebarDisplayOption::ProviderIcons));
    assert!(!app.state.sidebar_show_provider_icons);
    assert!(
        app.state.config_diagnostic.is_some(),
        "the existing notice fires"
    );
    std::fs::remove_dir_all(&directory).ok();
}

#[tokio::test]
async fn sidebar_display_dropdown_keys_and_clicks_toggle_independently() {
    let fixture = ConfigFixture::new(SEED);
    let mut app = app_for_mouse_test();
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 100, 30));
    app.state.open_sidebar_group_menu();
    assert_eq!(app.state.sidebar_group_menu_selected, 0);

    for _ in 0..SidebarGroupMode::VIEWS.len() {
        app.state
            .handle_sidebar_group_menu_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    }
    let toggled = app
        .state
        .handle_sidebar_group_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert_eq!(toggled, Some(SidebarDisplayOption::ProviderIcons));
    assert!(
        app.state.sidebar_group_menu_open,
        "the checklist stays open"
    );
    assert_eq!(app.state.sidebar_group_mode, SidebarGroupMode::Repo);
    app.toggle_sidebar_display_option(SidebarDisplayOption::ProviderIcons);
    assert!(app.state.sidebar_show_provider_icons);
    assert!(!app.state.sidebar_show_machine_names);

    let layout = crate::ui::sidebar_group_menu_layout(&app.state, app.state.screen_rect())
        .expect("dropdown layout");
    let machine_row = layout.list_rect.y
        + u16::try_from(SidebarGroupMode::VIEWS.len() + 1 - layout.first_visible)
            .expect("menu row");
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        layout.list_rect.x + 1,
        machine_row,
    ));
    assert!(app.state.sidebar_show_provider_icons);
    assert!(
        app.state.sidebar_show_machine_names,
        "click toggles only its row"
    );
    let (_, saved) = fixture.read();
    assert!(saved.ui.sidebar.show_provider_icons && saved.ui.sidebar.show_machine_names);

    // Choosing a view still switches views and does not touch the options.
    app.state.sidebar_group_menu_selected = 1;
    let toggled = app
        .state
        .handle_sidebar_group_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert_eq!(toggled, None);
    assert_eq!(app.state.sidebar_group_mode, SidebarGroupMode::VIEWS[1]);
    assert!(app.state.sidebar_show_provider_icons && app.state.sidebar_show_machine_names);
}

#[test]
fn agent_finder_first_typed_character_opens_the_finder_and_is_kept() {
    let mut app = app_for_mouse_test();
    assert_eq!(app.state.sidebar_group_mode, SidebarGroupMode::Repo);
    app.state.sidebar_work_filter.query = "prior".into();
    app.state
        .collapsed_sidebar_groups
        .insert("repo:repo:owner/alpha".into());
    let collapsed = app.state.collapsed_sidebar_groups.clone();
    app.state.sidebar_search_active = true;
    let generation = app.state.agent_finder_generation;

    app.state
        .handle_sidebar_search_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
    assert_eq!(app.state.agent_finder_saved_query.as_deref(), Some("prior"));
    assert_eq!(
        app.state.sidebar_work_filter.query, "a",
        "first char survives open"
    );
    assert!(
        app.state.agent_finder_deadline.is_some(),
        "debounced search scheduled"
    );
    assert!(app.state.agent_finder_generation != generation);
    assert!(
        app.state.agent_finder_results.is_empty(),
        "no synchronous search"
    );

    app.state
        .handle_sidebar_search_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    assert_eq!(app.state.sidebar_work_filter.query, "aB");
    assert_eq!(
        app.state.agent_finder_saved_query.as_deref(),
        Some("prior"),
        "one query owner: the open finder"
    );
    assert_eq!(app.state.input_owner(), InputOwner::Sidebar);

    app.state
        .handle_sidebar_search_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
    assert!(app.state.agent_finder_saved_query.is_none());
    assert_eq!(app.state.sidebar_work_filter.query, "prior");
    assert_eq!(
        app.state.collapsed_sidebar_groups, collapsed,
        "escape restores the tree"
    );
}

#[tokio::test]
async fn agent_finder_first_paste_opens_the_finder_and_is_kept() {
    let mut app = app_for_mouse_test();
    app.state.sidebar_search_active = true;
    app.state.sidebar_focused = true;
    assert!(app.paste_into_input_owner(InputOwner::Sidebar, "needle\n"));
    assert!(app.state.agent_finder_saved_query.is_some());
    assert_eq!(app.state.sidebar_work_filter.query, "needle");
    app.handle_paste_for_input_owner(InputOwner::Sidebar, " two".into())
        .await;
    assert_eq!(app.state.sidebar_work_filter.query, "needle two");
    assert!(app.state.workspaces.is_empty(), "nothing reached a PTY");
}

#[test]
fn agent_finder_search_text_needs_focused_search_and_the_project_list() {
    let mut app = app_for_mouse_test();
    assert!(
        !app.state.insert_agent_finder_text("a"),
        "inactive search ignores text"
    );
    assert!(app.state.agent_finder_saved_query.is_none());

    app.state.sidebar_search_active = true;
    assert!(
        !app.state.insert_agent_finder_text("\n"),
        "control-only text opens nothing"
    );
    assert!(app.state.agent_finder_saved_query.is_none());

    app.state.sidebar_group_mode = SidebarGroupMode::Spaces;
    app.state
        .handle_sidebar_search_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
    assert!(
        app.state.agent_finder_saved_query.is_none(),
        "other views keep their plain filter"
    );
    assert_eq!(app.state.sidebar_work_filter.query, "a");
}
