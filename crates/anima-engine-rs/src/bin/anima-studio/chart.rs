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
//! The readings live here rather than in the window because nothing else reads them, and because the chart is
//! the only thing that knows how much history it needs.

use std::collections::BTreeMap;

use iced::widget::canvas;
use iced::{Color, Point, Rectangle, Renderer, mouse};

use anima_engine_rs::telemetry::{History, Sample};

/// How much history to keep: five minutes at one sample per second.
const POINTS: usize = 300;

/// One colour per subject, in the order the subjects arrive. Fixed for now — reading them from the material
/// scheme is the refinement, and it changes nothing about whether the numbers are right.
const COLOURS: [Color; 2] = [
    Color::from_rgb(0.55, 0.75, 1.0),
    Color::from_rgb(1.00, 0.70, 0.40),
];

/// The space the labels take at the top of the chart, which the lines must not be drawn through.
const LABEL_BAND: f32 = 16.0;

/// One metric over time, per subject.
#[derive(Debug, Clone)]
pub struct Chart {
    /// What is drawn, for the label: `memory`, `cpu`, `gpu`.
    name: &'static str,
    /// What the numbers are in, for the label. In the unit rather than in a sentence, because a label on a
    /// chart is read at a glance and the field names in the log already carry the same units.
    unit: &'static str,
    /// Which number of a reading this chart draws. `None` means the reading does not have it — a process that
    /// has not touched the GPU reports nothing rather than zero — and such a reading is left out of the line
    /// rather than drawn as a drop to the floor.
    pick: fn(&Sample) -> Option<f32>,
    /// The smallest ceiling to scale to, so a chart of zeros still has an axis rather than a line through it.
    floor: f32,
    histories: BTreeMap<&'static str, History>,
}

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
            histories: BTreeMap::new(),
        }
    }

    /// Add one reading for one subject, starting its series the first time it appears.
    pub fn push(&mut self, who: &'static str, sample: &Sample) {
        self.histories
            .entry(who)
            .or_insert_with(|| History::new(POINTS))
            .push(sample.clone());
    }

    /// Every series in this chart's unit, and the tallest value across them, which is what the y axis is
    /// scaled to. One scale for every line, because two charts that share an axis can be compared and two that
    /// do not cannot.
    fn series(&self) -> (Vec<Vec<f64>>, f64) {
        let pick = self.pick;
        let mut peak: f64 = 0.0;
        let series = self
            .histories
            .values()
            .map(|history| {
                let (values, own_peak) = history.series(pick);
                peak = peak.max(own_peak);
                values
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
        _theme: &iced_material::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let (series, peak) = self.series();
        let names: Vec<&'static str> = self.histories.keys().copied().collect();

        // A wash behind the lines, so they are legible on either scheme. The window's own background shows
        // through the rest.
        frame.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Color::from_rgba(0.0, 0.0, 0.0, 0.25),
        );

        let width = bounds.width;
        let usable = (bounds.height - LABEL_BAND).max(1.0);
        let ceiling = (peak * 1.1).max(f64::from(self.floor));
        let count = series.iter().map(Vec::len).max().unwrap_or(0);

        if count >= 2 {
            for (index, values) in series.iter().enumerate() {
                let colour = COLOURS[index % COLOURS.len()];
                let mut line = canvas::path::Builder::new();
                for (position, value) in values.iter().enumerate() {
                    let x = (position as f32 / (count - 1) as f32) * width;
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
        }

        // The chart names itself, then each line in its own colour with its latest value: a chart of two lines
        // that does not say which is which is a decoration, and neither is one that does not say what it counts.
        frame.fill_text(canvas::Text {
            content: format!("{} ({})", self.name, self.unit),
            position: Point::new(6.0, 24.0),
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.55),
            size: iced::Pixels(10.0),
            ..canvas::Text::default()
        });
        for (index, name) in names.iter().enumerate() {
            let latest = series
                .get(index)
                .and_then(|values| values.last())
                .copied()
                .unwrap_or(0.0);
            frame.fill_text(canvas::Text {
                content: format!("{name} {latest:.1}"),
                position: Point::new(6.0, 11.0 + index as f32 * 13.0),
                color: COLOURS[index % COLOURS.len()],
                size: iced::Pixels(11.0),
                ..canvas::Text::default()
            });
        }

        vec![frame.into_geometry()]
    }
}
