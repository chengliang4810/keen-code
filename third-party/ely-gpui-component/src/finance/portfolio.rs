use gpui::{
    App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
};
use jiff::civil::Date;

use crate::{
    charts::{AreaChart, LineChart, Series},
    data_display::{KpiCard, Statistic, Tone, TrendIndicator, UsageBar},
    tables::{Cell, Column, DataTable, Row, TreeRow, TreeTable},
    theme::ActiveTheme,
    typography::format,
};

/// A portfolio at a glance: what it is worth, how it moved today and in all, the cash in it, and how it splits across holdings.
#[derive(IntoElement)]
pub struct PortfolioSummary {
    id: ElementId,
    value: f64,
    day: (f64, f64),
    total: (f64, f64),
    cash: f64,
    currency: SharedString,
    holdings: Vec<(SharedString, f64)>,
}

impl PortfolioSummary {
    /// Its worth, today's move and the move since bought, each an amount and a share, and its cash, in an ISO 4217 currency.
    pub fn new(
        id: impl Into<ElementId>,
        value: f64,
        (day, total): ((f64, f64), (f64, f64)),
        cash: f64,
        currency: &str,
    ) -> Self {
        Self {
            id: id.into(),
            value,
            day,
            total,
            cash,
            currency: currency.to_string().into(),
            holdings: Vec::new(),
        }
    }

    /// A holding and what it is worth; with the cash they fill the bar below.
    pub fn holding(mut self, name: impl Into<SharedString>, worth: f64) -> Self {
        self.holdings.push((name.into(), worth));
        self
    }
}

impl RenderOnce for PortfolioSummary {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (symbol, places, _) = format::currency_parts(&self.currency);
        let symbol = SharedString::from(symbol.to_string());
        let figure = |key: &'static str, label: &'static str, value: f64, trend: Option<f64>| {
            let statistic = Statistic::new((self.id.clone(), key), label, value)
                .decimals(places)
                .prefix(symbol.clone());
            KpiCard::new(match trend {
                Some(share) => statistic.trend(TrendIndicator::new(share)),
                None => statistic,
            })
        };
        let bar = self.holdings.iter().fold(
            UsageBar::new(self.value.max(f64::EPSILON)),
            |bar, (name, worth)| bar.part(name.clone(), *worth),
        );
        let bar = if self.cash > 0.0 {
            bar.part("Cash", self.cash)
        } else {
            bar
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .gap_4()
                    .child(figure("value", "Worth", self.value, None).flex_1())
                    .child(figure("day", "Today", self.day.0, Some(self.day.1)).flex_1())
                    .child(
                        figure("total", "Since bought", self.total.0, Some(self.total.1)).flex_1(),
                    )
                    .child(figure("cash", "Cash", self.cash, None).flex_1()),
            )
            .child(bar)
    }
}

/// How far below its highest so far each value sits, as a share: zero at a new high.
pub(crate) fn drawdown(values: &[f64]) -> Vec<f64> {
    let mut high = f64::MIN;
    values
        .iter()
        .map(|value| {
            high = high.max(*value);
            if high > 0.0 { value / high - 1.0 } else { 0.0 }
        })
        .collect()
}

/// Each value as a share gained since the first.
fn growth(values: &[f64]) -> Vec<f64> {
    let first = values
        .first()
        .copied()
        .filter(|first| *first != 0.0)
        .expect("growth runs from a first value other than zero");
    values.iter().map(|value| value / first - 1.0).collect()
}

/// How a portfolio grew against a benchmark, both from the same start, and below it how far it fell from each high.
#[derive(IntoElement)]
pub struct PerformanceChart {
    id: ElementId,
    labels: Vec<SharedString>,
    equity: Vec<f64>,
    benchmark: Option<(SharedString, Vec<f64>)>,
}

impl PerformanceChart {
    /// The portfolio's value at each label, oldest first.
    pub fn new(
        id: impl Into<ElementId>,
        labels: impl IntoIterator<Item = impl Into<SharedString>>,
        equity: impl IntoIterator<Item = f64>,
    ) -> Self {
        let labels: Vec<SharedString> = labels.into_iter().map(Into::into).collect();
        let equity: Vec<f64> = equity.into_iter().collect();
        assert!(
            equity.len() == labels.len() && equity.len() >= 2,
            "a curve needs a value per label and two of them"
        );
        Self {
            id: id.into(),
            labels,
            equity,
            benchmark: None,
        }
    }

    /// A benchmark's values at the same labels.
    pub fn benchmark(
        mut self,
        name: impl Into<SharedString>,
        values: impl IntoIterator<Item = f64>,
    ) -> Self {
        let values: Vec<f64> = values.into_iter().collect();
        assert_eq!(
            values.len(),
            self.labels.len(),
            "a benchmark needs a value per label"
        );
        self.benchmark = Some((name.into(), values));
        self
    }
}

impl RenderOnce for PerformanceChart {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let height = cx.theme().chart().height;
        let percent = |value: f64| format::percent(value, 0, true);
        let curve = LineChart::new((self.id.clone(), "growth"), self.labels.clone())
            .series(Series::new("Portfolio", growth(&self.equity)));
        let curve = match &self.benchmark {
            Some((name, values)) => curve.series(Series::new(name.clone(), growth(values))),
            None => curve,
        };
        let falls = AreaChart::new((self.id.clone(), "drawdown"), self.labels)
            .series(Series::new("Drawdown", drawdown(&self.equity)));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(curve.format(percent).h(height * 0.8))
            .child(falls.format(percent).h(height * 0.4))
    }
}

/// A dividend: when shares must be held by, when it pays, how much for each share, its yield as a share, and how often it pays.
#[derive(Clone, Debug, PartialEq)]
pub struct Dividend {
    pub ex: Date,
    pub pay: Date,
    pub amount: f64,
    pub dividend_yield: f64,
    pub frequency: SharedString,
}

/// Dividends by date: when to hold by, when each pays, how much, its yield and how often.
#[derive(IntoElement)]
pub struct DividendTable {
    id: ElementId,
    dividends: Vec<Dividend>,
    currency: SharedString,
}

impl DividendTable {
    pub fn new(
        id: impl Into<ElementId>,
        dividends: impl IntoIterator<Item = Dividend>,
        currency: &str,
    ) -> Self {
        Self {
            id: id.into(),
            dividends: dividends.into_iter().collect(),
            currency: currency.to_string().into(),
        }
    }
}

impl RenderOnce for DividendTable {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (symbol, places, _) = format::currency_parts(&self.currency);
        let columns = [
            Column::new("ex", "Hold by"),
            Column::new("pay", "Pays"),
            Column::new("amount", "Amount")
                .end()
                .decimals(places)
                .prefix(symbol.to_string()),
            Column::new("yield", "Yield").end(),
            Column::new("frequency", "Frequency"),
        ];
        let day = |date: Date| Cell::Text(date.strftime("%b %-d, %Y").to_string().into());
        let rows: Vec<Row> = self
            .dividends
            .iter()
            .enumerate()
            .map(|(ix, dividend)| {
                Row::new(
                    format!("dividend-{ix}"),
                    [
                        day(dividend.ex),
                        day(dividend.pay),
                        dividend.amount.into(),
                        Cell::Text(format::percent(dividend.dividend_yield, 2, false).into()),
                        Cell::Tag(dividend.frequency.clone(), Tone::Neutral),
                    ],
                )
            })
            .collect();
        DataTable::new(self.id, columns).rows(rows)
    }
}

/// A line of a financial statement: its name, a value for each period, and the lines inside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub name: SharedString,
    pub values: Vec<f64>,
    pub lines: Vec<Statement>,
}

impl Statement {
    pub fn new(name: impl Into<SharedString>, values: impl IntoIterator<Item = f64>) -> Self {
        Self {
            name: name.into(),
            values: values.into_iter().collect(),
            lines: Vec::new(),
        }
    }

    pub fn lines(mut self, lines: impl IntoIterator<Item = Statement>) -> Self {
        self.lines.extend(lines);
        self
    }
}

/// A financial statement by period: its lines as rows that open to the lines inside them, a column for each period.
#[derive(IntoElement)]
pub struct FinancialStatementTable {
    id: ElementId,
    periods: Vec<SharedString>,
    lines: Vec<Statement>,
}

impl FinancialStatementTable {
    pub fn new(
        id: impl Into<ElementId>,
        periods: impl IntoIterator<Item = impl Into<SharedString>>,
        lines: impl IntoIterator<Item = Statement>,
    ) -> Self {
        let periods: Vec<SharedString> = periods.into_iter().map(Into::into).collect();
        let lines: Vec<Statement> = lines.into_iter().collect();
        fn fits(lines: &[Statement], count: usize) -> bool {
            lines
                .iter()
                .all(|line| line.values.len() == count && fits(&line.lines, count))
        }
        assert!(
            fits(&lines, periods.len()),
            "every line needs a value per period"
        );
        Self {
            id: id.into(),
            periods,
            lines,
        }
    }
}

impl RenderOnce for FinancialStatementTable {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        fn row(line: &Statement, key: String) -> TreeRow {
            let cells = std::iter::once(Cell::Text(line.name.clone()))
                .chain(line.values.iter().map(|value| Cell::from(*value)));
            TreeRow::new(key.clone(), cells).children(
                line.lines
                    .iter()
                    .enumerate()
                    .map(|(ix, inner)| row(inner, format!("{key}-{ix}"))),
            )
        }
        let columns = std::iter::once(Column::new("line", "")).chain(
            self.periods.iter().enumerate().map(|(ix, period)| {
                Column::new(format!("period-{ix}"), period.clone())
                    .end()
                    .decimals(0)
            }),
        );
        let rows: Vec<TreeRow> = self
            .lines
            .iter()
            .enumerate()
            .map(|(ix, line)| row(line, format!("line-{ix}")))
            .collect();
        let open: Vec<String> = (0..self.lines.len())
            .map(|ix| format!("line-{ix}"))
            .collect();
        TreeTable::new(self.id, columns, rows).open(open)
    }
}

#[cfg(test)]
mod tests {
    use super::{drawdown, growth};

    #[test]
    fn falls_read_from_each_high_and_growth_from_the_start() {
        assert_eq!(
            drawdown(&[100.0, 120.0, 90.0, 130.0]),
            [0.0, 0.0, -0.25, 0.0]
        );
        let grown = growth(&[100.0, 110.0, 90.0]);
        assert!(
            grown
                .iter()
                .zip([0.0, 0.1, -0.1])
                .all(|(got, want)| (got - want).abs() < 1e-12),
            "{grown:?}"
        );
    }
}
