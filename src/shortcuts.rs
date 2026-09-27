use eframe::egui::{self, Color32, RichText, Window};

/// Cheat sheet item for keyboard shortcuts modal.
pub struct ShortcutEntry {
    pub key: &'static str,
    pub description: &'static str,
}

pub struct ShortcutCategory {
    pub name: &'static str,
    pub entries: &'static [ShortcutEntry],
}

pub const SHORTCUT_CATEGORIES: &[ShortcutCategory] = &[
    ShortcutCategory {
        name: "Acquisition & Control",
        entries: &[
            ShortcutEntry {
                key: "Space",
                description: "Run / Stop acquisition, or Pause / Resume Auto mode",
            },
            ShortcutEntry {
                key: "A",
                description: "Toggle automatic periodic capture (Auto)",
            },
            ShortcutEntry {
                key: "R",
                description: "Refresh / Read back front-panel settings from hardware",
            },
        ],
    },
    ShortcutCategory {
        name: "Display & Plotting",
        entries: &[
            ShortcutEntry {
                key: "F",
                description: "Fit now (autoscale X and Y axes to current waveform)",
            },
            ShortcutEntry {
                key: "L",
                description: "Toggle Stacked Lanes view (per-channel dedicated bands)",
            },
            ShortcutEntry {
                key: "W / 3",
                description: "Toggle 3D Waterfall / Spectrogram history view",
            },
            ShortcutEntry {
                key: "C / K",
                description: "Toggle measurement Cursors (A and B)",
            },
            ShortcutEntry {
                key: "P",
                description: "Toggle automatic Peak Tracking (PK+ and PK- markers)",
            },
        ],
    },
    ShortcutCategory {
        name: "Probes, Markers & Zoom",
        entries: &[
            ShortcutEntry {
                key: "Left Click",
                description: "Set Cursor A position on trace",
            },
            ShortcutEntry {
                key: "Right / Shift+Click",
                description: "Set Cursor B position on trace",
            },
            ShortcutEntry {
                key: "D",
                description: "Delete nearest pinned measurement marker",
            },
            ShortcutEntry {
                key: "Esc",
                description: "Clear pinned markers or close dialog modals",
            },
            ShortcutEntry {
                key: "Scroll / Drag",
                description: "Zoom enabled axes / Pan plot (Right-drag: box zoom)",
            },
        ],
    },
    ShortcutCategory {
        name: "Capture & Export",
        entries: &[
            ShortcutEntry {
                key: "S",
                description: "Quick Snapshot (save timestamped capture directly to disk)",
            },
            ShortcutEntry {
                key: "H / ?",
                description: "Toggle this Keyboard Shortcuts cheat sheet",
            },
        ],
    },
];

/// Displays the floating keyboard shortcuts modal window.
pub fn show_shortcuts_window(ctx: &egui::Context, open: &mut bool) {
    if !*open {
        return;
    }

    Window::new("⌨ Keyboard Shortcuts")
        .open(open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_width(480.0);
            ui.add_space(4.0);

            for cat in SHORTCUT_CATEGORIES {
                ui.heading(
                    RichText::new(cat.name)
                        .size(14.0)
                        .color(Color32::from_rgb(100, 180, 255)),
                );
                ui.add_space(2.0);

                egui::Grid::new(cat.name)
                    .striped(true)
                    .min_col_width(90.0)
                    .show(ui, |ui| {
                        for entry in cat.entries {
                            ui.colored_label(
                                Color32::from_rgb(255, 215, 0),
                                RichText::new(entry.key).monospace().strong(),
                            );
                            ui.label(entry.description);
                            ui.end_row();
                        }
                    });

                ui.add_space(8.0);
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("Press Esc to close");
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_categories_populated() {
        assert!(!SHORTCUT_CATEGORIES.is_empty());
        for cat in SHORTCUT_CATEGORIES {
            assert!(!cat.name.is_empty());
            assert!(!cat.entries.is_empty());
            for entry in cat.entries {
                assert!(!entry.key.is_empty());
                assert!(!entry.description.is_empty());
            }
        }
    }

    #[test]
    fn test_show_shortcuts_window_headless() {
        let ctx = egui::Context::default();
        let mut open = true;
        let _ = ctx.run(Default::default(), |ctx| {
            show_shortcuts_window(ctx, &mut open);
        });
        assert!(open);

        let mut closed = false;
        let _ = ctx.run(Default::default(), |ctx| {
            show_shortcuts_window(ctx, &mut closed);
        });
        assert!(!closed);
    }
}
