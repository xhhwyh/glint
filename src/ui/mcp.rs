use std::collections::BTreeSet;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, McpAddField, McpAddForm, McpAddTransport, McpFocus, McpScreen, McpView},
    event::{McpMouseAction, MouseAction},
    input::InputState,
    services::mcp::{
        McpApprovalPolicy, McpCapabilityStatus, McpConnectionState, McpServerStatus,
        McpTransportConfig,
    },
};

use super::{
    layout::{truncate_end_to_width, wrap_text},
    theme::*,
};

const READY_COLOR: Color = Color::Rgb(74, 222, 128);
const STARTING_COLOR: Color = Color::Rgb(34, 211, 238);
const WARNING_COLOR: Color = Color::Rgb(251, 191, 36);
const ERROR_COLOR: Color = Color::Rgb(248, 113, 113);
const STOPPED_COLOR: Color = Color::Rgb(100, 116, 139);
const SELECTED_BG_COLOR: Color = Color::Rgb(15, 23, 42);

#[derive(Clone, Copy)]
enum McpDetailMode {
    Overview,
    Full,
}

pub(super) fn render_mcp_view(frame: &mut Frame, app: &App, view: &McpView) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG_COLOR)),
        frame.area(),
    );

    let areas = mcp_view_areas(frame.area());
    let statuses = app.mcp_statuses();
    render_header(frame, &statuses, &view.screen, areas[0]);
    match &view.screen {
        McpScreen::Browse => render_browse(frame, app, view, &statuses, areas[1]),
        McpScreen::Details => render_full_details(frame, app, view, &statuses, areas[1]),
        McpScreen::Add(form) => render_add_server(frame, view, form, areas[1]),
        McpScreen::OAuth {
            server,
            authorization_url,
            callback,
        } => render_oauth(frame, view, server, authorization_url, callback, areas[1]),
        McpScreen::ConfirmLogout { server } => render_logout_confirmation(frame, server, areas[1]),
    }
    render_footer(frame, view, areas[2]);
}

pub(super) fn detail_max_scroll(app: &App, width: u16, height: u16) -> usize {
    let Some(view) = app.mcp_view.as_ref() else {
        return 0;
    };
    let statuses = app.mcp_statuses();
    let body = mcp_view_areas(Rect::new(0, 0, width, height))[1];
    let Some(status) = view
        .selected
        .checked_sub(1)
        .and_then(|index| statuses.get(index))
    else {
        return 0;
    };

    let (inner, mode) = match view.screen {
        McpScreen::Browse => {
            let detail_panel = browse_panel_areas(body)[1];
            let block = panel_block(
                &format!("Server · {}", status.name),
                view.focus == McpFocus::Details,
            );
            (block.inner(detail_panel), McpDetailMode::Overview)
        }
        McpScreen::Details => {
            let panel = full_details_panel(body);
            let block = panel_block(&format!("{} · full details", status.name), true);
            (block.inner(panel), McpDetailMode::Full)
        }
        _ => return 0,
    };
    let lines = server_detail_lines(
        app,
        status,
        inner.width as usize,
        view.notice.as_ref(),
        mode,
    );
    detail_scroll_limit(lines, inner.width, inner.height)
}

pub(super) fn mouse_action(
    app: &App,
    mouse: MouseAction,
    width: u16,
    height: u16,
) -> McpMouseAction {
    let Some(view) = app.mcp_view.as_ref() else {
        return McpMouseAction::None;
    };
    let body = mcp_view_areas(Rect::new(0, 0, width, height))[1];
    match &view.screen {
        McpScreen::Browse => browse_mouse_action(app, view, mouse, body),
        McpScreen::Details => {
            let panel = full_details_panel(body);
            match mouse {
                MouseAction::ScrollUp { column, row }
                    if panel.contains(Position::new(column, row)) =>
                {
                    McpMouseAction::ScrollDetails(-3)
                }
                MouseAction::ScrollDown { column, row }
                    if panel.contains(Position::new(column, row)) =>
                {
                    McpMouseAction::ScrollDetails(3)
                }
                _ => McpMouseAction::None,
            }
        }
        _ => McpMouseAction::None,
    }
}

fn browse_mouse_action(
    app: &App,
    view: &McpView,
    mouse: MouseAction,
    area: Rect,
) -> McpMouseAction {
    let panels = browse_panel_areas(area);
    let list_inner = panel_block("Servers", view.focus == McpFocus::Servers).inner(panels[0]);
    let row_count = app.mcp_statuses().len() + 1;
    let selected = view.selected.min(row_count - 1);
    let start = list_window_start(selected, row_count, list_inner.height as usize);

    match mouse {
        MouseAction::LeftDown { column, row }
            if list_inner.contains(Position::new(column, row)) =>
        {
            let selected = start + usize::from(row.saturating_sub(list_inner.y));
            if selected < row_count {
                McpMouseAction::SelectServer(selected)
            } else {
                McpMouseAction::None
            }
        }
        MouseAction::LeftDown { column, row } if panels[1].contains(Position::new(column, row)) => {
            McpMouseAction::OpenSelected
        }
        MouseAction::ScrollUp { column, row } if panels[0].contains(Position::new(column, row)) => {
            McpMouseAction::MoveServerSelection(-1)
        }
        MouseAction::ScrollDown { column, row }
            if panels[0].contains(Position::new(column, row)) =>
        {
            McpMouseAction::MoveServerSelection(1)
        }
        MouseAction::ScrollUp { column, row } if panels[1].contains(Position::new(column, row)) => {
            McpMouseAction::ScrollDetails(-3)
        }
        MouseAction::ScrollDown { column, row }
            if panels[1].contains(Position::new(column, row)) =>
        {
            McpMouseAction::ScrollDetails(3)
        }
        _ => McpMouseAction::None,
    }
}

fn mcp_view_areas(area: Rect) -> [Rect; 3] {
    let areas = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);
    [areas[0], areas[1], areas[2]]
}

fn render_header(frame: &mut Frame, statuses: &[McpServerStatus], screen: &McpScreen, area: Rect) {
    let ready = statuses
        .iter()
        .filter(|status| status.state == McpConnectionState::Ready)
        .count();
    let failed = statuses
        .iter()
        .filter(|status| status.state == McpConnectionState::Failed)
        .count();
    let tools = statuses
        .iter()
        .map(|status| status.tools.len())
        .sum::<usize>();
    let title = Line::from(vec![
        Span::styled(
            " MCP servers ",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{} configured  ·  {ready} ready  ·  {tools} tools",
                statuses.len()
            ),
            Style::default().fg(MUTED_TEXT_COLOR),
        ),
        if failed > 0 {
            Span::styled(
                format!("  ·  {failed} failed"),
                Style::default().fg(ERROR_COLOR),
            )
        } else {
            Span::raw("")
        },
    ]);
    let description = match screen {
        McpScreen::Browse => "Inspect connections, registered capabilities, and permissions",
        McpScreen::Details => "Full server details",
        McpScreen::Add(_) => "Add a standalone MCP server and activate it immediately",
        McpScreen::OAuth { .. } => "Complete browser authorization",
        McpScreen::ConfirmLogout { .. } => "Remove stored OAuth credentials",
    };
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(BORDER_COLOR));
    frame.render_widget(
        Paragraph::new(vec![
            title,
            Line::from(Span::styled(
                format!(" {description}"),
                Style::default().fg(SOFT_TEXT_COLOR),
            )),
            Line::from(vec![
                Span::raw(" "),
                Span::styled("● Ready", Style::default().fg(READY_COLOR)),
                Span::styled("  │  ", Style::default().fg(BORDER_COLOR)),
                Span::styled("◐ Starting", Style::default().fg(STARTING_COLOR)),
                Span::styled("  │  ", Style::default().fg(BORDER_COLOR)),
                Span::styled("× Failed", Style::default().fg(ERROR_COLOR)),
                Span::styled("  │  ", Style::default().fg(BORDER_COLOR)),
                Span::styled("○ Stopped", Style::default().fg(STOPPED_COLOR)),
            ]),
        ])
        .block(block)
        .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn render_browse(
    frame: &mut Frame,
    app: &App,
    view: &McpView,
    statuses: &[McpServerStatus],
    area: Rect,
) {
    let panels = browse_panel_areas(area);

    render_server_list(frame, view, statuses, panels[0]);
    render_selected_server(frame, app, view, statuses, panels[1]);
}

fn browse_panel_areas(area: Rect) -> [Rect; 2] {
    let direction = if area.width >= 88 && area.height >= 12 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    let constraints = match direction {
        Direction::Horizontal => [Constraint::Percentage(38), Constraint::Percentage(62)],
        Direction::Vertical => [Constraint::Percentage(42), Constraint::Percentage(58)],
    };
    let panels = Layout::default()
        .direction(direction)
        .constraints(constraints)
        .spacing(1)
        .split(area);
    [panels[0], panels[1]]
}

fn render_server_list(frame: &mut Frame, view: &McpView, statuses: &[McpServerStatus], area: Rect) {
    let block = panel_block("Servers", view.focus == McpFocus::Servers);
    let inner = block.inner(area);
    let row_count = statuses.len() + 1;
    let selected = view.selected.min(row_count - 1);
    let start = list_window_start(selected, row_count, inner.height as usize);
    let lines = (start..row_count)
        .take(inner.height as usize)
        .map(|index| {
            if index == 0 {
                add_server_line(selected == 0, inner.width as usize)
            } else {
                server_line(
                    &statuses[index - 1],
                    index == selected,
                    inner.width as usize,
                )
            }
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn server_line(status: &McpServerStatus, selected: bool, width: usize) -> Line<'static> {
    let (icon, _, color) = state_style(status.state);
    let marker = if selected { "›" } else { " " };
    let used = marker.width() + 1 + icon.width() + 1;
    let name = truncate_end_to_width(&status.name, width.saturating_sub(used));
    let content_width = used + name.width();
    let padding = " ".repeat(width.saturating_sub(content_width));
    let row_style = if selected {
        Style::default()
            .bg(SELECTED_BG_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(format!("{marker} "), row_style.fg(ACCENT_COLOR)),
        Span::styled(format!("{icon} "), row_style.fg(color)),
        Span::styled(name, row_style.fg(TEXT_COLOR)),
        Span::styled(padding, row_style),
    ])
}

fn add_server_line(selected: bool, width: usize) -> Line<'static> {
    let marker = if selected { "›" } else { " " };
    let label = "Add MCP server";
    let used = marker.width() + 1 + "＋".width() + 1;
    let label = truncate_end_to_width(label, width.saturating_sub(used));
    let padding = " ".repeat(width.saturating_sub(used + label.width()));
    let row_style = if selected {
        Style::default()
            .bg(SELECTED_BG_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(format!("{marker} "), row_style.fg(ACCENT_COLOR)),
        Span::styled("＋ ", row_style.fg(READY_COLOR)),
        Span::styled(label, row_style.fg(TEXT_COLOR)),
        Span::styled(padding, row_style),
    ])
}

fn render_selected_server(
    frame: &mut Frame,
    app: &App,
    view: &McpView,
    statuses: &[McpServerStatus],
    area: Rect,
) {
    let status = view
        .selected
        .checked_sub(1)
        .and_then(|index| statuses.get(index));
    let title = status
        .map(|status| format!("Server · {}", status.name))
        .unwrap_or_else(|| "Add a server".to_owned());
    let block = panel_block(&title, view.focus == McpFocus::Details);
    let inner = block.inner(area);
    let lines = status
        .map(|status| {
            server_detail_lines(
                app,
                status,
                inner.width as usize,
                view.notice.as_ref(),
                McpDetailMode::Overview,
            )
        })
        .unwrap_or_else(|| {
            vec![
                Line::from(Span::styled(
                    "＋ Add a standalone MCP server",
                    Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
                )),
                Line::default(),
                Line::from(Span::styled(
                    "Press Enter to configure stdio, Streamable HTTP, or OAuth.",
                    Style::default().fg(MUTED_TEXT_COLOR),
                )),
                Line::from(Span::styled(
                    "The server will be saved to config.yaml and activated immediately.",
                    Style::default().fg(MUTED_TEXT_COLOR),
                )),
            ]
        });
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll_value(view.detail_scroll), 0))
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn render_full_details(
    frame: &mut Frame,
    app: &App,
    view: &McpView,
    statuses: &[McpServerStatus],
    area: Rect,
) {
    let Some(status) = view
        .selected
        .checked_sub(1)
        .and_then(|index| statuses.get(index))
    else {
        return;
    };
    let panel = full_details_panel(area);
    let block = panel_block(&format!("{} · full details", status.name), true);
    let inner = block.inner(panel);
    frame.render_widget(
        Paragraph::new(server_detail_lines(
            app,
            status,
            inner.width as usize,
            view.notice.as_ref(),
            McpDetailMode::Full,
        ))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_value(view.detail_scroll), 0))
        .style(Style::default().bg(BG_COLOR)),
        panel,
    );
}

fn full_details_panel(area: Rect) -> Rect {
    inset_area(area, 1, if area.width >= 100 { 8 } else { 1 })
}

fn detail_scroll_limit(lines: Vec<Line<'static>>, width: u16, height: u16) -> usize {
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .line_count(width)
        .saturating_sub(height as usize)
}

fn server_detail_lines(
    app: &App,
    status: &McpServerStatus,
    width: usize,
    notice: Option<&crate::app::McpNotice>,
    mode: McpDetailMode,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(notice) = notice {
        lines.push(Line::from(vec![
            Span::styled(
                if notice.failed { "× " } else { "✓ " },
                Style::default().fg(if notice.failed {
                    ERROR_COLOR
                } else {
                    READY_COLOR
                }),
            ),
            Span::styled(
                notice.message.clone(),
                Style::default().fg(if notice.failed {
                    ERROR_COLOR
                } else {
                    SOFT_TEXT_COLOR
                }),
            ),
        ]));
        lines.push(Line::default());
    }
    if let Some(error) = &status.error {
        lines.push(section_heading("×", "Error", None, ERROR_COLOR, width));
        lines.extend(indented_wrapped(error, width, ERROR_COLOR));
        lines.push(Line::default());
    }

    lines.push(section_heading(
        "◇",
        "Connection",
        None,
        BORDER_BRIGHT_COLOR,
        width,
    ));
    let (icon, state, color) = state_style(status.state);
    lines.push(field_line(
        "Status",
        vec![
            Span::styled(format!("{icon} "), Style::default().fg(color)),
            Span::styled(state, Style::default().fg(color)),
        ],
    ));
    lines.push(field_text("Origin", &server_origin(app, &status.name)));

    let config = app.config.mcp.servers.get(&status.name);
    if let Some(config) = config {
        lines.push(field_text(
            "Enabled",
            if config.enabled { "yes" } else { "no" },
        ));
        lines.push(field_text(
            "Timeouts",
            &format!(
                "startup {} ms · tool {} ms",
                config.startup_timeout_ms, config.tool_timeout_ms
            ),
        ));
        append_transport_lines(&mut lines, &config.transport);
        if !config.enabled {
            lines.push(Line::from(Span::styled(
                "  Enable this server in config.yaml or in the plugin that contributes it.",
                Style::default().fg(WARNING_COLOR),
            )));
        }
    }

    append_tool_lines(&mut lines, &status.tools, width, mode);
    append_capability_lines(&mut lines, "▱", "Resources", &status.resources, width, mode);
    append_capability_lines(
        &mut lines,
        "⌁",
        "Resource templates",
        &status.resource_templates,
        width,
        mode,
    );
    append_capability_lines(&mut lines, "✦", "Prompts", &status.prompts, width, mode);

    lines.push(section_heading(
        "⚿",
        "Permissions",
        None,
        BORDER_BRIGHT_COLOR,
        width,
    ));
    if let Some(config) = config {
        lines.push(field_text("Default", approval_label(config.approval)));
        if config.tool_approval.is_empty() {
            lines.push(dim_line("  No per-tool approval overrides."));
        } else {
            for (tool, approval) in &config.tool_approval {
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(ACCENT_COLOR)),
                    Span::styled(tool.clone(), Style::default().fg(TEXT_COLOR)),
                    Span::styled(
                        format!("  {}", approval_label(*approval)),
                        Style::default().fg(approval_color(*approval)),
                    ),
                ]));
            }
        }
        if let Some(enabled) = &config.enabled_tools {
            lines.push(field_text("Enabled tools", &enabled.join(", ")));
        }
        if !config.disabled_tools.is_empty() {
            lines.push(field_text(
                "Disabled tools",
                &config.disabled_tools.join(", "),
            ));
        }
    } else {
        lines.push(dim_line("  Configuration is no longer available."));
    }
    lines
}

fn append_transport_lines(lines: &mut Vec<Line<'static>>, transport: &McpTransportConfig) {
    match transport {
        McpTransportConfig::Stdio {
            command,
            args,
            env,
            env_vars,
            cwd,
        } => {
            lines.push(field_text("Transport", "stdio"));
            lines.push(field_text("Command", command));
            if !args.is_empty() {
                lines.push(field_text("Arguments", &args.join(" ")));
            }
            if let Some(cwd) = cwd {
                lines.push(field_text("Working dir", cwd));
            }
            let names = env
                .keys()
                .chain(env_vars.iter())
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if !names.is_empty() {
                lines.push(field_text("Environment", &names.join(", ")));
            }
        }
        McpTransportConfig::StreamableHttp {
            url,
            headers,
            bearer_token_env,
            oauth,
        } => {
            lines.push(field_text("Transport", "Streamable HTTP"));
            lines.push(field_text("URL", &redacted_url(url)));
            let authentication = if oauth.is_some() {
                "OAuth".to_owned()
            } else if let Some(variable) = bearer_token_env {
                format!("Bearer token from {variable}")
            } else {
                "None".to_owned()
            };
            lines.push(field_text("Authentication", &authentication));
            if !headers.is_empty() {
                lines.push(field_text(
                    "Header names",
                    &headers.keys().cloned().collect::<Vec<_>>().join(", "),
                ));
            }
            if let Some(oauth) = oauth {
                lines.push(field_text(
                    "Redirect URI",
                    &redacted_url(&oauth.redirect_uri),
                ));
                if !oauth.scopes.is_empty() {
                    lines.push(field_text("OAuth scopes", &oauth.scopes.join(", ")));
                }
            }
        }
    }
}

fn append_tool_lines(
    lines: &mut Vec<Line<'static>>,
    tools: &[crate::services::mcp::McpToolStatus],
    width: usize,
    mode: McpDetailMode,
) {
    lines.push(Line::default());
    lines.push(section_heading(
        "◆",
        "Tools",
        Some(tools.len()),
        BORDER_BRIGHT_COLOR,
        width,
    ));
    if tools.is_empty() {
        lines.push(dim_line("  No tools advertised."));
        return;
    }
    for tool in tools {
        let access = if tool.read_only {
            "read-only"
        } else {
            "mutating"
        };
        let metadata = match mode {
            McpDetailMode::Overview => {
                format!("{} · {access}", approval_label(tool.approval))
            }
            McpDetailMode::Full => format!(
                "{} · {access} · {} ms",
                approval_label(tool.approval),
                tool.timeout_ms
            ),
        };
        let prefix = "  • ";
        let metadata = truncate_end_to_width(
            &format!("  {metadata}"),
            width.saturating_sub(prefix.width()).saturating_div(2),
        );
        let name = truncate_end_to_width(
            &tool.name,
            width.saturating_sub(prefix.width() + metadata.width()),
        );
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(ACCENT_COLOR)),
            Span::styled(
                name,
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
            Span::styled(metadata, Style::default().fg(approval_color(tool.approval))),
        ]));
        if matches!(mode, McpDetailMode::Full) && !tool.description.trim().is_empty() {
            lines.extend(bounded_indented(
                &tool.description,
                width,
                MUTED_TEXT_COLOR,
                3,
            ));
        }
    }
}

fn append_capability_lines(
    lines: &mut Vec<Line<'static>>,
    icon: &str,
    title: &str,
    capabilities: &[McpCapabilityStatus],
    width: usize,
    mode: McpDetailMode,
) {
    lines.push(Line::default());
    lines.push(section_heading(
        icon,
        title,
        Some(capabilities.len()),
        BORDER_BRIGHT_COLOR,
        width,
    ));
    if capabilities.is_empty() {
        lines.push(dim_line(&format!(
            "  No {} advertised.",
            title.to_lowercase()
        )));
        return;
    }
    for capability in capabilities {
        let name = truncate_end_to_width(&capability.name, width.saturating_sub(4));
        lines.push(Line::from(vec![
            Span::styled("  • ", Style::default().fg(ACCENT_COLOR)),
            Span::styled(
                name,
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
        ]));
        if let Some(detail) = &capability.detail {
            lines.extend(bounded_indented(detail, width, SOFT_TEXT_COLOR, 2));
        }
        if matches!(mode, McpDetailMode::Full)
            && let Some(description) = &capability.description
            && !description.trim().is_empty()
        {
            lines.extend(bounded_indented(description, width, MUTED_TEXT_COLOR, 3));
        }
    }
}

fn render_add_server(frame: &mut Frame, view: &McpView, form: &McpAddForm, area: Rect) {
    let panel = inset_area(area, 1, if area.width >= 100 { 8 } else { 1 });
    let block = panel_block("＋ Add MCP server", true);
    let inner = block.inner(panel);
    let mut lines = vec![
        Line::from(Span::styled(
            "Configure a standalone server",
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Secrets should be referenced by environment-variable name, never entered as values.",
            Style::default().fg(MUTED_TEXT_COLOR),
        )),
        view.notice.as_ref().map_or_else(Line::default, |notice| {
            Line::from(vec![
                Span::styled(
                    if notice.failed { "× " } else { "✓ " },
                    Style::default().fg(if notice.failed {
                        ERROR_COLOR
                    } else {
                        READY_COLOR
                    }),
                ),
                Span::styled(
                    notice.message.clone(),
                    Style::default().fg(if notice.failed {
                        ERROR_COLOR
                    } else {
                        SOFT_TEXT_COLOR
                    }),
                ),
            ])
        }),
        section_heading(
            "◇",
            "Server configuration",
            None,
            BORDER_BRIGHT_COLOR,
            inner.width as usize,
        ),
    ];
    for (index, field) in form.fields().iter().copied().enumerate() {
        lines.push(add_form_field_line(
            form,
            field,
            index == form.focus,
            inner.width as usize,
        ));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        add_field_help(form.selected_field()),
        Style::default().fg(MUTED_TEXT_COLOR),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().bg(BG_COLOR)),
        panel,
    );

    if let Some(input) = add_form_input(form, form.selected_field()) {
        let prefix_width = add_field_prefix(form.selected_field()).width();
        let available = (inner.width as usize).saturating_sub(prefix_width);
        let (_, cursor_x) = input_window(input, available);
        let cursor_y = 4_u16.saturating_add(form.focus as u16);
        if cursor_y < inner.height && inner.width > 0 {
            frame.set_cursor_position(Position::new(
                inner.x
                    + (prefix_width as u16)
                        .saturating_add(cursor_x)
                        .min(inner.width.saturating_sub(1)),
                inner.y + cursor_y,
            ));
        }
    }
}

fn add_form_field_line(
    form: &McpAddForm,
    field: McpAddField,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let prefix = add_field_prefix(field);
    let available = width.saturating_sub(prefix.width());
    let (value, placeholder) = if let Some(input) = add_form_input(form, field) {
        if input.value.is_empty() {
            (add_field_placeholder(field).to_owned(), true)
        } else {
            (input_window(input, available).0, false)
        }
    } else {
        let value = match field {
            McpAddField::Transport => match form.transport {
                McpAddTransport::Stdio => "‹ stdio ›",
                McpAddTransport::StreamableHttp => "‹ Streamable HTTP ›",
                McpAddTransport::OAuth => "‹ Streamable HTTP + OAuth ›",
            },
            McpAddField::Approval => match form.approval {
                McpApprovalPolicy::Allow => "‹ allow ›",
                McpApprovalPolicy::Prompt => "‹ prompt ›",
                McpApprovalPolicy::Deny => "‹ deny ›",
            },
            _ => "",
        };
        (value.to_owned(), false)
    };
    let value = truncate_end_to_width(&value, available);
    let padding = " ".repeat(width.saturating_sub(prefix.width() + value.width()));
    let row_style = if selected {
        Style::default().bg(SELECTED_BG_COLOR)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(
            prefix,
            row_style
                .fg(if selected {
                    ACCENT_COLOR
                } else {
                    MUTED_TEXT_COLOR
                })
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::styled(
            value,
            row_style.fg(if placeholder {
                STOPPED_COLOR
            } else {
                TEXT_COLOR
            }),
        ),
        Span::styled(padding, row_style),
    ])
}

fn add_field_prefix(field: McpAddField) -> String {
    let label = match field {
        McpAddField::Transport => "Transport",
        McpAddField::Name => "Server name",
        McpAddField::Command => "Command",
        McpAddField::Arguments => "Arguments",
        McpAddField::WorkingDirectory => "Working directory",
        McpAddField::EnvironmentVariables => "Environment vars",
        McpAddField::Url => "Server URL",
        McpAddField::BearerTokenEnv => "Bearer token env",
        McpAddField::RedirectUri => "Redirect URI",
        McpAddField::Scopes => "OAuth scopes",
        McpAddField::Approval => "Approval",
    };
    format!("  {label:<20}")
}

fn add_form_input(form: &McpAddForm, field: McpAddField) -> Option<&InputState> {
    match field {
        McpAddField::Name => Some(&form.name),
        McpAddField::Command => Some(&form.command),
        McpAddField::Arguments => Some(&form.arguments),
        McpAddField::WorkingDirectory => Some(&form.working_directory),
        McpAddField::EnvironmentVariables => Some(&form.environment_variables),
        McpAddField::Url => Some(&form.url),
        McpAddField::BearerTokenEnv => Some(&form.bearer_token_env),
        McpAddField::RedirectUri => Some(&form.redirect_uri),
        McpAddField::Scopes => Some(&form.scopes),
        McpAddField::Transport | McpAddField::Approval => None,
    }
}

fn add_field_placeholder(field: McpAddField) -> &'static str {
    match field {
        McpAddField::Name => "required; for example filesystem",
        McpAddField::Command => "required; for example npx",
        McpAddField::Arguments => "shell syntax; quotes are supported",
        McpAddField::WorkingDirectory => "optional; defaults to the workspace",
        McpAddField::EnvironmentVariables => "optional comma-separated names",
        McpAddField::Url => "required; https://example.com/mcp",
        McpAddField::BearerTokenEnv => "optional environment-variable name",
        McpAddField::RedirectUri => "required callback URL",
        McpAddField::Scopes => "optional comma-separated scopes",
        McpAddField::Transport | McpAddField::Approval => "",
    }
}

fn add_field_help(field: McpAddField) -> &'static str {
    match field {
        McpAddField::Transport => "Use Left/Right to choose stdio, HTTP, or OAuth.",
        McpAddField::Name => "Names may contain letters, numbers, dots, hyphens, and underscores.",
        McpAddField::Command => "The executable must be available on Glint's PATH.",
        McpAddField::Arguments => "Example: -y @modelcontextprotocol/server-filesystem .",
        McpAddField::WorkingDirectory => "Relative paths are resolved from the current workspace.",
        McpAddField::EnvironmentVariables => {
            "Values are inherited from Glint's environment and are never stored."
        }
        McpAddField::Url => "Only HTTP and HTTPS Streamable HTTP endpoints are accepted.",
        McpAddField::BearerTokenEnv => {
            "The environment variable should contain the bearer token value."
        }
        McpAddField::RedirectUri => "This URI must match the OAuth client callback configuration.",
        McpAddField::Scopes => "Leave empty when the server does not require explicit scopes.",
        McpAddField::Approval => "Prompt is the safe default for newly discovered tools.",
    }
}

fn render_oauth(
    frame: &mut Frame,
    view: &McpView,
    server: &str,
    authorization_url: &str,
    callback: &InputState,
    area: Rect,
) {
    let panel = inset_area(area, 1, if area.width >= 100 { 8 } else { 1 });
    let block = panel_block(&format!("Authorize · {server}"), true);
    let inner = block.inner(panel);
    frame.render_widget(block, panel);

    let rows = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(3),
        Constraint::Length(2),
    ])
    .split(inner);
    let mut auth_lines = vec![
        Line::from(Span::styled(
            "Open this URL in a browser and approve access:",
            Style::default().fg(SOFT_TEXT_COLOR),
        )),
        Line::default(),
    ];
    auth_lines.extend(
        wrap_text(authorization_url, rows[0].width.saturating_sub(2))
            .into_iter()
            .map(|line| {
                Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(ACCENT_COLOR),
                ))
            }),
    );
    frame.render_widget(
        Paragraph::new(auth_lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(BG_COLOR)),
        rows[0],
    );

    let input_block = Block::default()
        .title(Span::styled(
            " Complete callback URL ",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_BRIGHT_COLOR));
    let input_inner = input_block.inner(rows[1]);
    let (visible, cursor_x) = input_window(callback, input_inner.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            visible,
            Style::default().fg(TEXT_COLOR),
        )))
        .block(input_block),
        rows[1],
    );
    if input_inner.width > 0 && input_inner.height > 0 {
        frame.set_cursor_position(Position::new(
            input_inner.x + cursor_x.min(input_inner.width.saturating_sub(1)),
            input_inner.y,
        ));
    }

    let notice = view.notice.as_ref().map_or_else(
        || {
            Line::from(Span::styled(
                "Paste the complete redirected URL; credentials are stored privately.",
                Style::default().fg(MUTED_TEXT_COLOR),
            ))
        },
        |notice| {
            Line::from(Span::styled(
                format!("× {}", notice.message),
                Style::default().fg(ERROR_COLOR),
            ))
        },
    );
    frame.render_widget(Paragraph::new(notice), rows[2]);
}

fn render_logout_confirmation(frame: &mut Frame, server: &str, area: Rect) {
    let width = area.width.saturating_sub(4).clamp(1, 68);
    let height = area.height.clamp(1, 8);
    let panel = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, panel);
    let block = panel_block("Log out of OAuth", true);
    frame.render_widget(
        Paragraph::new(vec![
            Line::default(),
            Line::from(vec![
                Span::styled("  Server  ", Style::default().fg(MUTED_TEXT_COLOR)),
                Span::styled(
                    server.to_owned(),
                    Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::default(),
            Line::from(Span::styled(
                "  Stored OAuth credentials will be deleted.",
                Style::default().fg(WARNING_COLOR),
            )),
        ])
        .block(block)
        .style(Style::default().bg(BG_COLOR)),
        panel,
    );
}

fn render_footer(frame: &mut Frame, view: &McpView, area: Rect) {
    let hints = match &view.screen {
        McpScreen::Browse if view.selected == 0 => key_help(&[
            (
                "↑/↓",
                if view.focus == McpFocus::Servers {
                    "select"
                } else {
                    "scroll"
                },
            ),
            ("Tab/←/→", "focus"),
            ("Enter", "add server"),
            ("Esc", "close"),
        ]),
        McpScreen::Browse => key_help(&[
            (
                "↑/↓",
                if view.focus == McpFocus::Servers {
                    "select"
                } else {
                    "scroll"
                },
            ),
            ("Tab/←/→", "focus"),
            ("Enter", "full details"),
            ("R", "reconnect"),
            ("A", "authorize"),
            ("L", "logout"),
            ("Esc", "close"),
        ]),
        McpScreen::Details => key_help(&[
            ("↑/↓", "scroll"),
            ("R", "reconnect"),
            ("A", "authorize"),
            ("L", "logout"),
            ("Enter/Esc", "back"),
        ]),
        McpScreen::Add(_) => key_help(&[
            ("↑/↓/Tab", "field"),
            ("←/→", "edit / choose"),
            ("Enter", "add server"),
            ("Esc", "cancel"),
        ]),
        McpScreen::OAuth { .. } => {
            key_help(&[("Enter", "complete authorization"), ("Esc", "cancel")])
        }
        McpScreen::ConfirmLogout { .. } => {
            key_help(&[("Y/Enter", "delete credentials"), ("N/Esc", "cancel")])
        }
    };
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(BORDER_COLOR));
    frame.render_widget(
        Paragraph::new(hints)
            .block(block)
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn panel_block(title: &str, emphasized: bool) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if emphasized {
                    ACCENT_COLOR
                } else {
                    BORDER_BRIGHT_COLOR
                })
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if emphasized {
            BORDER_BRIGHT_COLOR
        } else {
            BORDER_COLOR
        }))
        .style(Style::default().bg(BG_COLOR))
}

fn section_heading(
    icon: &str,
    title: &str,
    count: Option<usize>,
    color: Color,
    width: usize,
) -> Line<'static> {
    let suffix = count.map_or_else(String::new, |count| format!("  {count}"));
    let label = format!("{icon} {title}{suffix} ");
    let rule = "─".repeat(width.saturating_sub(label.width()));
    Line::from(vec![
        Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(rule, Style::default().fg(BORDER_COLOR)),
    ])
}

fn field_text(label: &str, value: &str) -> Line<'static> {
    field_line(
        label,
        vec![Span::styled(
            value.to_owned(),
            Style::default().fg(SOFT_TEXT_COLOR),
        )],
    )
}

fn field_line(label: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("  {label:<14}"),
        Style::default().fg(MUTED_TEXT_COLOR),
    )];
    spans.append(&mut value);
    Line::from(spans)
}

fn dim_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_owned(),
        Style::default().fg(MUTED_TEXT_COLOR),
    ))
}

fn indented_wrapped(text: &str, width: usize, color: Color) -> Vec<Line<'static>> {
    wrap_text(text, width.saturating_sub(4).max(1) as u16)
        .into_iter()
        .map(|line| {
            Line::from(Span::styled(
                format!("    {line}"),
                Style::default().fg(color),
            ))
        })
        .collect()
}

fn bounded_indented(
    text: &str,
    width: usize,
    color: Color,
    max_lines: usize,
) -> Vec<Line<'static>> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let content_width = width.saturating_sub(4).max(1);
    let mut wrapped = wrap_text(&normalized, content_width as u16);
    let truncated = wrapped.len() > max_lines;
    wrapped.truncate(max_lines);
    if truncated && let Some(last) = wrapped.last_mut() {
        let available = content_width.saturating_sub(1);
        *last = format!("{}…", truncate_end_to_width(last, available));
    }
    wrapped
        .into_iter()
        .map(|line| {
            Line::from(Span::styled(
                format!("    {line}"),
                Style::default().fg(color),
            ))
        })
        .collect()
}

fn state_style(state: McpConnectionState) -> (&'static str, &'static str, Color) {
    match state {
        McpConnectionState::Starting => ("◐", "Starting", STARTING_COLOR),
        McpConnectionState::Ready => ("●", "Ready", READY_COLOR),
        McpConnectionState::Failed => ("×", "Failed", ERROR_COLOR),
        McpConnectionState::Stopped => ("○", "Stopped", STOPPED_COLOR),
    }
}

fn approval_label(approval: McpApprovalPolicy) -> &'static str {
    match approval {
        McpApprovalPolicy::Allow => "allow",
        McpApprovalPolicy::Deny => "deny",
        McpApprovalPolicy::Prompt => "prompt",
    }
}

fn approval_color(approval: McpApprovalPolicy) -> Color {
    match approval {
        McpApprovalPolicy::Allow => READY_COLOR,
        McpApprovalPolicy::Deny => ERROR_COLOR,
        McpApprovalPolicy::Prompt => WARNING_COLOR,
    }
}

fn server_origin(app: &App, name: &str) -> String {
    if app.config.base_mcp.servers.contains_key(name) {
        "config.yaml".to_owned()
    } else if let Some((plugin, _)) = name.split_once(':') {
        format!("plugin {plugin}")
    } else {
        "plugin contribution".to_owned()
    }
}

fn redacted_url(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return "<invalid URL>".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    if url.query().is_some() {
        url.set_query(Some("<redacted>"));
    }
    url.to_string()
}

fn input_window(input: &InputState, width: usize) -> (String, u16) {
    if width == 0 {
        return (String::new(), 0);
    }
    let before = &input.value[..input.cursor];
    if before.width() < width {
        return (
            truncate_end_to_width(&input.value, width),
            before.width().min(width.saturating_sub(1)) as u16,
        );
    }

    let mut start = input.cursor;
    let mut used = 0;
    for (index, char) in before.char_indices().rev() {
        let char_width = char.width().unwrap_or(0);
        if used + char_width >= width {
            break;
        }
        start = index;
        used += char_width;
    }
    (
        truncate_end_to_width(&input.value[start..], width),
        used.min(width.saturating_sub(1)) as u16,
    )
}

fn list_window_start(selected: usize, len: usize, height: usize) -> usize {
    if height == 0 || len <= height {
        0
    } else {
        selected.saturating_sub(height - 1).min(len - height)
    }
}

fn scroll_value(scroll: usize) -> u16 {
    u16::try_from(scroll).unwrap_or(u16::MAX)
}

fn inset_area(area: Rect, vertical: u16, horizontal: u16) -> Rect {
    Rect::new(
        area.x + horizontal.min(area.width),
        area.y + vertical.min(area.height),
        area.width.saturating_sub(horizontal.saturating_mul(2)),
        area.height.saturating_sub(vertical.saturating_mul(2)),
    )
}

fn key_help(items: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (index, (key, action)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(BORDER_COLOR)));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::default()
                .fg(KEY_HINT_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::default().fg(MUTED_TEXT_COLOR),
        ));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::{
        app::{McpNotice, McpScreen},
        services::mcp::{McpConfig, McpServerConfig, McpToolStatus},
    };

    fn disabled_stdio_config() -> McpConfig {
        McpConfig {
            servers: BTreeMap::from([(
                "docs".to_owned(),
                McpServerConfig {
                    enabled: false,
                    startup_timeout_ms: 20_000,
                    tool_timeout_ms: 60_000,
                    approval: McpApprovalPolicy::Prompt,
                    tool_approval: BTreeMap::new(),
                    enabled_tools: None,
                    disabled_tools: Vec::new(),
                    transport: McpTransportConfig::Stdio {
                        command: "docs-server".to_owned(),
                        args: vec!["--stdio".to_owned()],
                        env: BTreeMap::from([(
                            "PRIVATE_TOKEN".to_owned(),
                            "must-not-render".to_owned(),
                        )]),
                        env_vars: vec!["PUBLIC_TOKEN_NAME".to_owned()],
                        cwd: Some("/workspace/services/docs".to_owned()),
                    },
                },
            )]),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn empty_manager_still_offers_add_server_row() {
        let app = App::test_empty();
        let view = McpView {
            selected: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
            focus: McpFocus::Servers,
            screen: McpScreen::Browse,
            notice: None,
        };
        let backend = TestBackend::new(100, 26);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render_mcp_view(frame, &app, &view))
            .expect("render MCP view");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Add MCP server"));
        assert!(rendered.contains("Press Enter to configure"));
    }

    #[test]
    fn add_form_shows_only_fields_for_selected_transport() {
        let app = App::test_empty();
        let mut form = McpAddForm::default();
        form.transport = McpAddTransport::OAuth;
        form.focus = 2;
        form.name.set("remote");
        form.url.set("https://example.test/mcp");
        let view = McpView {
            selected: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
            focus: McpFocus::Servers,
            screen: McpScreen::Add(Box::new(form)),
            notice: None,
        };
        let backend = TestBackend::new(110, 28);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render_mcp_view(frame, &app, &view))
            .expect("render MCP add form");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Add MCP server"));
        assert!(rendered.contains("Streamable HTTP + OAuth"));
        assert!(rendered.contains("Redirect URI"));
        assert!(rendered.contains("OAuth scopes"));
        assert!(!rendered.contains("Bearer token env"));
        assert!(!rendered.contains("Working directory"));
    }

    #[test]
    fn manager_render_separates_list_and_connection_details() {
        let mut app = App::test_empty();
        app.reload_mcp_for_test(disabled_stdio_config());
        let view = McpView {
            selected: 1,
            detail_scroll: 0,
            detail_max_scroll: 0,
            focus: McpFocus::Servers,
            screen: McpScreen::Browse,
            notice: None,
        };
        let backend = TestBackend::new(120, 34);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render_mcp_view(frame, &app, &view))
            .expect("render MCP view");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("MCP servers"));
        assert!(rendered.contains("Servers"));
        assert!(rendered.contains("Server · docs"));
        assert!(rendered.contains("Connection"));
        assert!(rendered.contains("config.yaml"));
        assert!(rendered.contains("PRIVATE_TOKEN"));
        assert!(!rendered.contains("must-not-render"));
    }

    #[test]
    fn details_list_discovered_capabilities_one_item_per_line() {
        let mut app = App::test_empty();
        app.config.mcp = disabled_stdio_config();
        app.config.base_mcp = app.config.mcp.clone();
        let status = McpServerStatus {
            name: "docs".to_owned(),
            state: McpConnectionState::Ready,
            tools: vec![
                McpToolStatus {
                    name: "mcp__docs__search".to_owned(),
                    description: "Search documentation".to_owned(),
                    approval: McpApprovalPolicy::Allow,
                    read_only: true,
                    timeout_ms: 60_000,
                },
                McpToolStatus {
                    name: "mcp__docs__publish".to_owned(),
                    description: "Publish documentation".to_owned(),
                    approval: McpApprovalPolicy::Prompt,
                    read_only: false,
                    timeout_ms: 60_000,
                },
            ],
            resources: vec![McpCapabilityStatus {
                name: "guide".to_owned(),
                detail: Some("docs://guide".to_owned()),
                description: Some("Project guide".to_owned()),
            }],
            resource_templates: vec![McpCapabilityStatus {
                name: "topic".to_owned(),
                detail: Some("docs://topics/{name}".to_owned()),
                description: None,
            }],
            prompts: vec![
                McpCapabilityStatus {
                    name: "summarize".to_owned(),
                    detail: Some("document*".to_owned()),
                    description: None,
                },
                McpCapabilityStatus {
                    name: "review".to_owned(),
                    detail: Some("document*, focus".to_owned()),
                    description: None,
                },
            ],
            error: None,
        };

        let rendered = server_detail_lines(
            &app,
            &status,
            100,
            Some(&McpNotice {
                message: "Refreshed capabilities.".to_owned(),
                failed: false,
            }),
            McpDetailMode::Full,
        )
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

        let search_line = rendered
            .iter()
            .position(|line| line.contains("mcp__docs__search"))
            .unwrap();
        let publish_line = rendered
            .iter()
            .position(|line| line.contains("mcp__docs__publish"))
            .unwrap();
        let summarize_line = rendered
            .iter()
            .position(|line| line.contains("summarize"))
            .unwrap();
        let review_line = rendered
            .iter()
            .position(|line| line.contains("review"))
            .unwrap();
        assert_ne!(search_line, publish_line);
        assert_ne!(summarize_line, review_line);
        assert!(rendered.iter().any(|line| line.contains("docs://guide")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("docs://topics/{name}"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Refreshed capabilities"))
        );
    }

    #[test]
    fn overview_lists_tool_names_without_description_wall() {
        let app = App::test_empty();
        let status = McpServerStatus {
            name: "filesystem".to_owned(),
            state: McpConnectionState::Ready,
            tools: vec![McpToolStatus {
                name: "mcp__filesystem__read_text_file".to_owned(),
                description: "A very long description that should stay out of the compact overview panel even though the registered tool remains visible.".to_owned(),
                approval: McpApprovalPolicy::Prompt,
                read_only: true,
                timeout_ms: 60_000,
            }],
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
            error: None,
        };

        let overview = server_detail_lines(&app, &status, 60, None, McpDetailMode::Overview)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert!(overview.iter().any(|line| line.contains("mcp__filesystem")));
        assert!(
            !overview
                .iter()
                .any(|line| line.contains("very long description"))
        );
    }

    #[test]
    fn full_details_bound_each_tool_description() {
        let description = (0..100)
            .map(|index| format!("description-{index}"))
            .collect::<Vec<_>>()
            .join(" ");

        let lines = bounded_indented(&description, 36, MUTED_TEXT_COLOR, 3)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert_eq!(lines.len(), 3);
        assert!(lines.last().unwrap().ends_with('…'));
        assert!(lines.iter().all(|line| line.width() <= 36));
    }

    #[test]
    fn detail_scroll_limit_stops_at_last_visible_row() {
        let lines = (0..10)
            .map(|index| Line::from(format!("line {index}")))
            .collect();

        assert_eq!(detail_scroll_limit(lines, 20, 4), 6);
        assert_eq!(
            detail_scroll_limit(vec![Line::from("123456 123456 123456")], 10, 1),
            2
        );
    }

    #[test]
    fn mouse_targets_server_rows_and_detail_scrolling() {
        let mut app = App::test_empty();
        app.reload_mcp_for_test(disabled_stdio_config());
        app.mcp_view = Some(McpView {
            selected: 1,
            detail_scroll: 0,
            detail_max_scroll: 10,
            focus: McpFocus::Servers,
            screen: McpScreen::Browse,
            notice: None,
        });

        assert_eq!(
            mouse_action(&app, MouseAction::LeftDown { column: 2, row: 6 }, 120, 30,),
            McpMouseAction::SelectServer(1)
        );
        assert_eq!(
            mouse_action(&app, MouseAction::ScrollDown { column: 2, row: 6 }, 120, 30,),
            McpMouseAction::MoveServerSelection(1)
        );
        assert_eq!(
            mouse_action(
                &app,
                MouseAction::ScrollDown { column: 70, row: 6 },
                120,
                30,
            ),
            McpMouseAction::ScrollDetails(3)
        );
        assert_eq!(
            mouse_action(&app, MouseAction::LeftDown { column: 70, row: 6 }, 120, 30,),
            McpMouseAction::OpenSelected
        );
    }

    #[test]
    fn connection_urls_hide_credentials_and_query_values() {
        let rendered = redacted_url("https://user:secret@example.test/mcp?token=hidden&mode=full");

        assert!(rendered.contains("example.test/mcp"));
        assert!(rendered.contains("%3Credacted%3E"));
        assert!(!rendered.contains("user"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("hidden"));
    }

    #[test]
    fn server_names_stay_aligned_for_every_status_icon() {
        let status = |name: &str, state| McpServerStatus {
            name: name.to_owned(),
            state,
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
            error: None,
        };
        let ready = line_text(&server_line(
            &status("ready", McpConnectionState::Ready),
            true,
            40,
        ));
        let failed = line_text(&server_line(
            &status("failed", McpConnectionState::Failed),
            false,
            40,
        ));
        let stopped = line_text(&server_line(
            &status("stopped", McpConnectionState::Stopped),
            false,
            40,
        ));

        let name_column = |line: &str, name: &str| {
            line.find(name)
                .map(|index| line[..index].width())
                .expect("server name")
        };
        assert_eq!(name_column(&ready, "ready"), name_column(&failed, "failed"));
        assert_eq!(
            name_column(&ready, "ready"),
            name_column(&stopped, "stopped")
        );
    }
}
