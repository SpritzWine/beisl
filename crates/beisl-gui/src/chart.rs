//! Charts for the results view, drawn on cosmic's own Canvas.
//!
//! We reuse **plotters** — a backend-agnostic chart engine — for the frametime timeline
//! (a real time axis with ticks and auto-scaling), rendering it through a small
//! [`DrawingBackend`] that targets cosmic's `canvas::Frame`. plotters brings no iced of its
//! own — it only emits drawing primitives — so it coexists with cosmic's vendored iced fork
//! where the `plotters-iced` *integration* crate (bound to a different, upstream iced) can
//! never link into a `cosmic::Element`. cosmic's Canvas still owns DPI scaling and redraw,
//! so the chart scales and animates like any other widget; the adapter below is the one
//! bespoke seam, and it calls the same `Frame` API any hand-drawn canvas code would.

use std::fmt;

use cosmic::iced::{Color, Length, Point, Rectangle, Size};
use cosmic::widget::canvas::{self, stroke, Cache, Canvas, Frame, Geometry, Path, Stroke, Text};
use cosmic::{Element, Renderer, Theme};

use plotters::prelude::*;
use plotters::style::Color as _; // brings `.filled()` / `.stroke_width()` on colors into scope
use plotters_backend::{
    BackendColor, BackendCoord, BackendStyle, BackendTextStyle, DrawingBackend, DrawingErrorKind,
};

/// The color for a stack layer. The GPU `driver` layer is tinted by vendor (Intel blue, AMD
/// red, Nvidia green, Qualcomm red) so the bar reads at a glance; the rest get a fixed,
/// distinguishable palette — `wine` in wine-red, naturally.
pub fn layer_color(layer: &str, gpu_vendor: &str) -> Color {
    match layer {
        "kernel" => Color::from_rgb8(0x5a, 0x67, 0x77), // slate
        "driver" => vendor_color(gpu_vendor),           // GPU vendor
        "dxvk/vkd3d" => Color::from_rgb8(0x8e, 0x6f, 0xd6), // violet
        "wine" => Color::from_rgb8(0x9c, 0x2b, 0x3b),   // wine red
        "game" => Color::from_rgb8(0xe0, 0xa0, 0x30),   // amber
        _ => Color::from_rgb8(0x80, 0x80, 0x80),
    }
}

fn vendor_color(vendor: &str) -> Color {
    match vendor {
        "intel" => Color::from_rgb8(0x00, 0x71, 0xc5), // Intel blue
        "amd" => Color::from_rgb8(0xed, 0x1c, 0x24),   // AMD red
        "nvidia" => Color::from_rgb8(0x76, 0xb9, 0x00), // Nvidia green
        "qualcomm" => Color::from_rgb8(0xe4, 0x00, 0x2b), // Snapdragon red
        _ => Color::from_rgb8(0x3a, 0x91, 0x88),       // undetected: teal
    }
}

/// A stacked horizontal bar of the layer split — one colored segment per layer, widths
/// proportional to each layer's share. The visual companion to the precise text legend
/// below it in the results view.
pub struct LayerBar {
    cache: Cache,
    /// (share 0..=100, color), in draw order.
    segments: Vec<(f32, Color)>,
}

impl LayerBar {
    pub fn new(segments: Vec<(f32, Color)>) -> Self {
        Self {
            cache: Cache::new(),
            segments,
        }
    }

    pub fn view<M: 'static>(self) -> Element<'static, M> {
        Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fixed(26.0))
            .into()
    }
}

impl<M> canvas::Program<M, Theme, Renderer> for LayerBar {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            let h = frame.height();
            let w = frame.width();
            let total: f32 = self.segments.iter().map(|(p, _)| p).sum::<f32>().max(1.0);
            let mut x = 0.0f32;
            for (pct, color) in &self.segments {
                let seg = (pct / total) * w;
                // A 1px inset between segments keeps adjacent colors legible.
                let width = (seg - 1.0).max(0.0);
                frame.fill_rectangle(Point::new(x, 0.0), Size::new(width, h), *color);
                x += seg;
            }
        });
        vec![geometry]
    }
}

/// A small solid color square for a legend row — ties each legend line to its bar segment.
pub fn swatch<M: 'static>(color: Color) -> Element<'static, M> {
    Canvas::new(Swatch(color))
        .width(Length::Fixed(12.0))
        .height(Length::Fixed(12.0))
        .into()
}

struct Swatch(Color);

impl<M> canvas::Program<M, Theme, Renderer> for Swatch {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::new(0.0, 0.0), bounds.size(), self.0);
        vec![frame.into_geometry()]
    }
}

/// The frametime timeline: one line series over session time, with the worst spikes marked.
pub struct FrametimeChart {
    cache: Cache,
    /// (t_seconds, frametime_ms), in order.
    points: Vec<(f64, f64)>,
    /// The flagged spikes as (t_seconds, frametime_ms) — drawn as markers on the line.
    spikes: Vec<(f64, f64)>,
}

impl FrametimeChart {
    pub fn new(points: Vec<(f64, f64)>, spikes: Vec<(f64, f64)>) -> Self {
        Self {
            cache: Cache::new(),
            points,
            spikes,
        }
    }

    /// Wrap the chart in a fixed-height Canvas for the results column.
    pub fn view<M: 'static>(self) -> Element<'static, M> {
        Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fixed(150.0))
            .into()
    }
}

impl<M> canvas::Program<M, Theme, Renderer> for FrametimeChart {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            let backend = FrameBackend::new(frame);
            // Errors here are cosmetic (a chart that didn't draw), never fatal — swallow them
            // rather than propagate into the render path.
            let _ = render_timeline(backend, &self.points, &self.spikes);
        });
        vec![geometry]
    }
}

/// Build the timeline into a plotters drawing area over our Frame backend.
fn render_timeline(
    backend: FrameBackend<'_>,
    points: &[(f64, f64)],
    spikes: &[(f64, f64)],
) -> Result<(), Box<dyn std::error::Error>> {
    let area = backend.into_drawing_area();

    let x_max = points.last().map(|p| p.0).unwrap_or(1.0).max(1.0);
    let y_max = points.iter().map(|p| p.1).fold(1.0_f64, f64::max).max(1.0) * 1.15;

    // Muted palette that reads on either theme (the chart sits on the window background).
    let axis = RGBColor(140, 140, 150);
    let line = RGBColor(90, 150, 245);
    let spike = RGBColor(240, 110, 110);

    let mut chart = ChartBuilder::on(&area)
        .margin(6)
        .x_label_area_size(18)
        .y_label_area_size(34)
        .build_cartesian_2d(0.0..x_max, 0.0..y_max)?;

    chart
        .configure_mesh()
        .light_line_style(RGBAColor(140, 140, 150, 0.12))
        .bold_line_style(RGBAColor(140, 140, 150, 0.22))
        .axis_style(axis)
        .label_style(("sans-serif", 11).into_font().color(&axis))
        .x_desc("seconds")
        .y_desc("ms")
        .x_labels(6)
        .y_labels(4)
        .draw()?;

    let line_style = plotters::style::Color::stroke_width(&line, 2);
    chart.draw_series(LineSeries::new(points.iter().copied(), line_style))?;
    // Mark the spikes so the worst hitches stand out on the line.
    chart.draw_series(
        spikes
            .iter()
            .map(|&(x, y)| Circle::new((x, y), 3, spike.filled())),
    )?;

    area.present()?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The one bespoke seam: a plotters DrawingBackend that renders into cosmic's canvas Frame.
// Every method maps a plotters primitive onto a Frame vector op — the same `fill`/`stroke`/
// `fill_text` calls hand-drawn canvas code would make.
// ---------------------------------------------------------------------------------------

struct FrameBackend<'a> {
    frame: &'a mut Frame,
    size: (u32, u32),
}

impl<'a> FrameBackend<'a> {
    fn new(frame: &'a mut Frame) -> Self {
        let s = frame.size();
        let size = (s.width as u32, s.height as u32);
        Self { frame, size }
    }
}

fn point(c: BackendCoord) -> Point {
    Point::new(c.0 as f32, c.1 as f32)
}

fn color(c: BackendColor) -> Color {
    Color::from_rgba(
        c.rgb.0 as f32 / 255.0,
        c.rgb.1 as f32 / 255.0,
        c.rgb.2 as f32 / 255.0,
        c.alpha as f32,
    )
}

fn line_stroke(c: BackendColor, width: u32) -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(color(c)),
        width: width.max(1) as f32,
        ..Stroke::default()
    }
}

#[derive(Debug)]
pub struct BackendErr;

impl fmt::Display for BackendErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "canvas drawing backend error")
    }
}

impl std::error::Error for BackendErr {}

impl DrawingBackend for FrameBackend<'_> {
    type ErrorType = BackendErr;

    fn get_size(&self) -> (u32, u32) {
        self.size
    }

    fn ensure_prepared(&mut self) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        Ok(())
    }

    fn present(&mut self) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        Ok(())
    }

    fn draw_pixel(
        &mut self,
        p: BackendCoord,
        c: BackendColor,
    ) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        self.frame
            .fill_rectangle(point(p), Size::new(1.0, 1.0), color(c));
        Ok(())
    }

    fn draw_line<S: BackendStyle>(
        &mut self,
        from: BackendCoord,
        to: BackendCoord,
        style: &S,
    ) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        let path = Path::line(point(from), point(to));
        self.frame
            .stroke(&path, line_stroke(style.color(), style.stroke_width()));
        Ok(())
    }

    fn draw_rect<S: BackendStyle>(
        &mut self,
        ul: BackendCoord,
        br: BackendCoord,
        style: &S,
        fill: bool,
    ) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        let tl = point(ul);
        let size = Size::new((br.0 - ul.0) as f32, (br.1 - ul.1) as f32);
        if fill {
            self.frame.fill_rectangle(tl, size, color(style.color()));
        } else {
            let path = Path::rectangle(tl, size);
            self.frame
                .stroke(&path, line_stroke(style.color(), style.stroke_width()));
        }
        Ok(())
    }

    fn draw_path<S: BackendStyle, I: IntoIterator<Item = BackendCoord>>(
        &mut self,
        path: I,
        style: &S,
    ) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        let pts: Vec<Point> = path.into_iter().map(point).collect();
        if pts.len() < 2 {
            return Ok(());
        }
        let p = Path::new(|b| {
            b.move_to(pts[0]);
            for q in &pts[1..] {
                b.line_to(*q);
            }
        });
        self.frame
            .stroke(&p, line_stroke(style.color(), style.stroke_width()));
        Ok(())
    }

    fn fill_polygon<S: BackendStyle, I: IntoIterator<Item = BackendCoord>>(
        &mut self,
        vert: I,
        style: &S,
    ) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        let pts: Vec<Point> = vert.into_iter().map(point).collect();
        if pts.len() < 3 {
            return Ok(());
        }
        let p = Path::new(|b| {
            b.move_to(pts[0]);
            for q in &pts[1..] {
                b.line_to(*q);
            }
            b.close();
        });
        self.frame.fill(&p, color(style.color()));
        Ok(())
    }

    fn draw_circle<S: BackendStyle>(
        &mut self,
        center: BackendCoord,
        radius: u32,
        style: &S,
        fill: bool,
    ) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        let p = Path::circle(point(center), radius as f32);
        if fill {
            self.frame.fill(&p, color(style.color()));
        } else {
            self.frame
                .stroke(&p, line_stroke(style.color(), style.stroke_width()));
        }
        Ok(())
    }

    fn draw_text<S: BackendTextStyle>(
        &mut self,
        text: &str,
        style: &S,
        pos: BackendCoord,
    ) -> Result<(), DrawingErrorKind<Self::ErrorType>> {
        self.frame.fill_text(Text {
            content: text.to_string(),
            position: point(pos),
            color: color(style.color()),
            size: (style.size() as f32).into(),
            ..Text::default()
        });
        Ok(())
    }

    fn estimate_text_size<S: BackendTextStyle>(
        &self,
        text: &str,
        style: &S,
    ) -> Result<(u32, u32), DrawingErrorKind<Self::ErrorType>> {
        // No font metrics on this path (we don't pull font-kit); approximate from the point
        // size. Good enough for axis-label layout, which only needs rough extents.
        let s = style.size();
        Ok(((text.len() as f64 * s * 0.6).ceil() as u32, s.ceil() as u32))
    }
}
