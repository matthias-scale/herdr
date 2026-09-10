use super::App;

impl App {
    #[cfg(not(windows))]
    pub(super) fn query_host_terminal_appearance(&self) {
        use std::io::Write;

        let _ = std::io::stdout()
            .write_all(crate::terminal_theme::HOST_COLOR_SCHEME_QUERY_SEQUENCE.as_bytes());
        let _ = std::io::stdout().flush();
    }

    pub(super) fn query_host_terminal_theme(&self) {
        use std::io::Write;

        let query = crate::terminal_theme::host_terminal_theme_query_sequence(
            crate::platform::should_query_host_terminal_palette(),
        );
        let _ = std::io::stdout().write_all(query.as_bytes());
        let _ = std::io::stdout().flush();
    }

    pub(super) fn update_host_terminal_theme(
        &mut self,
        kind: crate::terminal_theme::DefaultColorKind,
        color: crate::terminal_theme::RgbColor,
    ) -> bool {
        let mut changed = false;
        if matches!(kind, crate::terminal_theme::DefaultColorKind::Background)
            && !self.state.host_terminal_appearance_explicit
        {
            changed |= self.set_host_terminal_appearance(color.inferred_appearance(), false);
        }
        let next_theme = self.state.host_terminal_theme.with_color(kind, color);
        changed | self.set_host_terminal_theme(next_theme)
    }

    pub(super) fn update_host_terminal_palette_colors(
        &mut self,
        colors: &[(u8, crate::terminal_theme::RgbColor)],
    ) -> bool {
        let mut next_theme = self.state.host_terminal_theme;
        for &(index, color) in colors {
            next_theme = next_theme.with_palette_color(index, color);
        }
        self.set_host_terminal_theme(next_theme)
    }

    pub(super) fn set_host_terminal_appearance(
        &mut self,
        appearance: crate::terminal_theme::HostAppearance,
        explicit: bool,
    ) -> bool {
        if self.state.host_terminal_appearance == Some(appearance)
            && self.state.host_terminal_appearance_explicit == explicit
        {
            return false;
        }
        if self.state.host_terminal_appearance_explicit && !explicit {
            return false;
        }
        self.state.host_terminal_appearance = Some(appearance);
        self.state.host_terminal_appearance_explicit = explicit;
        self.refresh_effective_app_theme()
    }

    pub(crate) fn set_host_terminal_appearance_state(
        &mut self,
        appearance: Option<crate::terminal_theme::HostAppearance>,
        explicit: bool,
    ) -> bool {
        if self.state.host_terminal_appearance == appearance
            && self.state.host_terminal_appearance_explicit == explicit
        {
            return false;
        }
        self.state.host_terminal_appearance = appearance;
        self.state.host_terminal_appearance_explicit = explicit;
        self.refresh_effective_app_theme()
    }

    pub(crate) fn set_host_terminal_theme(
        &mut self,
        theme: crate::terminal_theme::TerminalTheme,
    ) -> bool {
        if theme == self.state.host_terminal_theme {
            return false;
        }
        self.state.host_terminal_theme = theme;
        self.apply_host_terminal_theme_to_panes();
        true
    }

    pub(crate) fn set_host_appearance_override(
        &mut self,
        appearance: crate::config::HostAppearanceOverride,
    ) -> bool {
        if self.state.theme_runtime.host_appearance == appearance {
            return false;
        }
        self.state.theme_runtime.host_appearance = appearance;
        self.refresh_effective_app_theme();
        true
    }

    /// Swap the active theme with its known dark/light sibling for this
    /// session. The config writer intentionally is not used here: save_theme
    /// disables auto_switch, which would make the palette command undo the
    /// user's theme mode.
    pub(crate) fn toggle_theme(&mut self) {
        let current = self.state.theme_name.clone();
        let (dark, light) = super::sibling_theme_names(&current);
        if dark == light {
            tracing::debug!(theme = %current, "theme has no dark/light sibling");
            return;
        }

        let normalized_current = super::normalize_theme_name(&current);
        let normalized_dark = super::normalize_theme_name(&dark);
        let normalized_light = super::normalize_theme_name(&light);
        let next = if normalized_current == normalized_dark {
            light
        } else if normalized_current == normalized_light {
            dark
        } else {
            match self.state.palette.appearance() {
                Some(crate::terminal_theme::HostAppearance::Dark) => light,
                Some(crate::terminal_theme::HostAppearance::Light) => dark,
                None => {
                    tracing::debug!(theme = %current, "theme sibling cannot be inferred");
                    return;
                }
            }
        };
        let next_appearance = if super::normalize_theme_name(&next) == normalized_dark {
            crate::config::HostAppearanceOverride::Dark
        } else {
            crate::config::HostAppearanceOverride::Light
        };

        self.state.theme_runtime.manual_name = next;
        if self.state.theme_runtime.auto_switch {
            // Pin the host side too, otherwise the next appearance poll would
            // immediately select the previous auto-switch branch again.
            if !self.set_host_appearance_override(next_appearance) {
                self.refresh_effective_app_theme();
            }
        } else {
            self.refresh_effective_app_theme();
        }
    }

    pub(super) fn refresh_effective_app_theme(&mut self) -> bool {
        let (palette, theme_name) = super::resolve_effective_theme(
            &self.state.theme_runtime,
            self.state.host_terminal_appearance,
        );
        let mismatch = theme_appearance_mismatch(
            &theme_name,
            palette.appearance(),
            self.state
                .theme_runtime
                .host_appearance
                .appearance()
                .or(self.state.host_terminal_appearance),
        );
        if self.state.theme_appearance_mismatch != mismatch {
            if let Some(message) = &mismatch {
                tracing::warn!(theme = %theme_name, "{message}");
            }
            self.state.theme_appearance_mismatch = mismatch;
        }
        let changed = self.state.theme_name != theme_name || self.state.palette != palette;
        if changed {
            self.state.theme_name = theme_name;
            self.state.palette = palette;
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        self.apply_host_terminal_theme_to_panes();
        self.apply_host_terminal_appearance_to_panes();
        changed
    }

    fn apply_host_terminal_appearance_to_panes(&self) {
        let appearance = Some(self.state.pane_terminal_appearance());
        for runtime in self.terminal_runtimes.values() {
            runtime.apply_host_terminal_appearance(appearance);
        }
    }

    fn apply_host_terminal_theme_to_panes(&self) {
        let theme = self.state.pane_terminal_theme();
        for runtime in self.terminal_runtimes.values() {
            runtime.apply_host_terminal_theme(theme);
        }

        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }
}

/// Name the case where the active palette was built for one appearance and the
/// host terminal reports the other.
///
/// This is the failure that hides a UI: light foregrounds on a dark terminal
/// (or the reverse) stay technically rendered and practically invisible. It is
/// cheap to detect — both sides are already known — and expensive to notice by
/// eye, so say it out loud instead of leaving it to a screenshot.
pub(super) fn theme_appearance_mismatch(
    theme_name: &str,
    palette: Option<crate::terminal_theme::HostAppearance>,
    host: Option<crate::terminal_theme::HostAppearance>,
) -> Option<String> {
    let (palette, host) = (palette?, host?);
    if palette == host {
        return None;
    }
    let describe = |appearance: crate::terminal_theme::HostAppearance| match appearance {
        crate::terminal_theme::HostAppearance::Dark => "dark",
        crate::terminal_theme::HostAppearance::Light => "light",
    };
    Some(format!(
        "theme \"{theme_name}\" is {} but the terminal reports a {} background; \
         set [theme] auto_switch = true or pin the {} sibling",
        describe(palette),
        describe(host),
        describe(host),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(config: &crate::config::Config) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(config, true, None, api_rx, crate::api::EventHub::default())
    }

    #[test]
    fn theme_toggle_swaps_to_the_sibling_and_back() {
        let mut app = test_app(&crate::config::Config::default());

        app.toggle_theme();
        assert_eq!(app.state.theme_name, "catppuccin-latte");
        assert_eq!(app.state.theme_runtime.manual_name, "catppuccin-latte");

        app.toggle_theme();
        assert_eq!(app.state.theme_name, "catppuccin");
        assert_eq!(app.state.theme_runtime.manual_name, "catppuccin");
    }

    #[test]
    fn theme_toggle_is_a_no_op_for_an_unpaired_theme() {
        let mut config = crate::config::Config::default();
        config.theme.name = Some("vesper".into());
        let mut app = test_app(&config);
        let before = (
            app.state.theme_name.clone(),
            app.state.theme_runtime.manual_name.clone(),
            app.state.palette.clone(),
        );

        app.toggle_theme();

        assert_eq!(
            (
                app.state.theme_name,
                app.state.theme_runtime.manual_name,
                app.state.palette,
            ),
            before
        );
    }

    #[test]
    fn theme_toggle_pins_the_target_side_when_auto_switch_is_enabled() {
        let mut config = crate::config::Config::default();
        config.theme.auto_switch = true;
        let mut app = test_app(&config);
        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Dark, true);

        app.toggle_theme();
        assert_eq!(
            app.state.theme_runtime.host_appearance,
            crate::config::HostAppearanceOverride::Light
        );
        assert_eq!(app.state.theme_name, "catppuccin-latte");

        app.toggle_theme();
        assert_eq!(
            app.state.theme_runtime.host_appearance,
            crate::config::HostAppearanceOverride::Dark
        );
        assert_eq!(app.state.theme_name, "catppuccin");
    }
}
