use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy)]
pub(crate) enum Alignment {
    Left,
    Right,
}

pub(crate) struct Cell {
    text: String,
    width: usize,
    style: Option<&'static str>,
}

impl Cell {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self::with_style(value, None)
    }

    pub(crate) fn styled(value: impl Into<String>, style: &'static str) -> Self {
        Self::with_style(value, Some(style))
    }

    pub(crate) fn empty() -> Self {
        Self {
            text: String::new(),
            width: 0,
            style: None,
        }
    }

    fn with_style(value: impl Into<String>, style: Option<&'static str>) -> Self {
        let text = sanitize(value.into());
        let width = UnicodeWidthStr::width(text.as_str());
        Self { text, width, style }
    }
}

pub(crate) struct Table {
    alignments: Box<[Alignment]>,
    widths: Box<[usize]>,
    cells: Vec<Cell>,
    separator: &'static str,
    color: bool,
}

impl Table {
    pub(crate) fn with_capacity(
        alignments: &[Alignment],
        row_capacity: usize,
        color: bool,
    ) -> Self {
        assert!(
            !alignments.is_empty(),
            "a table requires at least one column"
        );
        Self {
            alignments: alignments.into(),
            widths: vec![0; alignments.len()].into_boxed_slice(),
            cells: Vec::with_capacity(alignments.len().saturating_mul(row_capacity)),
            separator: " ",
            color,
        }
    }

    pub(crate) fn push<const N: usize>(&mut self, cells: [Cell; N]) {
        assert_eq!(N, self.alignments.len(), "table row has the wrong width");
        for (column, cell) in cells.iter().enumerate() {
            self.widths[column] = self.widths[column].max(cell.width);
        }
        self.cells.extend(cells);
    }

    pub(crate) fn render(&self) -> String {
        let columns = self.alignments.len();
        let rows = self.cells.len() / columns;
        let visible_columns = self.widths.iter().filter(|width| **width > 0).count();
        if rows == 0 || visible_columns == 0 {
            return String::new();
        }

        let mut capacity = rows.saturating_sub(1);
        capacity = capacity.saturating_add(
            rows.saturating_mul(visible_columns.saturating_sub(1) * self.separator.len()),
        );
        for row in self.cells.chunks_exact(columns) {
            for (column, cell) in row.iter().enumerate() {
                if self.widths[column] == 0 {
                    continue;
                }
                capacity = capacity
                    .saturating_add(cell.text.len())
                    .saturating_add(self.widths[column].saturating_sub(cell.width));
                if self.color
                    && !cell.text.is_empty()
                    && let Some(style) = cell.style
                {
                    capacity = capacity.saturating_add(style.len() + 7);
                }
            }
        }

        let mut output = String::with_capacity(capacity);
        for (row_index, row) in self.cells.chunks_exact(columns).enumerate() {
            if row_index > 0 {
                output.push('\n');
            }
            let last_column = row
                .iter()
                .enumerate()
                .rfind(|(column, cell)| self.widths[*column] > 0 && !cell.text.is_empty())
                .map_or(0, |(column, _)| column);
            let mut emitted = false;
            for (column, cell) in row.iter().enumerate().take(last_column + 1) {
                let width = self.widths[column];
                if width == 0 {
                    continue;
                }
                if emitted {
                    output.push_str(self.separator);
                }
                emitted = true;
                let padding = width.saturating_sub(cell.width);
                if matches!(self.alignments[column], Alignment::Right) {
                    push_spaces(&mut output, padding);
                }
                if self.color && !cell.text.is_empty() {
                    if let Some(style) = cell.style {
                        output.push_str("\u{1b}[");
                        output.push_str(style);
                        output.push('m');
                        output.push_str(&cell.text);
                        output.push_str("\u{1b}[0m");
                    } else {
                        output.push_str(&cell.text);
                    }
                } else {
                    output.push_str(&cell.text);
                }
                if matches!(self.alignments[column], Alignment::Left) {
                    push_spaces(&mut output, padding);
                }
            }
        }
        output
    }
}

fn sanitize(value: String) -> String {
    if !value.chars().any(char::is_control) {
        return value;
    }
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn push_spaces(output: &mut String, count: usize) {
    for _ in 0..count {
        output.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_long_and_wide_unicode_cells_without_truncating() {
        let mut table = Table::with_capacity(&[Alignment::Left, Alignment::Right], 3, false);
        table.push([Cell::new("Session"), Cell::new("Total")]);
        table.push([Cell::new("短い"), Cell::new("9")]);
        table.push([Cell::new("A very long title"), Cell::new("1.2M")]);

        let output = table.render();
        let lines: Vec<_> = output.lines().collect();
        let total_edges: Vec<_> = ["Total", "9", "1.2M"]
            .into_iter()
            .zip(&lines)
            .map(|(value, line)| {
                UnicodeWidthStr::width(&line[..line.find(value).unwrap()])
                    + UnicodeWidthStr::width(value)
            })
            .collect();
        assert_eq!(total_edges, vec![23, 23, 23]);
        assert!(output.contains("A very long title"));
    }

    #[test]
    fn emits_styles_inside_alignment_padding() {
        let mut table = Table::with_capacity(&[Alignment::Left], 2, true);
        table.push([Cell::styled("x", "36")]);
        table.push([Cell::new("wide")]);
        assert_eq!(table.render(), "\u{1b}[36mx\u{1b}[0m   \nwide");
    }

    #[test]
    fn neutralizes_control_characters_in_provider_fields() {
        let cell = Cell::new("unsafe\ntitle\tvalue");
        assert_eq!(cell.text, "unsafe title value");
    }
}
