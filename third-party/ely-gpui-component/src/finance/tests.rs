use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext,
    VisualTestContext, Window, div, px,
};
use jiff::Timestamp;

use crate::finance::{
    CandlestickChart, ChartSync, CurrencyConverter, DepthChart, Leg, MarketHeatmap, PayoffDiagram,
    PointFigureChart, RenkoChart, candles::Candle, stage::Visible,
};
use crate::{layout::tests::narrow_width, theme::Theme};

fn candles(count: usize) -> Rc<Vec<Candle>> {
    Rc::new(
        (0..count)
            .map(|ix| {
                let time = Timestamp::from_second(ix as i64 * 86_400).expect("a day");
                Candle::new(time, (10.0, 11.0, 9.0, 10.0 + ix as f64 * 0.01), 100.0)
            })
            .collect(),
    )
}

struct Pair {
    first: Rc<Vec<Candle>>,
    second: Rc<Vec<Candle>>,
    sync: Entity<ChartSync>,
}

impl Render for Pair {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(800.))
            .child(CandlestickChart::new("first", self.first.clone()).sync(&self.sync))
            .child(CandlestickChart::new("second", self.second.clone()).sync(&self.sync))
    }
}

/// Two synced charts of 40 candles, their shared window held on the newest 20.
fn held(cx: &mut TestAppContext) -> (Entity<Pair>, Entity<ChartSync>, &mut VisualTestContext) {
    cx.update(Theme::init);
    let sync = cx.new(|_| ChartSync::default());
    let shared = sync.clone();
    let (view, cx) = cx.add_window_view(|_, _| Pair {
        first: candles(40),
        second: candles(40),
        sync: shared,
    });
    cx.run_until_parked();
    sync.update(cx, |sync, cx| {
        sync.visible = Some(Visible {
            start: 20.0,
            count: 20.0,
        });
        cx.notify();
    });
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    (view, sync, cx)
}

fn start(sync: &Entity<ChartSync>, cx: &mut VisualTestContext) -> f64 {
    sync.read_with(cx, |sync, _| sync.visible.expect("held").start)
}

#[gpui::test]
fn synced_charts_move_their_shared_window_once_per_append(cx: &mut TestAppContext) {
    let (view, sync, cx) = held(cx);
    view.update(cx, |pair, cx| {
        pair.first = candles(45);
        pair.second = candles(45);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        start(&sync, cx),
        25.0,
        "five new candles move the shared window five, not ten"
    );
}

#[gpui::test]
fn a_lagging_chart_leaves_the_shared_window_alone(cx: &mut TestAppContext) {
    let (view, sync, cx) = held(cx);
    view.update(cx, |pair, cx| {
        pair.second = candles(45);
        cx.notify();
    });
    cx.run_until_parked();
    for _ in 0..2 {
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    }
    assert_eq!(
        (start(&sync, cx), sync.read_with(cx, |sync, _| sync.seen)),
        (25.0, 45),
        "redraws while the first chart lags move nothing"
    );
    view.update(cx, |pair, cx| {
        pair.first = candles(45);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        start(&sync, cx),
        25.0,
        "the lagging chart's append is already shown"
    );
}

#[gpui::test]
fn every_chart_fills_a_column_its_block_measures_by_content(cx: &mut TestAppContext) {
    cx.update(Theme::init);
    let leg = Leg {
        call: true,
        strike: 100.0,
        quantity: 1.0,
        premium: 2.0,
    };
    let widths = [
        narrow_width(cx, "chart-root", |_, _| {
            DepthChart::new("depth", [(99.0, 2.0)], [(101.0, 3.0)]).into_any_element()
        }),
        narrow_width(cx, "chart-root", |_, _| {
            MarketHeatmap::new("heatmap")
                .tile("A", 1.0, 0.5)
                .into_any_element()
        }),
        narrow_width(cx, "chart-root", |_, _| {
            CandlestickChart::new("candles", candles(30)).into_any_element()
        }),
        narrow_width(cx, "chart-root", move |_, _| {
            PayoffDiagram::new("payoff", [leg], (80.0, 120.0), 100.0).into_any_element()
        }),
        narrow_width(cx, "chart-root", |_, _| {
            RenkoChart::new("renko", candles(30), 0.1).into_any_element()
        }),
        narrow_width(cx, "chart-root", |_, _| {
            PointFigureChart::new("figure", candles(30), 0.1, 3).into_any_element()
        }),
    ];
    assert_eq!(
        widths,
        [px(240.0); 6],
        "each chart spans the card inside its padding"
    );
}

/// A converter squeezed beside a card in a 280px row.
struct Squeezed;

impl Render for Squeezed {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let converter = CurrencyConverter::new("converter", 1000.0, ("USD", "EUR"), 0.9174);
        div().w(px(280.)).child(
            div()
                .flex()
                .gap_8()
                .child(div().w(px(460.)).child(converter))
                .child(div().flex_1().child(div().w(px(260.)).h(px(100.)))),
        )
    }
}

#[gpui::test]
fn a_squeezed_converter_keeps_room_for_its_currencies(cx: &mut TestAppContext) {
    cx.update(Theme::init);
    let (_, cx) = cx.add_window_view(|_, _| Squeezed);
    cx.run_until_parked();
    let text = cx
        .debug_bounds("input-text")
        .expect("the second currency's text");
    assert_eq!(text.size.width, px(26.), "the frame's inset for its name");
}
