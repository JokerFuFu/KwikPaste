use super::*;
use kwikpaste_core::ImageOcrStatus;

#[derive(Default)]
pub(super) struct ImageOcrViewState {
    status: Option<ImageOcrStatus>,
    error: Option<String>,
    busy: bool,
    revision: u64,
    refreshing: bool,
    refresh_again: bool,
}

#[derive(Clone, Copy)]
enum ImageOcrAction {
    Enable(bool),
    Pause(bool),
    IndexHistory,
    Clear,
}

/// 采集、清理及数据根替换也会改变图片总数，状态不依赖偏好页当前是否可见。
pub(super) fn refresh_for_event(event: &CoreEvent) -> bool {
    match event {
        CoreEvent::SettingsUpdated { delta, .. } => delta.touches("clipboard.ocr"),
        CoreEvent::ImageOcrChanged
        | CoreEvent::ClipboardUpserted { .. }
        | CoreEvent::ClipboardCleaned { .. }
        | CoreEvent::ClipboardReloaded => true,
        _ => false,
    }
}

fn queue_status_key(status: &ImageOcrStatus) -> &'static str {
    if !status.enabled {
        "disabled"
    } else if status.paused {
        "paused"
    } else if status.pending > 0 {
        "running"
    } else {
        "idle"
    }
}

impl Preferences {
    /// 合并连续 OCR 事件的查询，并丢弃设置操作开始前已经发出的旧状态响应。
    pub(super) fn refresh_image_ocr(&mut self, cx: &mut Context<Self>) {
        self.image_ocr.revision = self.image_ocr.revision.wrapping_add(1);
        if self.image_ocr.refreshing || self.image_ocr.busy {
            self.image_ocr.refresh_again = true;
            return;
        }
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        self.image_ocr.refreshing = true;
        self.image_ocr.refresh_again = false;
        let revision = self.image_ocr.revision;
        cx.spawn(async move |this, cx| {
            let result = core.image_ocr_status().await;
            let _ = this.update(cx, |this, cx| {
                this.image_ocr.refreshing = false;
                if this.image_ocr.revision == revision {
                    match result {
                        Ok(status) => {
                            this.image_ocr.status = Some(status);
                            this.image_ocr.error = None;
                        }
                        Err(error) => {
                            log::warn!("image OCR status failed: {error:#}");
                            this.image_ocr.status = None;
                            this.image_ocr.error = Some(error.to_string());
                        }
                    }
                }
                if this.image_ocr.refresh_again {
                    this.refresh_image_ocr(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 设置和索引管理都交给异步 Core；识别、数据库查询不在渲染回调里执行。
    fn run_image_ocr_action(
        &mut self,
        action: ImageOcrAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.image_ocr.busy {
            return;
        }
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let confirmation = matches!(action, ImageOcrAction::Clear).then(|| {
            form_dialog(
                DialogSpec::new(i18n::t("preferences:imageOcr.clearTitle"))
                    .ok_text(i18n::t("preferences:imageOcr.clear"))
                    .cancel_text(i18n::t("common:actions.cancel"))
                    .danger(),
                |_, _| {
                    div()
                        .kp_text(TextSize::Sm)
                        .child(i18n::t("preferences:imageOcr.clearContent"))
                        .into_any_element()
                },
                window,
                cx,
            )
        });
        self.image_ocr.busy = true;
        self.image_ocr.revision = self.image_ocr.revision.wrapping_add(1);
        cx.notify();
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if let Some(confirmation) = confirmation
                    && !confirmation.await.unwrap_or(false)
                {
                    let _ = entity.update_in(cx, |this, _, cx| {
                        this.image_ocr.busy = false;
                        this.refresh_image_ocr(cx);
                        cx.notify();
                    });
                    return;
                }
                let result = async {
                    match action {
                        ImageOcrAction::Enable(enabled) => {
                            core.update_settings(
                                json!({"clipboard": {"ocr": {"enabled": enabled}}}),
                            )
                            .await?;
                            core.image_ocr_status().await
                        }
                        ImageOcrAction::Pause(paused) => {
                            core.update_settings(json!({"clipboard": {"ocr": {"paused": paused}}}))
                                .await?;
                            core.image_ocr_status().await
                        }
                        ImageOcrAction::IndexHistory => core.queue_image_ocr_history().await,
                        ImageOcrAction::Clear => core.clear_image_ocr().await,
                    }
                }
                .await;
                let _ = entity.update_in(cx, |this, window, cx| {
                    this.image_ocr.busy = false;
                    this.settings = core.settings();
                    match result {
                        Ok(status) => {
                            this.image_ocr.status = Some(status);
                            this.image_ocr.error = None;
                            if matches!(action, ImageOcrAction::Clear) {
                                toast::show(
                                    Toast::success(i18n::t("preferences:imageOcr.cleared")),
                                    window,
                                    cx,
                                );
                            }
                        }
                        Err(error) => {
                            log::warn!("image OCR action failed: {error:#}");
                            this.image_ocr.error = Some(error.to_string());
                            toast::show(
                                Toast::error(i18n::t_args(
                                    "preferences:imageOcr.error",
                                    &[("message", &error.to_string())],
                                )),
                                window,
                                cx,
                            );
                        }
                    }
                    this.refresh_image_ocr(cx);
                    cx.notify();
                });
            })
            .detach();
    }

    pub(super) fn render_image_ocr(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let state = &self.image_ocr;
        let status = state.status.as_ref();
        let supported = status.is_some_and(|status| status.supported);
        let enabled = self.settings.clipboard.ocr.enabled;
        let paused = self.settings.clipboard.ocr.paused;
        let unavailable = core_host::core(cx).is_none();
        let entity = cx.entity().downgrade();
        let enable = Switch::new("image-ocr-enable")
            .accessibility_label(i18n::t("preferences:imageOcr.enable"))
            .checked(enabled)
            .disabled(state.busy || unavailable || (!supported && !enabled))
            .on_change(move |enabled, window, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.run_image_ocr_action(ImageOcrAction::Enable(enabled), window, cx);
                });
            });
        let readiness = if state.error.is_some() && status.is_none() {
            "statusUnavailable"
        } else if unavailable || status.is_some_and(|status| !status.supported) {
            "unavailable"
        } else if status.is_some() {
            "ready"
        } else {
            "loading"
        };
        let readiness_color = if supported {
            tokens.status.success.solid
        } else {
            tokens.text.secondary
        };
        let mut panel =
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(space(3.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space(3.))
                        .child(i18n::t("preferences:imageOcr.enable"))
                        .child(enable),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(space(2.))
                        .child(
                            div()
                                .text_color(readiness_color)
                                .child(i18n::t(&format!("preferences:imageOcr.{readiness}"))),
                        )
                        .when_some(status, |row, status| {
                            row.child(div().text_color(tokens.text.secondary).child(i18n::t(
                                &format!("preferences:imageOcr.{}", queue_status_key(status)),
                            )))
                        }),
                );
        if let Some(status) = status {
            panel = panel.child(
                div()
                    .kp_text(TextSize::Sm)
                    .text_color(tokens.text.secondary)
                    .child(i18n::t_args(
                        "preferences:imageOcr.counts",
                        &[
                            ("total", &status.total.to_string()),
                            ("pending", &status.pending.to_string()),
                            ("completed", &status.completed.to_string()),
                            ("failed", &status.failed.to_string()),
                        ],
                    )),
            );
        }
        if let Some(error) = &state.error {
            panel = panel.child(
                div()
                    .kp_text(TextSize::Sm)
                    .text_color(tokens.status.danger.solid)
                    .child(i18n::t_args(
                        "preferences:imageOcr.error",
                        &[("message", error)],
                    )),
            );
        }
        panel
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space(2.))
                    .child(
                        Button::new(
                            "image-ocr-pause",
                            i18n::t(if paused {
                                "preferences:imageOcr.resume"
                            } else {
                                "preferences:imageOcr.pause"
                            }),
                        )
                        .disabled(state.busy || !enabled || !supported)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.run_image_ocr_action(
                                    ImageOcrAction::Pause(!paused),
                                    window,
                                    cx,
                                )
                            },
                        )),
                    )
                    .child(
                        Button::new(
                            "image-ocr-history",
                            i18n::t("preferences:imageOcr.indexHistory"),
                        )
                        .disabled(state.busy || !enabled || !supported)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.run_image_ocr_action(ImageOcrAction::IndexHistory, window, cx)
                        })),
                    )
                    .child(
                        Button::new("image-ocr-clear", i18n::t("preferences:imageOcr.clear"))
                            .danger_outline()
                            .disabled(state.busy || unavailable || status.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_image_ocr_action(ImageOcrAction::Clear, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_status_prioritizes_disabled_and_paused_over_pending_work() {
        let mut status = ImageOcrStatus {
            supported: true,
            enabled: false,
            paused: false,
            total: 3,
            pending: 2,
            completed: 1,
            failed: 0,
        };
        assert_eq!(queue_status_key(&status), "disabled");
        status.enabled = true;
        status.paused = true;
        assert_eq!(queue_status_key(&status), "paused");
        status.paused = false;
        assert_eq!(queue_status_key(&status), "running");
        status.pending = 0;
        assert_eq!(queue_status_key(&status), "idle");
    }

    #[test]
    fn image_ocr_status_refreshes_after_index_changes_and_history_replacement() {
        assert!(refresh_for_event(&CoreEvent::ImageOcrChanged));
        assert!(refresh_for_event(&CoreEvent::ClipboardReloaded));
        assert!(refresh_for_event(&CoreEvent::ClipboardCleaned {
            removed: 1
        }));
        assert!(!refresh_for_event(&CoreEvent::GroupsUpdated));
    }

    #[test]
    fn image_ocr_status_refreshes_for_ocr_settings_and_full_replacements() {
        use kwikpaste_core::settings::SettingsDelta;

        let event = |delta| CoreEvent::SettingsUpdated {
            settings: Arc::new(Settings::default()),
            delta,
        };
        assert!(refresh_for_event(&event(SettingsDelta::from_patch(
            &json!({
                "clipboard": { "ocr": { "enabled": false } }
            })
        ))));
        assert!(refresh_for_event(&event(SettingsDelta::replaced())));
        assert!(!refresh_for_event(&event(SettingsDelta::from_patch(
            &json!({
                "appearance": { "theme": "dark" }
            })
        ))));
    }
}
