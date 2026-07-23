use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, PluginOperationView, PluginsScreen, PluginsTab, PluginsView},
    event::{MouseAction, PluginsMouseAction, PluginsMouseTab},
    plugins::{
        ConfiguredMarketplace, InstalledPluginStatus, MarketplacePlugin, PluginContributions,
    },
};

use super::{
    layout::{truncate_end_to_width, wrap_text},
    theme::*,
};

const ENABLED_COLOR: Color = Color::Rgb(74, 222, 128);
const WARNING_COLOR: Color = Color::Rgb(251, 191, 36);
const ERROR_COLOR: Color = Color::Rgb(248, 113, 113);
const SELECTED_BG_COLOR: Color = Color::Rgb(15, 23, 42);

pub(super) fn render_plugins_view(frame: &mut Frame, app: &App, view: &PluginsView) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG_COLOR)),
        frame.area(),
    );

    let areas = plugins_view_areas(frame.area());

    render_header(frame, app, view, areas[0]);
    match &view.screen {
        PluginsScreen::Browse => render_browse(frame, app, view, areas[1]),
        PluginsScreen::InstalledDetail(index) => {
            render_installed_detail(frame, app, view, *index, areas[1])
        }
        PluginsScreen::MarketplacePluginDetail(index) => {
            render_marketplace_plugin_detail(frame, app, view, *index, areas[1])
        }
        PluginsScreen::AddMarketplace(input) => {
            render_add_marketplace(frame, input, areas[1]);
        }
        PluginsScreen::Operation(operation) => {
            render_operation(frame, operation, areas[1]);
        }
    }
    render_footer(frame, app, view, areas[2]);
}

fn plugins_view_areas(area: Rect) -> [Rect; 3] {
    let areas = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);
    [areas[0], areas[1], areas[2]]
}

pub(super) fn detail_max_scroll(app: &App, width: u16, height: u16) -> usize {
    let Some(view) = app.plugins_view.as_ref() else {
        return 0;
    };
    let body = plugins_view_areas(Rect::new(0, 0, width, height))[1];
    let (lines, inner, wrap) = match &view.screen {
        PluginsScreen::Browse => {
            let detail_panel = browse_panel_areas(body)[1];
            let block = panel_block(
                match view.tab {
                    PluginsTab::Installed => "◇ Plugin details",
                    PluginsTab::Marketplaces => "◇ Selection details",
                },
                false,
            );
            let inner = block.inner(detail_panel);
            let lines = match view.tab {
                PluginsTab::Installed => app
                    .config
                    .extensions
                    .installed_plugins
                    .get(view.selected_installed)
                    .map(|plugin| installed_overview_lines(plugin, inner.width as usize, true))
                    .unwrap_or_else(|| {
                        empty_lines(
                            "Nothing selected",
                            "Choose an installed plugin to inspect it.",
                        )
                    }),
                PluginsTab::Marketplaces => {
                    let rows = marketplace_rows(app);
                    match rows.get(view.selected_marketplace) {
                        Some(MarketplaceRow::Add) => {
                            add_marketplace_summary_lines(inner.width as usize)
                        }
                        Some(MarketplaceRow::Marketplace(marketplace)) => {
                            marketplace_overview_lines(app, marketplace, inner.width as usize)
                        }
                        Some(MarketplaceRow::Plugin(plugin)) => {
                            marketplace_plugin_lines(plugin, inner.width as usize)
                        }
                        None => empty_lines(
                            "Nothing selected",
                            "Choose a marketplace item to inspect it.",
                        ),
                    }
                }
            };
            (lines, inner, true)
        }
        PluginsScreen::InstalledDetail(index) => {
            let panel = installed_detail_panel_areas(body)[1];
            let block = panel_block("✦ Registered content", true);
            let inner = block.inner(panel);
            let Some(plugin) = app.config.extensions.installed_plugins.get(*index) else {
                return 0;
            };
            (
                contribution_detail_lines(&plugin.contributions, inner.width as usize),
                inner,
                false,
            )
        }
        PluginsScreen::MarketplacePluginDetail(index) => {
            let panel = marketplace_plugin_detail_panel(body);
            let block = panel_block("◇ Marketplace plugin", true);
            let inner = block.inner(panel);
            let Some(plugin) = app.config.extensions.marketplace_plugins.get(*index) else {
                return 0;
            };
            (
                marketplace_plugin_lines(plugin, inner.width as usize),
                inner,
                true,
            )
        }
        _ => return 0,
    };
    detail_scroll_limit(lines, inner.width, inner.height, wrap)
}

pub(super) fn mouse_action(
    app: &App,
    mouse: MouseAction,
    width: u16,
    height: u16,
) -> PluginsMouseAction {
    let Some(view) = app.plugins_view.as_ref() else {
        return PluginsMouseAction::None;
    };
    let areas = plugins_view_areas(Rect::new(0, 0, width, height));

    if matches!(
        view.screen,
        PluginsScreen::Browse
            | PluginsScreen::InstalledDetail(_)
            | PluginsScreen::MarketplacePluginDetail(_)
    ) && let MouseAction::LeftDown { column, row } = mouse
    {
        let position = Position::new(column, row);
        let [(installed_tab, installed), (marketplaces_tab, marketplaces)] =
            plugins_tab_hitboxes(areas[0]);
        if installed.contains(position) {
            return PluginsMouseAction::SelectTab(installed_tab);
        }
        if marketplaces.contains(position) {
            return PluginsMouseAction::SelectTab(marketplaces_tab);
        }
    }

    match &view.screen {
        PluginsScreen::Browse => browse_mouse_action(app, view, mouse, areas[1]),
        PluginsScreen::InstalledDetail(_) => {
            let details = installed_detail_panel_areas(areas[1])[1];
            detail_mouse_action(mouse, details)
        }
        PluginsScreen::MarketplacePluginDetail(_) => {
            detail_mouse_action(mouse, marketplace_plugin_detail_panel(areas[1]))
        }
        _ => PluginsMouseAction::None,
    }
}

fn browse_mouse_action(
    app: &App,
    view: &PluginsView,
    mouse: MouseAction,
    area: Rect,
) -> PluginsMouseAction {
    let panels = browse_panel_areas(area);
    let list_inner = panel_block("", true).inner(panels[0]);
    let (row_count, start) = match view.tab {
        PluginsTab::Installed => {
            let count = app.config.extensions.installed_plugins.len();
            let selected = view.selected_installed.min(count.saturating_sub(1));
            (
                count,
                list_window_start(selected, count, list_inner.height as usize),
            )
        }
        PluginsTab::Marketplaces => {
            let count = marketplace_rows(app).len();
            let selected = view.selected_marketplace.min(count.saturating_sub(1));
            (
                count,
                list_window_start(selected, count, list_inner.height as usize),
            )
        }
    };

    match mouse {
        MouseAction::LeftDown { column, row }
            if list_inner.contains(Position::new(column, row)) =>
        {
            let selected = start + usize::from(row.saturating_sub(list_inner.y));
            if selected < row_count {
                PluginsMouseAction::SelectItem(selected)
            } else {
                PluginsMouseAction::None
            }
        }
        MouseAction::LeftDown { column, row } if panels[1].contains(Position::new(column, row)) => {
            PluginsMouseAction::OpenSelected
        }
        MouseAction::ScrollUp { column, row } if panels[0].contains(Position::new(column, row)) => {
            PluginsMouseAction::MoveSelection(-1)
        }
        MouseAction::ScrollDown { column, row }
            if panels[0].contains(Position::new(column, row)) =>
        {
            PluginsMouseAction::MoveSelection(1)
        }
        _ => detail_mouse_action(mouse, panels[1]),
    }
}

fn detail_mouse_action(mouse: MouseAction, area: Rect) -> PluginsMouseAction {
    match mouse {
        MouseAction::ScrollUp { column, row } if area.contains(Position::new(column, row)) => {
            PluginsMouseAction::ScrollDetails(-3)
        }
        MouseAction::ScrollDown { column, row } if area.contains(Position::new(column, row)) => {
            PluginsMouseAction::ScrollDetails(3)
        }
        _ => PluginsMouseAction::None,
    }
}

fn plugins_tab_hitboxes(area: Rect) -> [(PluginsMouseTab, Rect); 2] {
    let installed_width = "● Installed".width() as u16;
    let marketplace_width = "● Marketplaces".width() as u16;
    let installed_x = area.x.saturating_add(1);
    let marketplace_x = installed_x
        .saturating_add(installed_width)
        .saturating_add("  │  ".width() as u16);
    let row = area.y.saturating_add(2);
    [
        (
            PluginsMouseTab::Installed,
            Rect::new(installed_x, row, installed_width, 1),
        ),
        (
            PluginsMouseTab::Marketplaces,
            Rect::new(marketplace_x, row, marketplace_width, 1),
        ),
    ]
}

fn detail_scroll_limit(lines: Vec<Line<'static>>, width: u16, height: u16, wrap: bool) -> usize {
    let paragraph = Paragraph::new(lines);
    let line_count = if wrap {
        paragraph.wrap(Wrap { trim: false }).line_count(width)
    } else {
        paragraph.line_count(width)
    };
    line_count.saturating_sub(height as usize)
}

fn render_header(frame: &mut Frame, app: &App, view: &PluginsView, area: Rect) {
    let installed_count = app.config.extensions.installed_plugins.len();
    let marketplace_count = app.config.extensions.marketplaces.len();
    let title = Line::from(vec![
        Span::styled(
            " Plugins ",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{} installed  ·  {} marketplace{}",
                installed_count,
                marketplace_count,
                plural(marketplace_count)
            ),
            Style::default().fg(MUTED_TEXT_COLOR),
        ),
    ]);
    let description = match &view.screen {
        PluginsScreen::Browse => "Manage installed plugins and plugin marketplaces",
        PluginsScreen::InstalledDetail(_) => "Inspect registered plugin content",
        PluginsScreen::MarketplacePluginDetail(_) => {
            "Inspect a marketplace plugin before changing it"
        }
        PluginsScreen::AddMarketplace(_) => {
            "Connect a Git repository, local catalog, or remote catalog"
        }
        PluginsScreen::Operation(_) => "Plugin work runs in the background and streams here",
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
            plugins_tab_line(view.tab),
        ])
        .block(block)
        .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn plugins_tab_line(selected: PluginsTab) -> Line<'static> {
    let tabs = [
        (PluginsTab::Installed, "Installed"),
        (PluginsTab::Marketplaces, "Marketplaces"),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (index, (tab, label)) in tabs.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(BORDER_COLOR)));
        }
        let (icon, style) = if tab == selected {
            (
                "●",
                Style::default()
                    .fg(ACCENT_COLOR)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("○", Style::default().fg(MUTED_TEXT_COLOR))
        };
        spans.push(Span::styled(format!("{icon} {label}"), style));
    }
    Line::from(spans)
}

fn render_browse(frame: &mut Frame, app: &App, view: &PluginsView, area: Rect) {
    let panels = browse_panel_areas(area);

    match view.tab {
        PluginsTab::Installed => {
            render_installed_list(frame, app, view.selected_installed, panels[0]);
            render_selected_installed(frame, app, view, panels[1]);
        }
        PluginsTab::Marketplaces => {
            render_marketplace_list(frame, app, view.selected_marketplace, panels[0]);
            render_selected_marketplace(frame, app, view, panels[1]);
        }
    }
}

fn browse_panel_areas(area: Rect) -> [Rect; 2] {
    let direction = browse_panel_direction(area.width, area.height);
    let constraints = match direction {
        Direction::Horizontal => [Constraint::Percentage(42), Constraint::Percentage(58)],
        Direction::Vertical => [Constraint::Percentage(48), Constraint::Percentage(52)],
    };
    let panels = Layout::default()
        .direction(direction)
        .constraints(constraints)
        .spacing(1)
        .split(area);
    [panels[0], panels[1]]
}

fn browse_panel_direction(width: u16, height: u16) -> Direction {
    if width >= 88 && height >= 12 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    }
}

fn installed_detail_panel_areas(area: Rect) -> [Rect; 2] {
    let direction = browse_panel_direction(area.width, area.height);
    let constraints = match direction {
        Direction::Horizontal => [Constraint::Percentage(42), Constraint::Percentage(58)],
        Direction::Vertical => [Constraint::Percentage(45), Constraint::Percentage(55)],
    };
    let panels = Layout::default()
        .direction(direction)
        .constraints(constraints)
        .spacing(1)
        .split(area);
    [panels[0], panels[1]]
}

fn marketplace_plugin_detail_panel(area: Rect) -> Rect {
    inset_area(area, 2, if area.width >= 100 { 8 } else { 1 })
}

fn render_installed_list(frame: &mut Frame, app: &App, selected: usize, area: Rect) {
    let block = panel_block("● Installed plugins", true);
    let inner = block.inner(area);
    let plugins = &app.config.extensions.installed_plugins;
    let lines = if plugins.is_empty() {
        empty_lines(
            "No plugins installed",
            "Open Marketplaces to find and install one.",
        )
    } else {
        let selected = selected.min(plugins.len() - 1);
        let start = list_window_start(selected, plugins.len(), inner.height as usize);
        plugins
            .iter()
            .enumerate()
            .skip(start)
            .take(inner.height as usize)
            .map(|(index, plugin)| {
                installed_plugin_line(plugin, index == selected, inner.width as usize)
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn installed_plugin_line(
    plugin: &InstalledPluginStatus,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let (icon, state) = if plugin.config_managed {
        ("◆", "Config")
    } else if plugin.enabled {
        ("●", "Enabled")
    } else {
        ("○", "Disabled")
    };
    let marker = if selected { "› " } else { "  " };
    let text = format!("{} {}  {}", plugin.name, plugin.version, state);
    styled_selection_line(
        marker,
        icon,
        panel_status_color(plugin),
        &text,
        selected,
        width,
    )
}

fn render_selected_installed(frame: &mut Frame, app: &App, view: &PluginsView, area: Rect) {
    let block = panel_block("◇ Plugin details", false);
    let inner = block.inner(area);
    let lines = app
        .config
        .extensions
        .installed_plugins
        .get(view.selected_installed)
        .map(|plugin| installed_overview_lines(plugin, inner.width as usize, true))
        .unwrap_or_else(|| {
            empty_lines(
                "Nothing selected",
                "Choose an installed plugin to inspect it.",
            )
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

enum MarketplaceRow<'a> {
    Add,
    Marketplace(&'a ConfiguredMarketplace),
    Plugin(&'a MarketplacePlugin),
}

fn marketplace_rows(app: &App) -> Vec<MarketplaceRow<'_>> {
    let mut rows = vec![MarketplaceRow::Add];
    for marketplace in &app.config.extensions.marketplaces {
        rows.push(MarketplaceRow::Marketplace(marketplace));
        rows.extend(
            app.config
                .extensions
                .marketplace_plugins
                .iter()
                .filter(|plugin| plugin.marketplace == marketplace.name)
                .map(MarketplaceRow::Plugin),
        );
    }
    rows
}

fn render_marketplace_list(frame: &mut Frame, app: &App, selected: usize, area: Rect) {
    let block = panel_block("▣ Marketplaces", true);
    let inner = block.inner(area);
    let rows = marketplace_rows(app);
    let selected = selected.min(rows.len().saturating_sub(1));
    let start = list_window_start(selected, rows.len(), inner.height as usize);
    let lines = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(inner.height as usize)
        .map(|(index, row)| marketplace_row_line(app, row, index == selected, inner.width as usize))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn marketplace_row_line(
    app: &App,
    row: &MarketplaceRow<'_>,
    selected: bool,
    width: usize,
) -> Line<'static> {
    match row {
        MarketplaceRow::Add => styled_selection_line(
            if selected { "› " } else { "  " },
            "＋",
            ACCENT_COLOR,
            "Add marketplace",
            selected,
            width,
        ),
        MarketplaceRow::Marketplace(marketplace) => {
            let count = app
                .config
                .extensions
                .marketplace_plugins
                .iter()
                .filter(|plugin| plugin.marketplace == marketplace.name)
                .count();
            styled_selection_line(
                if selected { "› " } else { "  " },
                "▣",
                BORDER_BRIGHT_COLOR,
                &format!("{}  {count} plugin{}", marketplace.name, plural(count)),
                selected,
                width,
            )
        }
        MarketplaceRow::Plugin(plugin) => {
            if plugin.installed {
                styled_selection_line(
                    if selected { "›   " } else { "    " },
                    "✓",
                    ENABLED_COLOR,
                    &plugin.name,
                    selected,
                    width,
                )
            } else {
                styled_selection_line(
                    if selected { "›   " } else { "    " },
                    " ",
                    MUTED_TEXT_COLOR,
                    &plugin.name,
                    selected,
                    width,
                )
            }
        }
    }
}

fn render_selected_marketplace(frame: &mut Frame, app: &App, view: &PluginsView, area: Rect) {
    let block = panel_block("◇ Selection details", false);
    let inner = block.inner(area);
    let rows = marketplace_rows(app);
    let lines = match rows.get(view.selected_marketplace) {
        Some(MarketplaceRow::Add) => add_marketplace_summary_lines(inner.width as usize),
        Some(MarketplaceRow::Marketplace(marketplace)) => {
            marketplace_overview_lines(app, marketplace, inner.width as usize)
        }
        Some(MarketplaceRow::Plugin(plugin)) => {
            marketplace_plugin_lines(plugin, inner.width as usize)
        }
        None => empty_lines(
            "Nothing selected",
            "Choose a marketplace item to inspect it.",
        ),
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll_value(view.detail_scroll), 0))
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn render_installed_detail(
    frame: &mut Frame,
    app: &App,
    view: &PluginsView,
    index: usize,
    area: Rect,
) {
    let Some(plugin) = app.config.extensions.installed_plugins.get(index) else {
        render_missing(frame, "Installed plugin is no longer available.", area);
        return;
    };
    let panels = installed_detail_panel_areas(area);

    let overview_block = panel_block("◇ Overview", false);
    let overview_inner = overview_block.inner(panels[0]);
    frame.render_widget(
        Paragraph::new(installed_overview_lines(
            plugin,
            overview_inner.width as usize,
            false,
        ))
        .block(overview_block)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(BG_COLOR)),
        panels[0],
    );

    let contributions_block = panel_block("✦ Registered content", true);
    let contributions_inner = contributions_block.inner(panels[1]);
    frame.render_widget(
        Paragraph::new(contribution_detail_lines(
            &plugin.contributions,
            contributions_inner.width as usize,
        ))
        .block(contributions_block)
        .scroll((scroll_value(view.detail_scroll), 0))
        .style(Style::default().bg(BG_COLOR)),
        panels[1],
    );
}

fn render_marketplace_plugin_detail(
    frame: &mut Frame,
    app: &App,
    view: &PluginsView,
    index: usize,
    area: Rect,
) {
    let Some(plugin) = app.config.extensions.marketplace_plugins.get(index) else {
        render_missing(frame, "Marketplace plugin is no longer available.", area);
        return;
    };
    let panel = marketplace_plugin_detail_panel(area);
    let block = panel_block("◇ Marketplace plugin", true);
    let inner = block.inner(panel);
    frame.render_widget(
        Paragraph::new(marketplace_plugin_lines(plugin, inner.width as usize))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll_value(view.detail_scroll), 0))
            .style(Style::default().bg(BG_COLOR)),
        panel,
    );
}

fn render_add_marketplace(frame: &mut Frame, input: &crate::input::InputState, area: Rect) {
    let panel = inset_area(area, 1, if area.width >= 100 { 8 } else { 1 });
    let block = panel_block("＋ Add marketplace", true);
    let inner = block.inner(panel);
    frame.render_widget(block, panel);

    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Connect a marketplace source",
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Glint will validate the catalog before adding it.",
                Style::default().fg(MUTED_TEXT_COLOR),
            )),
        ]),
        rows[0],
    );

    let input_block = Block::default()
        .title(Span::styled(
            " Source ",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_BRIGHT_COLOR));
    let input_inner = input_block.inner(rows[1]);
    let visible = truncate_end_to_width(&input.value, input_inner.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            visible,
            Style::default().fg(TEXT_COLOR),
        )))
        .block(input_block),
        rows[1],
    );

    frame.render_widget(
        Paragraph::new(vec![
            section_divider("Accepted sources", rows[2].width as usize),
            example_line("GitHub", "anthropics/claude-code"),
            example_line("Git", "https://github.com/owner/repo.git"),
            example_line("Local", "./marketplace"),
            example_line("Catalog", "https://example.com/marketplace.json"),
        ]),
        rows[2],
    );

    if input_inner.width > 0 && input_inner.height > 0 {
        let cursor_width = input.value[..input.cursor].width() as u16;
        frame.set_cursor_position(Position::new(
            input_inner.x + cursor_width.min(input_inner.width.saturating_sub(1)),
            input_inner.y,
        ));
    }
}

fn render_operation(frame: &mut Frame, operation: &PluginOperationView, area: Rect) {
    let source_height = if operation.subject.is_empty() { 0 } else { 3 };
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(source_height),
        Constraint::Min(1),
    ])
    .split(area);

    let status_block = panel_block("● Status", true);
    let status_line = if operation.finished {
        if operation.failed {
            status_line("×", ERROR_COLOR, &operation.title, "Failed")
        } else {
            status_line("✓", ENABLED_COLOR, &operation.title, "Complete")
        }
    } else {
        status_line("●", ACCENT_COLOR, &operation.title, "Working")
    };
    frame.render_widget(
        Paragraph::new(status_line)
            .block(status_block)
            .style(Style::default().bg(BG_COLOR)),
        rows[0],
    );

    if source_height > 0 {
        let source_block = panel_block("◇ Source", false);
        let source_inner = source_block.inner(rows[1]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_end_to_width(&operation.subject, source_inner.width as usize),
                Style::default().fg(SOFT_TEXT_COLOR),
            )))
            .block(source_block)
            .style(Style::default().bg(BG_COLOR)),
            rows[1],
        );
    }

    let activity_block = panel_block(
        if operation.uses_git {
            "▤ Git activity"
        } else {
            "▤ Plugin activity"
        },
        false,
    );
    let activity_inner = activity_block.inner(rows[2]);
    let mut activity_lines = operation
        .log
        .iter()
        .flat_map(|entry| {
            wrap_text(entry, activity_inner.width.saturating_sub(3).max(1))
                .into_iter()
                .map(|row| {
                    Line::from(vec![
                        Span::styled("│ ", Style::default().fg(BORDER_COLOR)),
                        Span::styled(row, Style::default().fg(SOFT_TEXT_COLOR)),
                    ])
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if activity_lines.is_empty() {
        activity_lines.push(Line::from(Span::styled(
            "Waiting for activity…",
            Style::default().fg(MUTED_TEXT_COLOR),
        )));
    }
    let visible = activity_inner.height as usize;
    let start = activity_lines.len().saturating_sub(visible);
    frame.render_widget(
        Paragraph::new(activity_lines.into_iter().skip(start).collect::<Vec<_>>())
            .block(activity_block)
            .style(Style::default().bg(BG_COLOR)),
        rows[2],
    );
}

fn installed_overview_lines(
    plugin: &InstalledPluginStatus,
    width: usize,
    include_contributions: bool,
) -> Vec<Line<'static>> {
    let (status_icon, status) = if plugin.config_managed {
        ("◆", "Config managed")
    } else if plugin.enabled {
        ("●", "Enabled")
    } else {
        ("○", "Disabled")
    };
    let status_color = panel_status_color(plugin);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                plugin.name.clone(),
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", plugin.version),
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
        ]),
        Line::from(vec![
            Span::styled(format!("{status_icon} "), Style::default().fg(status_color)),
            Span::styled(status, Style::default().fg(status_color)),
        ]),
    ];
    if !plugin.description.is_empty() {
        lines.push(Line::from(""));
        lines.extend(
            wrap_text(&plugin.description, width.max(1) as u16)
                .into_iter()
                .map(|line| Line::from(Span::styled(line, Style::default().fg(SOFT_TEXT_COLOR)))),
        );
    }
    lines.push(Line::from(""));
    lines.push(section_divider("Source", width));
    lines.push(field_line("Provider", &plugin_source(plugin), width));
    if let Some(root) = &plugin.root {
        lines.extend(field_lines("Path", &root.display().to_string(), width));
    }
    if include_contributions {
        lines.push(Line::from(""));
        lines.push(section_divider("Registered content", width));
        lines.extend(contribution_detail_lines(&plugin.contributions, width));
    }
    lines
}

fn add_marketplace_summary_lines(width: usize) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled("＋ ", Style::default().fg(ACCENT_COLOR)),
            Span::styled(
                "Add marketplace",
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Connect another plugin catalog without leaving this screen.",
            Style::default().fg(SOFT_TEXT_COLOR),
        )),
        Line::from(""),
        section_divider("Supported", width),
        Line::from(Span::styled(
            "GitHub owner/repo  ·  Git URL  ·  local directory  ·  remote JSON catalog",
            Style::default().fg(MUTED_TEXT_COLOR),
        )),
        Line::from(""),
        action_line("Enter", "open source input"),
    ]
}

fn marketplace_overview_lines(
    app: &App,
    marketplace: &ConfiguredMarketplace,
    width: usize,
) -> Vec<Line<'static>> {
    let count = app
        .config
        .extensions
        .marketplace_plugins
        .iter()
        .filter(|plugin| plugin.marketplace == marketplace.name)
        .count();
    let mut lines = vec![
        Line::from(vec![
            Span::styled("▣ ", Style::default().fg(BORDER_BRIGHT_COLOR)),
            Span::styled(
                marketplace.name.clone(),
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            format!("{count} plugin{} available", plural(count)),
            Style::default().fg(MUTED_TEXT_COLOR),
        )),
        Line::from(""),
        section_divider("Marketplace", width),
        field_line("Alias", &marketplace.alias, width),
        field_line("Source", &marketplace.source, width),
    ];
    if let Some(root) = &marketplace.root {
        lines.extend(field_lines("Cache", &root.display().to_string(), width));
    }
    lines
}

fn marketplace_plugin_lines(plugin: &MarketplacePlugin, width: usize) -> Vec<Line<'static>> {
    let (icon, status, color) = if plugin.installed {
        ("✓", "Installed", ENABLED_COLOR)
    } else {
        ("↓", "Available", ACCENT_COLOR)
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                plugin.name.clone(),
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", plugin.version),
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
        ]),
        Line::from(vec![
            Span::styled(format!("{icon} "), Style::default().fg(color)),
            Span::styled(status, Style::default().fg(color)),
        ]),
    ];
    if !plugin.description.is_empty() {
        lines.push(Line::from(""));
        lines.extend(
            wrap_text(&plugin.description, width.max(1) as u16)
                .into_iter()
                .map(|line| Line::from(Span::styled(line, Style::default().fg(SOFT_TEXT_COLOR)))),
        );
    }
    lines.push(Line::from(""));
    lines.push(section_divider("Source", width));
    lines.push(field_line("Marketplace", &plugin.marketplace, width));
    lines.push(field_line("Plugin", &plugin.source.label(), width));
    lines.push(Line::from(""));
    lines.push(action_line(
        "Space",
        if plugin.installed {
            "uninstall this plugin"
        } else {
            "install this plugin"
        },
    ));
    lines
}

fn contribution_detail_lines(
    contributions: &PluginContributions,
    width: usize,
) -> Vec<Line<'static>> {
    if contributions.total() == 0 {
        return empty_lines(
            "No registered content",
            "This plugin does not register commands, skills, agents, hooks, MCP, LSP, or settings.",
        );
    }
    let mut lines = Vec::new();
    push_contribution(&mut lines, "⌘", "Commands", &contributions.commands, width);
    push_contribution(&mut lines, "✦", "Skills", &contributions.skills, width);
    push_contribution(&mut lines, "◈", "Agents", &contributions.agents, width);
    push_contribution(&mut lines, "↯", "Hooks", &contributions.hooks, width);
    push_contribution(
        &mut lines,
        "◇",
        "MCP servers",
        &contributions.mcp_servers,
        width,
    );
    push_contribution(
        &mut lines,
        "λ",
        "LSP servers",
        &contributions.lsp_servers,
        width,
    );
    if contributions.settings {
        lines.push(contribution_heading("⚙", "Settings", 1));
        lines.push(Line::from(Span::styled(
            "   registered",
            Style::default().fg(SOFT_TEXT_COLOR),
        )));
    }
    lines
}

fn push_contribution(
    lines: &mut Vec<Line<'static>>,
    icon: &str,
    label: &str,
    values: &[String],
    width: usize,
) {
    if values.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines.push(contribution_heading(icon, label, values.len()));
    for value in values {
        for (index, row) in wrap_text(value, width.saturating_sub(4).max(1) as u16)
            .into_iter()
            .enumerate()
        {
            lines.push(Line::from(vec![
                Span::styled(
                    if index == 0 { "  • " } else { "    " },
                    Style::default().fg(MUTED_TEXT_COLOR),
                ),
                Span::styled(row, Style::default().fg(SOFT_TEXT_COLOR)),
            ]));
        }
    }
}

fn contribution_heading(icon: &str, label: &str, count: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{icon} "), Style::default().fg(ACCENT_COLOR)),
        Span::styled(
            label.to_owned(),
            Style::default()
                .fg(BORDER_BRIGHT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {count}"), Style::default().fg(MUTED_TEXT_COLOR)),
    ])
}

fn render_footer(frame: &mut Frame, app: &App, view: &PluginsView, area: Rect) {
    let hints = match &view.screen {
        PluginsScreen::Browse => match view.tab {
            PluginsTab::Installed => key_help(&[
                ("↑/↓", "select"),
                ("Space", "enable / disable"),
                ("Enter", "details"),
                ("←/→", "tab"),
                ("Esc", "close"),
            ]),
            PluginsTab::Marketplaces => key_help(&[
                ("↑/↓", "select"),
                ("Space", "install / uninstall"),
                ("Enter", "details / add"),
                ("←/→", "tab"),
                ("Esc", "close"),
            ]),
        },
        PluginsScreen::InstalledDetail(index) => {
            let managed = app
                .config
                .extensions
                .installed_plugins
                .get(*index)
                .is_some_and(|plugin| plugin.config_managed);
            if managed {
                key_help(&[
                    ("config.yaml", "controls this plugin"),
                    ("Enter/Esc", "back"),
                ])
            } else {
                key_help(&[("Space", "enable / disable"), ("Enter/Esc", "back")])
            }
        }
        PluginsScreen::MarketplacePluginDetail(_) => {
            key_help(&[("Space", "install / uninstall"), ("Enter/Esc", "back")])
        }
        PluginsScreen::AddMarketplace(_) => {
            key_help(&[("Enter", "add marketplace"), ("Esc", "back")])
        }
        PluginsScreen::Operation(operation) => {
            if operation.finished {
                key_help(&[("Enter/Esc", "back to plugins")])
            } else {
                Line::from(vec![
                    Span::styled("● ", Style::default().fg(ACCENT_COLOR)),
                    Span::styled(
                        "Working… changes are applied when this completes",
                        Style::default().fg(MUTED_TEXT_COLOR),
                    ),
                ])
            }
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

fn styled_selection_line(
    marker: &str,
    icon: &str,
    icon_color: Color,
    text: &str,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let marker = marker.to_owned();
    let icon = format!("{icon} ");
    let used = marker.width() + icon.width();
    let text = truncate_end_to_width(text, width.saturating_sub(used));
    let content_width = used + text.width();
    let padding = " ".repeat(width.saturating_sub(content_width));
    let base = if selected {
        Style::default()
            .bg(SELECTED_BG_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(
            marker,
            base.fg(if selected {
                ACCENT_COLOR
            } else {
                MUTED_TEXT_COLOR
            }),
        ),
        Span::styled(icon, base.fg(icon_color)),
        Span::styled(
            text,
            base.fg(if selected {
                TEXT_COLOR
            } else {
                SOFT_TEXT_COLOR
            }),
        ),
        Span::styled(padding, base),
    ])
}

fn panel_status_color(plugin: &InstalledPluginStatus) -> Color {
    if plugin.config_managed {
        WARNING_COLOR
    } else if plugin.enabled {
        ENABLED_COLOR
    } else {
        MUTED_TEXT_COLOR
    }
}

fn section_divider(title: &str, width: usize) -> Line<'static> {
    let title = format!(" {title} ");
    let rule = "─".repeat(width.saturating_sub(title.width()));
    Line::from(vec![
        Span::styled(
            title,
            Style::default()
                .fg(BORDER_BRIGHT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(rule, Style::default().fg(BORDER_COLOR)),
    ])
}

fn field_line(label: &str, value: &str, width: usize) -> Line<'static> {
    let prefix = format!("{label:<12}");
    let value = truncate_end_to_width(value, width.saturating_sub(prefix.width()));
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled(value, Style::default().fg(SOFT_TEXT_COLOR)),
    ])
}

fn field_lines(label: &str, value: &str, width: usize) -> Vec<Line<'static>> {
    const LABEL_WIDTH: usize = 12;
    if width <= LABEL_WIDTH {
        let mut lines = vec![Line::from(Span::styled(
            label.to_owned(),
            Style::default().fg(MUTED_TEXT_COLOR),
        ))];
        lines.extend(
            wrap_text(value, width.max(1) as u16)
                .into_iter()
                .map(|row| Line::from(Span::styled(row, Style::default().fg(SOFT_TEXT_COLOR)))),
        );
        return lines;
    }

    let prefix = format!("{label:<LABEL_WIDTH$}");
    let mut rows = wrap_text(value, (width - LABEL_WIDTH) as u16).into_iter();
    let first = rows.next().unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled(prefix, Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled(first, Style::default().fg(SOFT_TEXT_COLOR)),
    ])];
    lines.extend(rows.map(|row| {
        Line::from(vec![
            Span::raw(" ".repeat(LABEL_WIDTH)),
            Span::styled(row, Style::default().fg(SOFT_TEXT_COLOR)),
        ])
    }));
    lines
}

fn action_line(key: &str, action: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{key} "),
            Style::default()
                .fg(KEY_HINT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(action.to_owned(), Style::default().fg(MUTED_TEXT_COLOR)),
    ])
}

fn example_line(label: &str, example: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<10}"),
            Style::default().fg(MUTED_TEXT_COLOR),
        ),
        Span::styled(example.to_owned(), Style::default().fg(SOFT_TEXT_COLOR)),
    ])
}

fn status_line(icon: &str, color: Color, title: &str, status: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{icon} "), Style::default().fg(color)),
        Span::styled(
            title.to_owned(),
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {status}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn empty_lines(title: &str, help: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            title.to_owned(),
            Style::default()
                .fg(SOFT_TEXT_COLOR)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            help.to_owned(),
            Style::default().fg(MUTED_TEXT_COLOR),
        )),
    ]
}

fn render_missing(frame: &mut Frame, message: &str, area: Rect) {
    let block = panel_block("× Unavailable", false);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            message.to_owned(),
            Style::default().fg(ERROR_COLOR),
        )))
        .block(block)
        .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn inset_area(area: Rect, vertical: u16, horizontal: u16) -> Rect {
    Rect::new(
        area.x.saturating_add(horizontal.min(area.width / 2)),
        area.y.saturating_add(vertical.min(area.height / 2)),
        area.width
            .saturating_sub(horizontal.min(area.width / 2).saturating_mul(2)),
        area.height
            .saturating_sub(vertical.min(area.height / 2).saturating_mul(2)),
    )
}

fn plugin_source(plugin: &InstalledPluginStatus) -> String {
    if plugin.config_managed {
        "config.yaml".to_owned()
    } else {
        plugin
            .marketplace
            .as_ref()
            .map(|marketplace| format!("marketplace {marketplace}"))
            .unwrap_or_else(|| "local".to_owned())
    }
}

fn list_window_start(selected: usize, len: usize, visible: usize) -> usize {
    if len <= visible {
        0
    } else {
        selected
            .saturating_sub(visible.saturating_sub(1))
            .min(len - visible)
    }
}

fn scroll_value(scroll: usize) -> u16 {
    u16::try_from(scroll).unwrap_or(u16::MAX)
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn list_window_keeps_selection_visible() {
        assert_eq!(list_window_start(0, 10, 4), 0);
        assert_eq!(list_window_start(5, 10, 4), 2);
        assert_eq!(list_window_start(9, 10, 4), 6);
    }

    #[test]
    fn browse_panels_stack_on_narrow_terminals() {
        assert_eq!(browse_panel_direction(120, 30), Direction::Horizontal);
        assert_eq!(browse_panel_direction(80, 30), Direction::Vertical);
        assert_eq!(browse_panel_direction(120, 10), Direction::Vertical);
    }

    #[test]
    fn marketplace_plugin_row_marks_installed_without_extra_metadata() {
        let app = App::test_empty();
        let mut plugin = MarketplacePlugin {
            name: "demo".to_owned(),
            version: "1.2.3".to_owned(),
            description: "Plugin description".to_owned(),
            source: crate::plugins::MarketplacePluginSource::Relative("./demo".to_owned()),
            marketplace: "demo-market".to_owned(),
            installed: true,
        };

        let line = marketplace_row_line(&app, &MarketplaceRow::Plugin(&plugin), false, 40);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(text.trim(), "✓ demo");

        plugin.installed = false;
        let line = marketplace_row_line(&app, &MarketplaceRow::Plugin(&plugin), false, 40);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(text.trim(), "demo");

        plugin.installed = true;
        let installed = marketplace_row_line(&app, &MarketplaceRow::Plugin(&plugin), false, 40)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        plugin.installed = false;
        let available = marketplace_row_line(&app, &MarketplaceRow::Plugin(&plugin), false, 40)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let installed_column = installed
            .find("demo")
            .map(|index| installed[..index].width());
        let available_column = available
            .find("demo")
            .map(|index| available[..index].width());
        assert_eq!(installed_column, available_column);
    }

    #[test]
    fn installed_detail_lists_registered_contributions() {
        let contributions = PluginContributions {
            commands: vec!["/demo:review".to_owned()],
            hooks: vec!["BeforeToolCall (Edit)".to_owned()],
            mcp_servers: vec!["demo:server".to_owned()],
            ..Default::default()
        };

        let lines = contribution_detail_lines(&contributions, 100);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains("/demo:review"));
        assert!(text.contains("BeforeToolCall (Edit)"));
        assert!(text.contains("demo:server"));
    }

    #[test]
    fn contribution_items_render_on_separate_lines() {
        let contributions = PluginContributions {
            agents: vec![
                "agent-sdk-dev:agent-sdk-verifier-py".to_owned(),
                "agent-sdk-dev:agent-sdk-verifier-ts".to_owned(),
            ],
            ..Default::default()
        };

        let rendered = contribution_detail_lines(&contributions, 80)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let py_line = rendered
            .iter()
            .position(|line| line.contains("agent-sdk-verifier-py"))
            .expect("Python agent line");
        let ts_line = rendered
            .iter()
            .position(|line| line.contains("agent-sdk-verifier-ts"))
            .expect("TypeScript agent line");

        assert_ne!(py_line, ts_line);
        assert!(rendered[py_line].trim_start().starts_with("• "));
        assert!(rendered[ts_line].trim_start().starts_with("• "));
    }

    #[test]
    fn config_managed_plugins_use_warning_status() {
        let plugin = InstalledPluginStatus {
            name: "demo".to_owned(),
            version: "1.0.0".to_owned(),
            description: String::new(),
            marketplace: None,
            enabled: true,
            config_managed: true,
            root: None,
            contributions: PluginContributions::default(),
        };

        assert_eq!(panel_status_color(&plugin), WARNING_COLOR);
    }

    #[test]
    fn browse_view_separates_list_and_details() {
        let mut app = App::test_empty();
        app.config
            .extensions
            .installed_plugins
            .push(InstalledPluginStatus {
                name: "demo".to_owned(),
                version: "1.0.0".to_owned(),
                description: "A plugin used to verify the panel hierarchy.".to_owned(),
                marketplace: Some("demo-market".to_owned()),
                enabled: true,
                config_managed: false,
                root: None,
                contributions: PluginContributions {
                    skills: vec!["demo:review".to_owned()],
                    mcp_servers: vec!["demo:server".to_owned()],
                    ..Default::default()
                },
            });
        let view = PluginsView {
            tab: PluginsTab::Installed,
            screen: PluginsScreen::Browse,
            selected_installed: 0,
            selected_marketplace: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
        };
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render_plugins_view(frame, &app, &view))
            .expect("render plugins view");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Installed plugins"));
        assert!(rendered.contains("Plugin details"));
        assert!(rendered.contains("Registered content"));
        assert!(rendered.contains("demo:review"));
        assert!(rendered.contains("demo:server"));
    }

    #[test]
    fn mouse_targets_tabs_list_rows_and_detail_panel() {
        let mut app = App::test_empty();
        app.config
            .extensions
            .installed_plugins
            .push(InstalledPluginStatus {
                name: "demo".to_owned(),
                version: "1.0.0".to_owned(),
                description: "Mouse target fixture".to_owned(),
                marketplace: None,
                enabled: true,
                config_managed: false,
                root: None,
                contributions: PluginContributions::default(),
            });
        app.plugins_view = Some(PluginsView {
            tab: PluginsTab::Installed,
            screen: PluginsScreen::Browse,
            selected_installed: 0,
            selected_marketplace: 0,
            detail_scroll: 0,
            detail_max_scroll: 10,
        });

        assert_eq!(
            mouse_action(&app, MouseAction::LeftDown { column: 18, row: 2 }, 120, 30,),
            PluginsMouseAction::SelectTab(PluginsMouseTab::Marketplaces)
        );
        assert_eq!(
            mouse_action(&app, MouseAction::LeftDown { column: 2, row: 5 }, 120, 30,),
            PluginsMouseAction::SelectItem(0)
        );
        assert_eq!(
            mouse_action(&app, MouseAction::ScrollDown { column: 2, row: 5 }, 120, 30,),
            PluginsMouseAction::MoveSelection(1)
        );
        assert_eq!(
            mouse_action(
                &app,
                MouseAction::ScrollDown { column: 80, row: 6 },
                120,
                30,
            ),
            PluginsMouseAction::ScrollDetails(3)
        );
        assert_eq!(
            mouse_action(&app, MouseAction::LeftDown { column: 80, row: 6 }, 120, 30,),
            PluginsMouseAction::OpenSelected
        );
    }

    #[test]
    fn plugin_detail_scroll_limit_tracks_registered_content() {
        let mut app = App::test_empty();
        app.config
            .extensions
            .installed_plugins
            .push(InstalledPluginStatus {
                name: "demo".to_owned(),
                version: "1.0.0".to_owned(),
                description: String::new(),
                marketplace: None,
                enabled: true,
                config_managed: false,
                root: None,
                contributions: PluginContributions {
                    skills: (0..20).map(|index| format!("demo:skill-{index}")).collect(),
                    ..Default::default()
                },
            });
        app.plugins_view = Some(PluginsView {
            tab: PluginsTab::Installed,
            screen: PluginsScreen::InstalledDetail(0),
            selected_installed: 0,
            selected_marketplace: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
        });

        assert!(detail_max_scroll(&app, 100, 16) > 0);
    }

    #[test]
    fn long_path_fields_wrap_and_indent_continuations() {
        let path = "/home/example/.glint/plugins/cache/marketplace-with-a-long-name/plugin";
        let lines = field_lines("Cache", path, 32);
        let rendered = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(rendered.len() > 1);
        assert!(rendered[0].starts_with("Cache"));
        assert!(rendered[1].starts_with("            "));
        assert_eq!(
            rendered.iter().map(|line| &line[12..]).collect::<String>(),
            path
        );
    }

    #[test]
    fn enable_disable_uses_plugin_activity_panel() {
        let app = App::test_empty();
        let view = PluginsView {
            tab: PluginsTab::Installed,
            screen: PluginsScreen::Operation(PluginOperationView {
                title: "Disabling plugin".to_owned(),
                subject: "demo@demo-market".to_owned(),
                log: vec!["Unloading plugin contributions...".to_owned()],
                uses_git: false,
                finished: false,
                failed: false,
            }),
            selected_installed: 0,
            selected_marketplace: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
        };
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render_plugins_view(frame, &app, &view))
            .expect("render plugins view");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Plugin activity"));
        assert!(!rendered.contains("Git activity"));
    }
}
