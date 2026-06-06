//! The bottom transfer dock: collapsible header (tabs + clear) and rows.

use gpui::{div, prelude::*, px, Context};
use nyx_core::{TransferDirection, TransferStatus};
use nyx_ui::{ActiveTheme, IconButton, IconButtonSize, ProgressBar, Tabs};

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

    let (dir_icon, dir_color) = match t.transfer.direction {
        TransferDirection::Upload => ("upload", theme.blue),
        TransferDirection::Download => ("download", theme.green),
    };

    let name = t
        .transfer
        .remote_path
        .rsplit('/')
        .next()
        .unwrap_or(&t.transfer.remote_path)
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

    // Status shows as a small colored dot + label (matching the design's
    // `.xstatus`), not an oval badge.
    let (status_color, status_label) = match status {
        TransferStatus::Running => (theme.blue, format!("{pct}%")),
        TransferStatus::Queued => (theme.text_faint, "Queued".to_string()),
        TransferStatus::Completed => (theme.green, "Completed".to_string()),
        TransferStatus::Failed => (theme.red, "Failed".to_string()),
        TransferStatus::Cancelled => (theme.text_dim, "Cancelled".to_string()),
    };

    let show_bar = matches!(status, TransferStatus::Running | TransferStatus::Queued);
    let path_or_error = if status == TransferStatus::Failed {
        (
            t.error.clone().unwrap_or_else(|| "Transfer failed".into()),
            theme.red,
        )
    } else {
        (t.transfer.remote_path.clone().into(), theme.text_faint)
    };

    div()
        .flex()
        .items_center()
        .gap_2p5()
        .px_3()
        .py(px(7.))
        .border_b_1()
        .border_color(theme.border_soft)
        .hover(|s| s.bg(theme.bg_hover))
        .child(
            div()
                .w(px(18.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .child(icon(dir_icon, 15., dir_color)),
        )
        .child(
            // Main: name + path/error + optional progress bar.
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
                        .text_xs()
                        .text_color(path_or_error.1)
                        .truncate()
                        .child(path_or_error.0),
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
                .when(show_bar, |this| this.child(cancel_button(t, cx)))
                .when(status == TransferStatus::Failed, |this| {
                    this.child(retry_button(t, cx))
                }),
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
