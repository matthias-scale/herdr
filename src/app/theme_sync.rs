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

        let query = crate::terminal_theme::host_terminal_theme_query_sequence();
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
        self.apply_host_terminal_appearance_to_panes();
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
        self.apply_host_terminal_appearance_to_panes();
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

    pub(super) fn refresh_effective_app_theme(&mut self) -> bool {
        let (palette, theme_name) = super::resolve_effective_theme(
            &self.state.theme_runtime,
            self.state.host_terminal_appearance,
        );
        let mismatch = theme_appearance_mismatch(
            &theme_name,
            palette.appearance(),
            self.state.host_terminal_appearance,
        );
        if self.state.theme_appearance_mismatch != mismatch {
            if let Some(message) = &mismatch {
                tracing::warn!(theme = %theme_name, "{message}");
            }
            self.state.theme_appearance_mismatch = mismatch;
        }
        if self.state.theme_name == theme_name && self.state.palette == palette {
            return false;
        }
        self.state.theme_name = theme_name;
        self.state.palette = palette;
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
        true
    }

    fn apply_host_terminal_appearance_to_panes(&self) {
        for runtime in self.terminal_runtimes.values() {
            runtime.apply_host_terminal_appearance(self.state.host_terminal_appearance);
        }
    }

    fn apply_host_terminal_theme_to_panes(&self) {
        for runtime in self.terminal_runtimes.values() {
            runtime.apply_host_terminal_theme(self.state.host_terminal_theme);
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
