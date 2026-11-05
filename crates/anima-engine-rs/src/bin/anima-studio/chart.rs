//! The resource charts: one number over time, one line per process being watched.
//!
//! Drawn with `iced::widget::canvas` rather than a plotting crate, and that is not a preference. This window's
//! element tree is typed by `iced_material::Theme`, and a widget that names `iced::Theme` cannot be mounted in
//! it; the one crate tried for this draws through a `shader::Program`, whose element needs a renderer that
//! implements `primitive::Renderer` — which in iced 0.14 is `iced_wgpu::Renderer` alone, while every element in
//! an application is typed by iced's own `Renderer`. A canvas `Program` takes the theme *as a parameter*, so
//! this one is written for the theme the window actually uses, which is also why the colours can come from the
//! material scheme later without a translation layer.
//!
//! One chart is one metric; the subjects are lines on it. That is the arrangement because a shared axis is what
//! makes two subjects comparable, and because three charts of one metric each is the same code three times
//! rather than three nearly identical pieces of code.
//!
//! The colours come from the window's own theme rather than from a palette of its own: the two lines are
//! material's primary and secondary, and the ink is chosen for a light or a dark scheme, so the chart reads as
//! part of the window it is in and stays legible when the user's palette is light.
//!
//! Each point carries the instant it was read, and the x axis is that in seconds from the oldest point still
//! held. A tick that never fired is therefore a gap in the line rather than a compression of it: the readings
//! come from a timer, and a chart that counts readings instead of time would draw the same shape whether they
//! arrived every second or every ten.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use iced::widget::canvas;
use iced::{Color, Point, Rectangle, Renderer, mouse};

use anima_engine_rs::telemetry::Sample;

/// How much history to keep: five minutes at one sample per second.
const POINTS: usize = 300;

/// The space the labels take at the top of the chart, which the lines must not be drawn through.
const LABEL_BAND: f32 = 16.0;

/// One metric over time, per subject.
#[derive(Debug, Clone)]
pub struct Chart {
    /// What is drawn, for the label: `memory`, `cpu`, `gpu`.
    name: &'static str,
    /// What the numbers are in, for the label. The unit rather than a sentence, because a label on a chart is
    /// read at a glance and the field names in the log already carry the same units.
    unit: &'static str,
    /// Which number of a reading this chart draws. `None` means the reading does not have it — a process that
    /// has not touched the GPU reports nothing rather than zero — and such a reading is left out of the line
    /// rather than drawn as a drop to the floor.
    pick: fn(&Sample) -> Option<f32>,
    /// The smallest ceiling to scale to, so a chart of zeros still has an axis rather than a line lying on it.
    floor: f32,
    /// The instant each reading was taken, and its value.
    series: BTreeMap<&'static str, VecDeque<(Instant, f32)>>,
}

/// One point as drawn: seconds from the oldest point still held, and the value.
type Plotted = (f64, f64);

impl Chart {
    /// What a process costs.
    pub fn memory() -> Self {
        Self::new("memory", "MiB", |sample| {
            Some(sample.memory_bytes as f32 / (1024.0 * 1024.0))
        })
    }

    /// How much of one core it is using, which is why a busy window reads above 100.
    pub fn cpu() -> Self {
        Self::new("cpu", "%", |sample| Some(sample.cpu))
    }

    /// How much of the GPU's time it has been given.
    pub fn gpu() -> Self {
        Self::new("gpu", "%", |sample| sample.gpu)
    }

    fn new(name: &'static str, unit: &'static str, pick: fn(&Sample) -> Option<f32>) -> Self {
        Self {
            name,
            unit,
            pick,
            floor: 1.0,
            series: BTreeMap::new(),
        }
    }

    /// Add one reading for one subject, at the instant it was taken, starting that subject's line the first
    /// time it appears. A reading with nothing to draw is dropped here rather than at the drawing, so the times
    /// and the values cannot get out of step.
    pub fn push(&mut self, who: &'static str, sample: &Sample, at: Instant) {
        let Some(value) = (self.pick)(sample) else {
            return;
        };
        let points = self.series.entry(who).or_default();
        if points.len() == POINTS {
            points.pop_front();
        }
        points.push_back((at, value));
    }

    /// Every line in this chart's unit, on a shared scale, and the tallest value across them.
    ///
    /// The x axis starts at the oldest point still held rather than at the first reading ever taken: a window
    /// measured from a fixed origin would keep its last five minutes squeezed into the right-hand edge.
    fn window(&self) -> (Vec<Vec<Plotted>>, f64) {
        let Some(oldest) = self
            .series
            .values()
            .filter_map(|points| points.front())
            .map(|(at, _)| *at)
            .min()
        else {
            return (Vec::new(), 0.0);
        };
        let mut peak: f64 = 0.0;
        let series = self
            .series
            .values()
            .map(|points| {
                points
                    .iter()
                    .map(|(at, value)| {
                        let seconds = at.saturating_duration_since(oldest).as_secs_f64();
                        peak = peak.max(f64::from(*value));
                        (seconds, f64::from(*value))
                    })
                    .collect()
            })
            .collect();
        (series, peak)
    }
}

impl canvas::Program<crate::Message, iced_material::Theme, Renderer> for Chart {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &iced_material::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let (series, peak) = self.window();
        let names: Vec<&'static str> = self.series.keys().copied().collect();

        // The two lines are material's own accents, and the ink and the wash are picked for a light or a dark
        // scheme rather than assumed to be light — a Material You palette can be either, and a chart drawn in
        // white on a light window is a chart nobody can read.
        let colours = [theme.colors().primary.color, theme.colors().secondary.color];
        let (ink, wash) = if theme.is_dark() {
            (
                Color::from_rgba(1.0, 1.0, 1.0, 0.75),
                Color::from_rgba(0.0, 0.0, 0.0, 0.20),
            )
        } else {
            (
                Color::from_rgba(0.0, 0.0, 0.0, 0.75),
                Color::from_rgba(1.0, 1.0, 1.0, 0.30),
            )
        };
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), wash);

        let width = bounds.width;
        let usable = (bounds.height - LABEL_BAND).max(1.0);
        let ceiling = (peak * 1.1).max(f64::from(self.floor));
        // One reading is a point and a line needs two of them, which is what the span being at least a
        // millisecond guards: it cannot be zero once anything has been read.
        let span = series
            .iter()
            .filter_map(|points| points.last())
            .map(|(seconds, _)| *seconds)
            .fold(0.0f64, f64::max)
            .max(0.001);

        for (index, points) in series.iter().enumerate() {
            if points.len() < 2 {
                continue;
            }
            let colour = colours[index % colours.len()];
            let mut line = canvas::path::Builder::new();
            for (position, (seconds, value)) in points.iter().enumerate() {
                let x = (seconds / span).min(1.0) as f32 * width;
                let y = LABEL_BAND + usable * (1.0 - (value / ceiling).min(1.0) as f32);
                let point = Point::new(x, y);
                if position == 0 {
                    line.move_to(point);
                } else {
                    line.line_to(point);
                }
            }
            frame.stroke(
                &line.build(),
                canvas::Stroke::default().with_color(colour).with_width(1.5),
            );
        }

        // The chart names itself, then each line in its own colour with its latest value: a chart of two lines
        // that does not say which is which is a decoration, and so is one that does not say what it counts.
        frame.fill_text(canvas::Text {
            content: format!("{} ({})", self.name, self.unit),
            position: Point::new(6.0, 24.0),
            color: Color { a: 0.55, ..ink },
            size: iced::Pixels(10.0),
            ..canvas::Text::default()
        });
        for (index, name) in names.iter().enumerate() {
            let latest = series
                .get(index)
                .and_then(|points| points.last())
                .map(|(_, value)| *value)
                .unwrap_or(0.0);
            frame.fill_text(canvas::Text {
                content: format!("{name} {latest:.1}"),
                position: Point::new(6.0, 11.0 + index as f32 * 13.0),
                color: colours[index % colours.len()],
                size: iced::Pixels(11.0),
                ..canvas::Text::default()
            });
        }

        vec![frame.into_geometry()]
    }
}
