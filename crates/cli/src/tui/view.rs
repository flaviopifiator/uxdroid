//! Draws the terminal UI's state. Every string from a source or an APK is shown through `clean`,
//! as the commands print them, so none can move the cursor or forge a line.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};

use super::state::{Form, FormKind, Modal, State, VIEWS, View};
use crate::store::{clean, date, shorten, version};

/// Below this, the UI shows only a request for a larger terminal.
pub(crate) const MIN_WIDTH: u16 = 40;
pub(crate) const MIN_HEIGHT: u16 = 10;
/// From this width the details sit beside the results instead of replacing them.
const SIDE_BY_SIDE: u16 = 120;

fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub(crate) fn draw(frame: &mut Frame, state: &State) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let text = format!(
            "uxdroid tui needs a terminal of at least {MIN_WIDTH}x{MIN_HEIGHT}; this one is {}x{}. q quits.",
            area.width, area.height
        );
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), area);
        return;
    }
    // What the user is told while a job runs gets a line above the job's own.
    let status_lines = if state.busy.is_some() && state.status.during_job {
        2
    } else {
        1
    };
    let [tabs, body, status, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(status_lines),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_tabs(frame, state, tabs);
    match state.view {
        View::Search => draw_search(frame, state, body),
        View::Installed => draw_installed(frame, state, body),
        View::Sources => draw_sources(frame, state, body),
    }
    draw_status(frame, state, status);
    frame.render_widget(
        Paragraph::new(hint(state)).style(Style::default().fg(Color::DarkGray)),
        hints,
    );
    match &state.modal {
        Some(Modal::Help) => draw_help(frame, area),
        Some(Modal::Remove(package)) => draw_remove(frame, area, package),
        Some(Modal::Form(form)) => draw_form(frame, area, form),
        None => {}
    }
}

/// From this width the top line names the program before the views.
const BRAND: u16 = 60;

fn draw_tabs(frame: &mut Frame, state: &State, area: Rect) {
    let area = if area.width >= BRAND {
        let name = ">_ uxdroid ";
        let [brand, rest] =
            Layout::horizontal([Constraint::Length(name.len() as u16), Constraint::Min(0)])
                .areas(area);
        let (red, green, blue) = crate::logo::ACCENT;
        frame.render_widget(
            Paragraph::new(name).style(
                Style::default()
                    .fg(Color::Rgb(red, green, blue))
                    .add_modifier(Modifier::BOLD),
            ),
            brand,
        );
        rest
    } else {
        area
    };
    let selected = VIEWS
        .iter()
        .position(|(view, _)| *view == state.view)
        .unwrap_or(0);
    let titles: Vec<String> = VIEWS
        .iter()
        .enumerate()
        .map(|(index, (_, title))| format!("{} {title}", index + 1))
        .collect();
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)),
        area,
    );
}

fn hint(state: &State) -> String {
    match (&state.modal, state.view, state.typing) {
        (Some(Modal::Help), ..) => "any key closes the help".to_string(),
        (Some(Modal::Remove(_)), ..) => "y removes, n or Esc keeps".to_string(),
        (Some(Modal::Form(_)), ..) => "Tab next field  Enter submit  Esc cancel".to_string(),
        (None, View::Search, true) if state.play_enabled() => {
            "type to search  Enter asks Google Play  ↓ results  Tab next  Ctrl-R refresh  Ctrl-C quit"
                .to_string()
        }
        (None, View::Search, true) => {
            "type to search  Enter/↓ results  Tab next  Ctrl-R refresh  Ctrl-C quit".to_string()
        }
        (None, View::Search, false) => {
            "/ search  ↑↓ jk move  Enter details  i install  u update  d remove  Tab next  ? help  q quit"
                .to_string()
        }
        (None, View::Installed, _) => {
            "↑↓ jk move  u update  U update all  d remove  r refresh  Tab next  ? help  q quit"
                .to_string()
        }
        (None, View::Sources, _) => {
            "↑↓ jk move  e enable/disable  a add  h refresh interval  r refresh  ? help  q quit"
                .to_string()
        }
    }
}

fn draw_status(frame: &mut Frame, state: &State, area: Rect) {
    let message = Paragraph::new(state.status.text.clone()).style(if state.status.error {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    });
    let Some(busy) = &state.busy else {
        frame.render_widget(message, area);
        return;
    };
    let job = match state.transfer {
        Some((done, total)) => format!(
            "{busy}: {} of {} ({}%)",
            size(done),
            size(total),
            done.saturating_mul(100) / total.max(1)
        ),
        None => format!("{busy}…"),
    };
    let job = Paragraph::new(job).style(Style::default().fg(Color::Yellow));
    if state.status.during_job && area.height >= 2 {
        let [above, below] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        frame.render_widget(message, above);
        frame.render_widget(job, below);
    } else {
        frame.render_widget(job, area);
    }
}

fn bordered(title: String) -> Block<'static> {
    Block::default().borders(Borders::ALL).title(title)
}

fn highlight(active: bool) -> Style {
    if active {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().add_modifier(Modifier::UNDERLINED)
    }
}

fn draw_search(frame: &mut Frame, state: &State, area: Rect) {
    let compact = area.height < 12;
    let [query, list] = Layout::vertical([
        Constraint::Length(if compact { 1 } else { 3 }),
        Constraint::Min(0),
    ])
    .areas(area);
    let cursor = if state.typing { "▏" } else { "" };
    let text = format!("{}{cursor}", state.query);
    if compact {
        frame.render_widget(Paragraph::new(format!("/ {text}")), query);
    } else {
        let style = if state.typing {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(text).block(bordered(" Search ".to_string()).border_style(style)),
            query,
        );
    }
    if list.width >= SIDE_BY_SIDE {
        let [results, details] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(list);
        draw_results(frame, state, results);
        draw_details(frame, state, details);
    } else if state.details {
        draw_details(frame, state, list);
    } else {
        draw_results(frame, state, list);
    }
}

fn draw_results(frame: &mut Frame, state: &State, area: Rect) {
    let Some(apps) = &state.apps else {
        let mut lines = vec!["reading the indexes of the enabled sources…".to_string()];
        lines.extend(state.notes.iter().cloned());
        frame.render_widget(
            Paragraph::new(lines.join("\n"))
                .wrap(Wrap { trim: true })
                .block(bordered(" Apps ".to_string())),
            area,
        );
        return;
    };
    if state.hits.is_empty() {
        let mut lines = Vec::new();
        if state.query.trim().is_empty() {
            lines.push(format!(
                "type to search {} apps from the enabled sources",
                apps.len()
            ));
        } else {
            lines.push(format!("no app matches {}", clean(&state.query)));
        }
        if state.unfit > 0 {
            lines.push(format!(
                "{} more match but have no version for {} and SDK {}",
                state.unfit, state.device.abi, state.device.sdk
            ));
        }
        lines.extend(state.notes.iter().cloned());
        frame.render_widget(
            Paragraph::new(lines.join("\n"))
                .wrap(Wrap { trim: true })
                .block(bordered(" Apps ".to_string())),
            area,
        );
        return;
    }
    let wide = area.width >= 76;
    let header = if wide {
        vec!["NAME", "PACKAGE", "SOURCE", "VERSION", "UPDATED", ""]
    } else {
        vec!["NAME", "VERSION", ""]
    };
    let rows: Vec<Row> = state
        .hits
        .iter()
        .map(|index| {
            let app = &apps[*index];
            let release = app.newest(&state.device);
            let installed = if state.listed(&app.package).is_some() {
                "✓"
            } else {
                ""
            };
            let name = clean(&app.name);
            let release_version =
                release.map_or_else(String::new, |release| clean(&release.version_name));
            if wide {
                let updated = app
                    .updated
                    .or(release.and_then(|release| release.added))
                    .map_or("unknown".to_string(), date);
                Row::new(vec![
                    Cell::from(name),
                    Cell::from(app.package.clone()),
                    Cell::from(app.source.clone()),
                    Cell::from(release_version),
                    Cell::from(updated),
                    Cell::from(installed),
                ])
            } else {
                Row::new(vec![
                    Cell::from(name),
                    Cell::from(release_version),
                    Cell::from(installed),
                ])
            }
        })
        .collect();
    let widths: Vec<Constraint> = if wide {
        vec![
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Fill(1),
            Constraint::Length(12),
            Constraint::Length(1),
        ]
    };
    let mut title = format!(" {} of {} apps ", state.hits.len(), apps.len());
    if state.unfit > 0 {
        title.push_str(&format!("({} more without a version here) ", state.unfit));
    }
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(Style::default().add_modifier(Modifier::BOLD)))
        .row_highlight_style(highlight(!state.typing))
        .block(bordered(title));
    let mut table_state = TableState::default().with_selected(Some(state.selected()));
    frame.render_stateful_widget(table, area, &mut table_state);
}

fn details_text(state: &State) -> Vec<Line<'static>> {
    let Some((app, release)) = state.hit() else {
        return vec![Line::from("select an app to see its details")];
    };
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut lines = vec![
        Line::from(Span::styled(clean(&app.name), bold)),
        Line::from(format!("{} from {}", app.package, app.source)),
        Line::from(format!(
            "version {}, updated {}",
            version(&release.version_name, release.version_code),
            app.updated
                .or(release.added)
                .map_or("unknown".to_string(), date)
        )),
        Line::from(format!(
            "license {}, size {}, SDK {} or later",
            app.license.as_deref().map_or("unknown".to_string(), clean),
            size(release.size),
            release.min_sdk
        )),
    ];
    lines.push(Line::from(match state.listed(&app.package) {
        Some(listed) => match state.available(listed) {
            Some(newer) => format!(
                "installed {} from {}; u updates it to {}",
                listed.version,
                listed.source,
                version(&newer.version_name, newer.version_code)
            ),
            None => format!("installed {} from {}", listed.version, listed.source),
        },
        None => "not installed; i installs it".to_string(),
    }));
    lines.push(Line::from(""));
    if !app.summary.is_empty() {
        lines.push(Line::from(clean(&app.summary)));
        lines.push(Line::from(""));
    }
    let key = (
        app.source.clone(),
        app.package.clone(),
        release.version_code,
    );
    match state.permissions.as_ref().map(|known| known.get(&key)) {
        None => lines.push(Line::from("permissions: reading…")),
        Some(None) => lines.push(Line::from(
            "permissions: not in the cached index; r refreshes it",
        )),
        Some(Some(permissions)) if permissions.is_empty() => {
            lines.push(Line::from("permissions: none"))
        }
        Some(Some(permissions)) => {
            lines.push(Line::from(format!("permissions ({}):", permissions.len())));
            for permission in permissions {
                lines.push(Line::from(format!("  {}", clean(permission))));
            }
        }
    }
    lines
}

fn draw_details(frame: &mut Frame, state: &State, area: Rect) {
    frame.render_widget(
        Paragraph::new(details_text(state))
            .wrap(Wrap { trim: false })
            .block(bordered(" Details ".to_string())),
        area,
    );
}

fn draw_installed(frame: &mut Frame, state: &State, area: Rect) {
    if state.installed.is_empty() {
        frame.render_widget(
            Paragraph::new(format!(
                "no app is installed in {}",
                clean(&state.data.display().to_string())
            ))
            .wrap(Wrap { trim: true })
            .block(bordered(" Installed ".to_string())),
            area,
        );
        return;
    }
    let mut updates = 0;
    let rows: Vec<Row> = state
        .installed
        .iter()
        .map(|listed| {
            let available = match (&listed.record, state.available(listed)) {
                (_, Some(newer)) => {
                    updates += 1;
                    version(&newer.version_name, newer.version_code)
                }
                (Some(_), None) if state.apps.is_some() => "up to date".to_string(),
                _ => String::new(),
            };
            Row::new(vec![
                listed.package.clone(),
                listed.source.clone(),
                listed.version.clone(),
                available,
            ])
        })
        .collect();
    let title = format!(
        " {} installed, {updates} {} ",
        state.installed.len(),
        if updates == 1 { "update" } else { "updates" }
    );
    let table = Table::new(
        rows,
        [
            Constraint::Fill(2),
            Constraint::Length(8),
            Constraint::Fill(1),
            Constraint::Fill(1),
        ],
    )
    .header(
        Row::new(vec!["PACKAGE", "SOURCE", "VERSION", "AVAILABLE"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(highlight(true))
    .block(bordered(title));
    let mut table_state = TableState::default().with_selected(Some(state.selected()));
    frame.render_stateful_widget(table, area, &mut table_state);
}

fn draw_sources(frame: &mut Frame, state: &State, area: Rect) {
    let Some(config) = &state.config else {
        frame.render_widget(
            Paragraph::new("reading the sources…").block(bordered(" Sources ".to_string())),
            area,
        );
        return;
    };
    let [list, interval] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let rows: Vec<Row> = config
        .sources
        .iter()
        .map(|source| {
            Row::new(vec![
                source.name.clone(),
                source.kind.to_string(),
                if source.enabled { "yes" } else { "no" }.to_string(),
                source.url.clone(),
                shorten(&source.fingerprint, 17),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Fill(1),
            Constraint::Length(17),
        ],
    )
    .header(
        Row::new(vec!["NAME", "KIND", "ENABLED", "URL", "FINGERPRINT"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(highlight(true))
    .block(bordered(format!(" {} sources ", config.sources.len())));
    let mut table_state = TableState::default().with_selected(Some(state.selected()));
    frame.render_stateful_widget(table, list, &mut table_state);
    frame.render_widget(
        Paragraph::new(format!(
            " indexes are refreshed when older than {} hours",
            config.refresh_after_hours
        )),
        interval,
    );
}

/// A box of `width` by `height` in the middle of `area`, no larger than it.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

const HELP: &[(&str, &str)] = &[
    ("/", "search: type, Enter or ↓ to the results"),
    ("↑ ↓ j k", "move; PgUp PgDn g G jump"),
    ("Tab Shift-Tab", "next or previous pane"),
    ("1 2 3", "Search, Installed, Sources"),
    ("Enter", "details of the selected app"),
    ("i", "install the selected app"),
    ("u / U", "update the selected app / all"),
    ("d", "remove the selected app, after asking"),
    ("r Ctrl-R F5", "refresh the indexes"),
    ("e a h", "sources: enable/disable, add, interval"),
    ("Esc", "close, or cancel a download"),
    ("q Ctrl-C", "quit"),
];

fn draw_help(frame: &mut Frame, area: Rect) {
    let rect = centered(area, 60, HELP.len() as u16 + 2);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(keys, what)| {
            Line::from(vec![
                Span::styled(
                    format!("{keys:>13}  "),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(*what),
            ])
        })
        .collect();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(bordered(" Keys ".to_string())),
        rect,
    );
}

fn draw_remove(frame: &mut Frame, area: Rect, package: &str) {
    let rect = centered(area, 60, 5);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(format!(
            "Remove {package} with its data, signer record and component settings? y/n"
        ))
        .wrap(Wrap { trim: true })
        .block(bordered(" Remove ".to_string()).border_style(Style::default().fg(Color::Red))),
        rect,
    );
}

fn draw_form(frame: &mut Frame, area: Rect, form: &Form) {
    let title = match form.kind {
        FormKind::AddSource => " Add an F-Droid repository ",
        FormKind::RefreshAfter => " Refresh the indexes when older than ",
    };
    let mut lines: Vec<Line> = form
        .fields
        .iter()
        .enumerate()
        .map(|(index, (label, value))| {
            let focused = index == form.focus;
            let cursor = if focused { "▏" } else { "" };
            let style = if focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(format!("{label:>12}: "), style),
                Span::raw(format!("{}{cursor}", clean(value))),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    match (&form.error, form.pending) {
        (_, true) => lines.push(Line::from("checking…")),
        (Some(error), _) => lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        ))),
        (None, _) => lines.push(Line::from(match form.kind {
            FormKind::AddSource => {
                "an https URL and the SHA-256 of the certificate its index is signed with"
            }
            FormKind::RefreshAfter => "whole hours, 0 to 8760",
        })),
    }
    let rect = centered(area, 72, lines.len() as u16 + 4);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(bordered(title.to_string())),
        rect,
    );
}
