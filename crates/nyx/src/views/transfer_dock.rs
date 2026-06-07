//! The bottom transfer dock: collapsible header (tabs + clear) and rows.

use gpui::{div, prelude::*, px, AnyElement, Context, Hsla, SharedString};
use nyx_core::{EntryOutcomeKind, TransferDirection, TransferStatus};
use nyx_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize, ProgressBar, Tabs,
};

use crate::icon::icon;
use crate::state::models::{fmt_bytes_pair, fmt_size, DockTab, TransferVm};
use crate::state::AppState;

/// Render the transfer dock.
pub fn render(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let open = state.dock_open;
    let height = if open { 218.0 } else { 32.0 };

    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .min_h_0()
        .h(px(height))
        .bg(theme.bg_panel)
        .border_t_1()
        .border_color(theme.border)
        .child(header(state, cx))
        .when(open, |this| this.child(body(state, cx)))
}

fn header(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let (all, active, completed, failed) = state.dock_counts();
    let open = state.dock_open;

    div()
        .flex()
        .items_center()
        .gap_0p5()
        .h(px(32.))
        .flex_shrink_0()
        .pl_1()
        .pr_1p5()
        .border_b_1()
        .border_color(theme.border_soft)
        .child(
            IconButton::new(
                "dock-collapse",
                icon(if open { "chevD" } else { "chevR" }, 14., theme.text_faint),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.dock_open = !this.dock_open;
                cx.notify();
            })),
        )
        .child(
            Tabs::new("dock-tabs")
                .tab("Transfers", Some(all))
                .tab("Active", Some(active))
                .tab("Completed", Some(completed))
                .tab("Failed", Some(failed))
                .selected(state.dock_tab.index())
                .on_select({
                    let view = cx.entity();
                    move |ix, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.dock_tab = DockTab::from_index(ix);
                            cx.notify();
                        });
                    }
                }),
        )
        .child(div().flex_1())
        .child(
            IconButton::new("dock-clear", icon("trash", 14., theme.text_faint)).on_click(
                cx.listener(|this, _, _, cx| {
                    this.clear_finished();
                    cx.notify();
                }),
            ),
        )
}

fn body(state: &AppState, cx: &mut Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let rows = state.dock_rows();

    let content: gpui::AnyElement = if rows.is_empty() {
        div()
            .flex()
            .items_center()
            .justify_center()
            .p(px(28.))
            .text_color(theme.text_dim)
            .text_sm()
            .child("No transfers here.")
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .children(rows.iter().map(|t| transfer_row(t, cx)).collect::<Vec<_>>())
            .into_any_element()
    };

    div()
        .id("dock-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(content)
}

fn transfer_row(t: &TransferVm, cx: &Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let mono = crate::assets::FONT_MONO;
    let status = t.transfer.status;
    let id = t.transfer.id;

    // A folder that completed with skips/failures carries a report — surface its
    // summary in a warning color and let the row expand to the per-entry detail.
    let has_report = status == TransferStatus::Completed && t.report.is_some();
    let summary = t.report.as_ref().and_then(|r| r.summary());
    let expanded = t.report_expanded;

    let (dir_icon, dir_color) = match t.transfer.direction {
        TransferDirection::Upload => ("upload", theme.blue),
        TransferDirection::Download => ("download", theme.green),
    };

    let name = t
        .transfer
        .remote_path
        .file_name()
        .unwrap_or("/")
        .to_string();
    let pct = (t.transfer.progress().unwrap_or(0.0) * 100.0).round() as u32;

    let speed_label = match status {
        TransferStatus::Running => t
            .speed_bps
            .map(|b| format!("{:.1} MB/s", b as f64 / 1_000_000.0))
            .unwrap_or_else(|| "—".into()),
        TransferStatus::Completed => fmt_size(t.transfer.total_bytes.unwrap_or(0)),
        _ => "—".to_string(),
    };

    let (status_color, status_label) = match status {
        TransferStatus::Running => (theme.blue, format!("{pct}%")),
        TransferStatus::Queued => (theme.text_faint, "Queued".to_string()),
        TransferStatus::AwaitingDecision => (theme.yellow, "Conflict".to_string()),
        TransferStatus::Completed => (theme.green, "Completed".to_string()),
        TransferStatus::Failed => (theme.red, "Failed".to_string()),
        TransferStatus::Cancelled => (theme.text_dim, "Cancelled".to_string()),
        TransferStatus::Skipped => (theme.text_dim, "Skipped".to_string()),
        TransferStatus::Interrupted => (theme.yellow, "Paused".to_string()),
    };

    // Interrupted keeps its bar at the retained watermark (it resumes on
    // reconnect) and stays cancellable so the user can abandon it.
    let show_bar = matches!(
        status,
        TransferStatus::Running | TransferStatus::Queued | TransferStatus::Interrupted
    );
    let show_cancel = show_bar || status == TransferStatus::AwaitingDecision;
    let (detail_text, detail_color): (SharedString, Hsla) = if status == TransferStatus::Failed {
        (
            t.error.clone().unwrap_or_else(|| "Transfer failed".into()),
            theme.red,
        )
    } else if let Some(summary) = summary {
        (summary.into(), theme.yellow)
    } else {
        (t.transfer.remote_path.as_str().into(), theme.text_faint)
    };

    let main = div()
        .id(SharedString::from(format!("xfer-row-{}", id.0)))
        .flex()
        .items_center()
        .gap_2p5()
        .px_3()
        .py(px(7.))
        .hover(|s| s.bg(theme.bg_hover))
        .when(has_report, |this| {
            this.cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_transfer_report(id);
                    cx.notify();
                }))
        })
        .child(
            div()
                .w(px(18.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .child(icon(dir_icon, 15., dir_color)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .font_family(mono)
                        .text_xs()
                        .text_color(theme.text)
                        .truncate()
                        .child(name.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .when(has_report, |this| {
                            this.child(icon("alert", 11., theme.yellow))
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(detail_color)
                                .truncate()
                                .child(detail_text),
                        ),
                )
                .when(show_bar, |this| {
                    this.child(div().mt_1p5().child(ProgressBar::new(
                        gpui::SharedString::from(format!("bar-{}", t.transfer.id.0)),
                        t.transfer.progress().unwrap_or(0.0),
                    )))
                }),
        )
        .child(
            div()
                .w(px(90.))
                .flex_shrink_0()
                .font_family(mono)
                .text_xs()
                .text_color(theme.text_muted)
                .text_right()
                .child(fmt_bytes_pair(&t.transfer)),
        )
        .child(
            div()
                .w(px(84.))
                .flex_shrink_0()
                .font_family(mono)
                .text_xs()
                .text_color(theme.text_muted)
                .text_right()
                .child(speed_label),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_1p5()
                .w(px(112.))
                .flex_shrink_0()
                .when(has_report, |this| {
                    this.child(icon(
                        if expanded { "chevD" } else { "chevR" },
                        13.,
                        theme.text_faint,
                    ))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_xs()
                        .text_color(status_color)
                        .child(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(status_color)
                                .flex_shrink_0(),
                        )
                        .child(status_label),
                )
                .when(show_cancel, |this| this.child(cancel_button(t, cx)))
                .when(status == TransferStatus::Failed, |this| {
                    this.child(retry_button(t, cx))
                }),
        );

    div()
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(theme.border_soft)
        .child(main)
        .when(has_report && expanded, |this| {
            this.child(report_panel(t, cx))
        })
}

/// The expanded per-entry report under a completed-with-issues folder row: the
/// failed and skipped paths with reasons (grouped, scrollable) plus a copy
/// action. Only rendered when `t.report` is `Some`.
fn report_panel(t: &TransferVm, cx: &Context<AppState>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let mono = crate::assets::FONT_MONO;
    let id = t.transfer.id;
    let report = t.report.clone().unwrap_or_default();

    let group = |kind: EntryOutcomeKind, label: &str, color: Hsla| -> Option<AnyElement> {
        let rows: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.kind == kind)
            .map(|i| {
                div()
                    .flex()
                    .gap_2()
                    .font_family(mono)
                    .text_xs()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(i.rel.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme.text_faint)
                            .child(i.reason.clone()),
                    )
            })
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_xs().text_color(color).child(label.to_string()))
                .children(rows)
                .into_any_element(),
        )
    };

    let truncated = report.truncated();

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .px_3()
        .pb_2()
        .pl(px(38.))
        .bg(theme.bg_app)
        .child(
            div()
                .id(SharedString::from(format!("xfer-report-{}", id.0)))
                .flex()
                .flex_col()
                .gap_2()
                .max_h(px(132.))
                .overflow_y_scroll()
                .py_1()
                .children(group(EntryOutcomeKind::Failed, "Failed", theme.red))
                .children(group(EntryOutcomeKind::Skipped, "Skipped", theme.text_dim))
                .when(truncated > 0, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.text_faint)
                            .child(format!("…and {truncated} more")),
                    )
                }),
        )
        .child(
            div().flex().child(
                Button::new(
                    SharedString::from(format!("xfer-copy-report-{}", id.0)),
                    "Copy report",
                )
                .size(ButtonSize::Sm)
                .variant(ButtonVariant::Secondary)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy_transfer_report(id, cx);
                    cx.notify();
                })),
            ),
        )
}

/// The cancel (`x`) button on a running/queued row — sends a real cancel command.
fn cancel_button(t: &TransferVm, cx: &Context<AppState>) -> impl IntoElement {
    let id = t.transfer.id;
    IconButton::new(
        gpui::SharedString::from(format!("xfer-cancel-{}", id.0)),
        icon("x", 13., cx.theme().text_faint),
    )
    .size(IconButtonSize::Xs)
    .on_click(cx.listener(move |this, _, _, cx| {
        this.cancel_transfer(id);
        cx.notify();
    }))
}

/// The retry (`refresh`) button on a failed row — re-issues the transfer.
fn retry_button(t: &TransferVm, cx: &Context<AppState>) -> impl IntoElement {
    let id = t.transfer.id;
    IconButton::new(
        gpui::SharedString::from(format!("xfer-retry-{}", id.0)),
        icon("refresh", 13., cx.theme().text_faint),
    )
    .size(IconButtonSize::Xs)
    .on_click(cx.listener(move |this, _, _, cx| {
        this.retry_transfer(id, cx);
        cx.notify();
    }))
}
