use egui::{Color32, RichText, Ui};

use crate::MessageToRT;
use crate::overlay::resize::handle_resize;
use crate::settings::{
    EasingModes, MediaControlsPosition, NowPlayingSource, ProgressBarPosition, Settings,
};
use crate::theming::{self, Theme};

fn section_label(ui: &mut Ui, background: [u8; 3], text: &str) {
    ui.add_space(8.0);
    ui.label(
        RichText::new(text)
            .size(11.0)
            .color(super::readable_gray(background, 130))
            .strong(),
    );
    ui.add_space(2.0);
}

fn settings_row(
    ui: &mut Ui,
    background: [u8; 3],
    label: &str,
    tooltip: &str,
    widget: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(12.0)
                .color(super::readable_gray(background, 160)),
        )
        .on_hover_text(tooltip);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), widget);
    });
}

/// Custom title bar: drag-to-move, since decorations are off.
fn settings_title_bar(ui: &mut Ui, ctx: &egui::Context, accent: Color32) {
    let title_rect = egui::Rect::from_min_size(
        ui.min_rect().min,
        egui::vec2(ui.available_width(), 22.0),
    );
    if ui
        .interact(title_rect, ui.id().with("settings_drag"), egui::Sense::drag())
        .dragged()
    {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    ui.label(RichText::new("⚙  Settings").size(12.0).color(accent).strong());
    ui.add_space(4.0);
    ui.separator();
}

impl super::LyricsAppUI {
    /// `show_button` controls only the gear toggle's visibility (e.g. hidden until
    /// hover); the settings viewport itself, once open, keeps being serviced every
    /// frame regardless, since egui's immediate viewports need that to stay alive.
    pub(super) fn settings_ui(&mut self, ui: &mut Ui, ctx: &egui::Context, show_button: bool) {
        let accent = super::accent_color(self.settings_cache.current_line_color);

        if show_button
            && ui
                .add(egui::Button::selectable(
                    self.settings_open,
                    RichText::new("⚙").size(14.0).color(accent),
                ))
                .clicked()
        {
            self.settings_open = !self.settings_open;
        }

        if !self.settings_open {
            return;
        }

        let background = self.settings_cache.background_color;

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("settings_window"),
            egui::ViewportBuilder::default()
                .with_title("Lyrics Overlay — Settings")
                .with_inner_size([320.0, 480.0])
                .with_min_inner_size([280.0, 320.0])
                .with_decorations(false) // no window chrome, matches the main overlay
                .with_transparent(true)
                .with_always_on_top() // otherwise the always-on-top main window paints over it
                .with_resizable(true),
            |ctx, _class| {
                // Popup/dropdown background: the theme's background, nudged lighter so
                // dropdowns stay visually distinct from the panel behind them.
                let lighten = |c: u8| c.saturating_add(12);
                let popup_fill = Color32::from_rgb(
                    lighten(background[0]),
                    lighten(background[1]),
                    lighten(background[2]),
                );

                ctx.set_visuals(egui::Visuals {
                    panel_fill: Color32::TRANSPARENT,
                    window_fill: popup_fill,
                    popup_shadow: egui::Shadow::NONE,
                    ..super::theme_visuals(background, accent)
                });

                handle_resize(ctx, 6.0f32);

                // Close when the window's own X is clicked, or the pointer leaves via Alt+F4 etc.
                if ctx.input(|i| i.viewport().close_requested()) {
                    self.settings_open = false;
                }

                let full_width = ctx.available_rect().width();

                // Close button, floating above the drag region below
                egui::Area::new("settings_close".into())
                    .fixed_pos(egui::pos2(full_width - 22., 8.))
                    .show(ctx, |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("X").size(13.0).color(accent))
                                    .frame(true)
                                    .frame_when_inactive(false),
                            )
                            .clicked()
                        {
                            self.settings_open = false;
                        }
                    });

                let panel_frame = egui::Frame::new()
                    .fill(Color32::from_rgb(background[0], background[1], background[2]))
                    .corner_radius(10)
                    .stroke(egui::Stroke::new(1.0f32, Color32::from_white_alpha(20)))
                    .inner_margin(egui::Margin::symmetric(14, 10));

                egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
                    settings_title_bar(ui, ctx, accent);

                    let mut settings = self.settings.blocking_read().clone();
                    let snapshot = format!("{settings:?}");

                    // Reserve room for the Reset defaults row so it can't get pushed
                    // below the window by a tall (e.g. expanded Advanced) scroll area.
                    let reset_row_height = 24.0;
                    let scroll_max_height = (ui.available_height() - reset_row_height).max(0.0);

                    egui::ScrollArea::vertical()
                        .max_height(scroll_max_height)
                        .show(ui, |ui| {
                            display_settings(ui, background, &mut settings);
                            theme_settings(ui, background, &mut settings, &self.custom_themes);
                            behaviour_settings(ui, background, &mut settings);
                            self.authentication_settings(ui, background, &mut settings);
                            advanced_settings(ui, background, &mut settings);
                        });

                    reset_defaults(ui, background, &mut settings);

                    if format!("{settings:?}") != snapshot {
                        if let Err(e) = settings.save() {
                            self.show_error(e);
                        }
                        *self.settings.blocking_write() = settings;
                    }
                });
            },
        );
    }
}

fn display_settings(ui: &mut Ui, background: [u8; 3], settings: &mut Settings) {
    section_label(ui, background, "Display");

    settings_row(ui, background, "Font size", "Size of the font used for lyrics", |ui| {
        ui.add(
            egui::Slider::new(&mut settings.font_size, 10.0..=72.0)
                .step_by(1.0)
                .suffix(" px")
                .text_color(super::readable_gray(background, 200)),
        );
    });
    settings_row(
        ui,
        background,
        "Line spacing",
        "Vertical space between lyric lines",
        |ui| {
            ui.add(
                egui::Slider::new(&mut settings.line_spacing, 0.0..=100.0)
                    .step_by(1.0)
                    .suffix(" px")
                    .text_color(super::readable_gray(background, 200)),
            );
        },
    );
    settings_row(
        ui,
        background,
        "Background opacity",
        "Opacity of the background of the main window",
        |ui| {
            ui.add(
                egui::Slider::new(&mut settings.opacity, 0.0..=1.0)
                    .step_by(0.01)
                    .custom_formatter(|v, _| format!("{:.00}%", v * 100.))
                    .text_color(super::readable_gray(background, 200)),
            );
        },
    );
    settings_row(
        ui,
        background,
        "Dim distant lines",
        "Do we reduce opacity of lines not currently being sung",
        |ui| {
            ui.checkbox(&mut settings.dim_distant_lines, "");
        },
    );
    settings_row(
        ui,
        background,
        "Auto-hide window buttons",
        "Only show the minimize/settings/close buttons while hovering the window",
        |ui| {
            ui.checkbox(&mut settings.window_controls_on_hover, "");
        },
    );
    settings_row(
        ui,
        background,
        "Scroll smoothly",
        "Scroll smoothly, or jump from line to line",
        |ui| {
            ui.checkbox(&mut settings.scroll_smoothly, "");
        },
    );
    if settings.scroll_smoothly {
        settings_row(
            ui,
            background,
            "Transition time",
            "Time spent transitioning from one line to the next",
            |ui| {
                ui.add(
                    egui::Slider::new(&mut settings.line_transition_ms, 0..=1000)
                        .step_by(10.0)
                        .custom_formatter(|v, _| format!("{v}ms"))
                        .text_color(super::readable_gray(background, 200)),
                );
            },
        );
    }
}

fn theme_settings(ui: &mut Ui, background: [u8; 3], settings: &mut Settings, custom_themes: &[Theme]) {
    section_label(ui, background, "Theme");

    let apply = |settings: &mut Settings, theme: &Theme| {
        settings.background_color = theme.background;
        settings.past_line_color = theme.past_line;
        settings.current_line_color = theme.current_line;
        settings.future_line_color = theme.future_line;
    };

    ui.horizontal_wrapped(|ui| {
        for theme in &theming::builtin_themes() {
            if ui.button(&theme.name).clicked() {
                apply(settings, theme);
            }
        }
    });
    if !custom_themes.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for theme in custom_themes {
                if ui
                    .button(&theme.name)
                    .on_hover_text("Custom theme from the themes/ folder")
                    .clicked()
                {
                    apply(settings, theme);
                }
            }
        });
    }
    ui.add_space(4.0);

    settings_row(
        ui,
        background,
        "Background",
        "Background color of the overlay",
        |ui| {
            ui.color_edit_button_srgb(&mut settings.background_color);
        },
    );
    settings_row(ui, background, "Past line", "Color of lines already sung", |ui| {
        ui.color_edit_button_srgb(&mut settings.past_line_color);
    });
    settings_row(
        ui,
        background,
        "Current line",
        "Color of the line currently being sung",
        |ui| {
            ui.color_edit_button_srgb(&mut settings.current_line_color);
        },
    );
    settings_row(ui, background, "Future line", "Color of lines not yet sung", |ui| {
        ui.color_edit_button_srgb(&mut settings.future_line_color);
    });
}

fn behaviour_settings(ui: &mut Ui, background: [u8; 3], settings: &mut Settings) {
    section_label(ui, background, "Behaviour");

    settings_row(ui, background, "SMTC refresh interval", "", |ui| {
        ui.add(
            egui::Slider::new(&mut settings.smtc_poll_interval_ms, 250..=5000)
                .suffix(" ms")
                .text_color(super::readable_gray(background, 200)),
        );
    });
    settings_row(
        ui,
        background,
        "Cache lyrics",
        "Do we cache any requested lyrics, improves future responsiveness and reduces load on the LRC lib",
        |ui| {
            ui.checkbox(&mut settings.caching_enabled, "");
        },
    );

    settings_row(
        ui,
        background,
        "Media controls",
        "Show play/pause and skip buttons in the overlay",
        |ui| {
            egui::ComboBox::from_id_salt("media_controls_position")
                .selected_text(settings.media_controls_position.as_str())
                .show_ui(ui, |ui| {
                    for pos in [
                        MediaControlsPosition::Hidden,
                        MediaControlsPosition::Top,
                        MediaControlsPosition::TopHover,
                        MediaControlsPosition::Bottom,
                        MediaControlsPosition::BottomHover,
                    ] {
                        ui.selectable_value(
                            &mut settings.media_controls_position,
                            pos,
                            pos.as_str(),
                        );
                    }
                });
        },
    );

    progress_bar_settings(ui, background, settings);
    easing_settings(ui, background, settings);
}

fn progress_bar_settings(ui: &mut Ui, background: [u8; 3], settings: &mut Settings) {
    settings_row(
        ui,
        background,
        "Line progress bar",
        "Shows a bar with progress/duration of current line",
        |ui| {
            egui::ComboBox::from_id_salt("progress_bar_position")
                .selected_text(settings.line_progress_bar_position.as_str())
                .show_ui(ui, |ui| {
                    for pos in [
                        ProgressBarPosition::BelowCurrentLine,
                        ProgressBarPosition::Bottom,
                        ProgressBarPosition::Hidden,
                    ] {
                        ui.selectable_value(
                            &mut settings.line_progress_bar_position,
                            pos,
                            pos.as_str(),
                        );
                    }
                });
        },
    );
    settings_row(
        ui,
        background,
        "Song progress bar",
        "Shows a bar with progress/duration of current song",
        |ui| {
            egui::ComboBox::from_id_salt("song_progress_bar_position")
                .selected_text(settings.song_progress_bar_position.as_str())
                .show_ui(ui, |ui| {
                    for pos in [
                        ProgressBarPosition::BelowCurrentLine,
                        ProgressBarPosition::Bottom,
                        ProgressBarPosition::Hidden,
                    ] {
                        ui.selectable_value(
                            &mut settings.song_progress_bar_position,
                            pos,
                            pos.as_str(),
                        );
                    }
                });
        },
    );
}

fn easing_settings(ui: &mut Ui, background: [u8; 3], settings: &mut Settings) {
    if settings.scroll_smoothly {
        settings_row(
            ui,
            background,
            "Position easing",
            "How the scroll eases into each line",
            |ui| {
                egui::ComboBox::from_id_salt("position_ease")
                    .selected_text(settings.ease_position.as_str())
                    .show_ui(ui, |ui| {
                        for mode in [EasingModes::Cubic, EasingModes::Linear] {
                            ui.selectable_value(&mut settings.ease_position, mode, mode.as_str());
                        }
                    });
            },
        );
    }

    settings_row(ui, background, "Color easing", "Ease color?", |ui| {
        egui::ComboBox::from_id_salt("color_ease")
            .selected_text(settings.ease_color.as_str())
            .show_ui(ui, |ui| {
                for mode in [EasingModes::Cubic, EasingModes::Linear] {
                    ui.selectable_value(&mut settings.ease_color, mode, mode.as_str());
                }
            });
    });
}

impl super::LyricsAppUI {
    /// Lyrics and playback controls already work with no setup via Windows' own media
    /// controls - this section is entirely optional, and only unlocks Spotify's own
    /// (richer) lyrics as a bonus when the active session is Spotify.
    fn authentication_settings(&mut self, ui: &mut Ui, background: [u8; 3], settings: &mut Settings) {
        section_label(ui, background, "Spotify (optional)");

        ui.label(
            RichText::new(
                "Lyrics work with no setup. Connect Spotify for its own bonus lyrics source.",
            )
            .size(11.0)
            .color(super::readable_gray(background, 120)),
        );
        ui.add_space(4.0);

        settings_row(
            ui,
            background,
            "Spotify developer Client ID",
            "Client ID as found in spotify developer dashboard",
            |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut settings.client_id)
                        .desired_width(120.0)
                        .text_color(super::readable_gray(background, 200)),
                );
            },
        );

        settings_row(
            ui,
            background,
            "Spotify developer Client Secret",
            "Client Secret as found in spotify developer dashboard",
            |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut settings.client_secret)
                        .desired_width(120.0)
                        .text_color(super::readable_gray(background, 200))
                        .password(true),
                );
            },
        );

        settings_row(
            ui,
            background,
            "sp_dc cookie",
            "Value of the 'sp_dc' cookie from a logged-in open.spotify.com session - \
             required (in addition to Client ID/Secret) for Spotify's own lyrics",
            |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut settings.sp_dc)
                        .desired_width(120.0)
                        .text_color(super::readable_gray(background, 200))
                        .password(true),
                );
            },
        );

        ui.add_space(4.0);
        self.connection_settings(ui, background, settings);
    }

    fn connection_settings(&mut self, ui: &mut Ui, background: [u8; 3], settings: &mut Settings) {
        settings_row(
            ui,
            background,
            if self.is_auth {
                "Connected"
            } else {
                "Not connected"
            },
            "",
            |ui| {
                if self.is_auth {
                    if ui.button("Disconnect").clicked() {
                        let _ = self.tx.try_send(MessageToRT::InvalidateToken);
                    }
                } else {
                    let has_credentials =
                        !settings.client_id.is_empty() && !settings.client_secret.is_empty();
                    ui.add_enabled_ui(has_credentials, |ui| {
                        if ui.button("Connect").clicked() {
                            let _ = self.tx.try_send(MessageToRT::Authenticate);
                        }
                    });
                }
            },
        );

        let is_auth = self.is_auth;
        settings_row(
            ui,
            background,
            "Now-playing source",
            "Which source to check first for what's playing and to control - the other is \
             used as a fallback when the preferred one has nothing (e.g. Spotify Connect \
             playing on another device/speaker, which Windows can't see). The Spotify \
             options need being connected above.",
            |ui| {
                ui.add_enabled_ui(is_auth, |ui| {
                    egui::ComboBox::from_id_salt("now_playing_source")
                        .selected_text(settings.now_playing_source.as_str())
                        .show_ui(ui, |ui| {
                            for source in [
                                NowPlayingSource::SmtcOnly,
                                NowPlayingSource::PreferSmtc,
                                NowPlayingSource::PreferSpotify,
                            ] {
                                ui.selectable_value(
                                    &mut settings.now_playing_source,
                                    source,
                                    source.as_str(),
                                );
                            }
                        });
                });
            },
        );
    }
}

fn advanced_settings(ui: &mut Ui, background: [u8; 3], settings: &mut Settings) {
    ui.add_space(8.0);
    egui::CollapsingHeader::new(
        RichText::new("Advanced")
            .size(11.0)
            .color(super::readable_gray(background, 130))
            .strong(),
    )
    .default_open(false)
    .show(ui, |ui| {
        if settings.caching_enabled {
            settings_row(
                ui,
                background,
                "Cache folder",
                "Where do you want to store cache?",
                |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut settings.cache_folder)
                            .desired_width(120.0)
                            .text_color(super::readable_gray(background, 200)),
                    );
                },
            );
        }
        settings_row(ui, background, "Log level", "Log level, what more can I say", |ui| {
            egui::ComboBox::from_id_salt("log_level")
                .selected_text(settings.log_level.as_str())
                .show_ui(ui, |ui| {
                    for level in ["error", "warn", "info", "debug", "trace"] {
                        ui.selectable_value(&mut settings.log_level, level.to_string(), level);
                    }
                });
        });
    });
}

fn reset_defaults(ui: &mut Ui, background: [u8; 3], settings: &mut Settings) {
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        // Arm on first click, only reset if confirmed with a second click within a few seconds.
        let armed_id = ui.id().with("reset_defaults_armed_at");
        let now = ui.input(|i| i.time);
        let armed = ui
            .data(|d| d.get_temp::<f64>(armed_id))
            .is_some_and(|armed_at| now - armed_at < 4.0);

        // The armed (destructive-confirmation) color stays a fixed alert red regardless
        // of theme - that's a deliberate, universally-recognizable warning color, not
        // themed body text.
        let (text, color) = if armed {
            ("Click again to confirm", Color32::from_rgb(210, 90, 90))
        } else {
            ("Reset defaults", super::readable_gray(background, 110))
        };

        let clicked = ui
            .add(egui::Button::new(RichText::new(text).size(11.0).color(color)).frame(false))
            .clicked();

        if clicked && armed {
            // Preserve credentials when resetting display/behaviour settings
            let client_id = settings.client_id.clone();
            let client_secret = settings.client_secret.clone();
            let sp_dc = settings.sp_dc.clone();
            settings.reset();
            settings.client_id = client_id;
            settings.client_secret = client_secret;
            settings.sp_dc = sp_dc;
            ui.data_mut(|d| d.remove::<f64>(armed_id));
        } else if clicked {
            ui.data_mut(|d| d.insert_temp(armed_id, now));
        }
    });
}
