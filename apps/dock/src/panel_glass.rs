//! Composition glass for the search and battery panels.
//!
//! The visual tree is built when a panel opens and released when it closes.
//! Nothing here animates, and nothing is retained while the panel is hidden.
//! Colours come from [`crate::panel_theme::GlassTokens`].

use anyhow::Result;
use windows::{
    UI::{
        Color,
        Composition::{CompositionColorSpace, Desktop::DesktopWindowTarget, VisualCollection},
    },
    Win32::{Foundation::HWND, System::WinRT::Composition::ICompositorDesktopInterop},
    core::Interface,
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    gfx::{Gfx, TextStyle},
    menu,
    panel_theme::{GlassTokens, Rgba},
};

/// Corner radius of Liquid Glass drop-down cards (logical pixels).
const CARD_RADIUS: f32 = 16.0;
/// Where the static top specular highlight fades out (fraction of card height).
const SHEEN_END: f32 = 0.18;

/// The composition target for one open panel. Dropping it releases the tree.
#[derive(Clone)]
pub struct Scene {
    target: DesktopWindowTarget,
}

/// Size and colours for one draw. The panel builds this from state it already has.
pub struct Canvas {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub tokens: GlassTokens,
}

pub struct Surface<'a> {
    gfx: &'a Gfx,
    layers: &'a VisualCollection,
    canvas: &'a Canvas,
}

#[derive(Clone, Copy)]
pub struct Round {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
}

#[derive(Clone, Copy)]
pub struct Line {
    pub x: f32,
    pub top: f32,
    pub height: f32,
    pub max_width: f32,
}

impl Scene {
    /// Attaches a target to `hwnd`. The window must use
    /// `WS_EX_NOREDIRECTIONBITMAP`, or the glass is covered by GDI.
    pub fn attach(gfx: &Gfx, hwnd: HWND) -> Result<Self> {
        // SAFETY: `hwnd` is our window. The compositor belongs to this thread.
        let target = unsafe {
            gfx.compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)?
        };
        Ok(Self { target })
    }

    /// Replaces the tree. The previous tree is released. No timer: the caller
    /// draws on open and when the contents change.
    pub fn paint(
        &self,
        gfx: &Gfx,
        canvas: &Canvas,
        draw: impl FnOnce(&Surface<'_>) -> Result<()>,
    ) -> Result<()> {
        if canvas.width < 1.0 || canvas.height < 1.0 {
            return Ok(());
        }
        let compositor = &gfx.compositor;
        let size = Vector2::new(canvas.width, canvas.height);
        let root = compositor.CreateContainerVisual()?;
        root.SetSize(size)?;
        let card = compositor.CreateContainerVisual()?;
        card.SetSize(size)?;
        let radius = (CARD_RADIUS * canvas.scale)
            .min(canvas.width / 2.0)
            .min(canvas.height / 2.0);
        if canvas.tokens.glass {
            let outline = compositor.CreateRoundedRectangleGeometry()?;
            outline.SetSize(size)?;
            outline.SetCornerRadius(Vector2::new(radius, radius))?;
            card.SetClip(&compositor.CreateGeometricClipWithGeometry(&outline)?)?;
        }
        let layers = card.Children()?;
        menu::draw_tinted_background(
            &layers,
            ui_color(canvas.tokens.tint),
            canvas.tokens.glass,
            canvas.tokens.backdrop_opacity(),
            canvas.width,
            canvas.height,
        )?;
        if canvas.tokens.glass && canvas.tokens.sheen.a > 0 {
            draw_specular_sheen(&layers, canvas.tokens.sheen, canvas.width, canvas.height)?;
        }
        let surface = Surface {
            gfx,
            layers: &layers,
            canvas,
        };
        draw(&surface)?;
        root.Children()?.InsertAtTop(&card)?;
        if canvas.tokens.glass {
            let thickness = canvas.scale.round().max(1.0);
            draw_diagonal_rim(
                &root.Children()?,
                canvas.tokens.rim_stops,
                Round {
                    x: 0.0,
                    y: 0.0,
                    w: canvas.width,
                    h: canvas.height,
                    radius,
                },
                thickness,
            )?;
        }
        self.target.SetRoot(&root)?;
        Ok(())
    }
}

impl Surface<'_> {
    #[must_use]
    pub fn tokens(&self) -> GlassTokens {
        self.canvas.tokens
    }

    /// One line, vertically centred in the row, elided to `line.max_width`.
    /// Returns the drawn width.
    pub fn text(&self, text: &str, line: Line, font: f32, bold: bool, color: Rgba) -> Result<f32> {
        if text.is_empty() || line.max_width < 1.0 {
            return Ok(0.0);
        }
        let shadow = self
            .canvas
            .tokens
            .text_shadow
            .map(|drop| [drop.r, drop.g, drop.b, drop.a]);
        let brush = self.gfx.text_brush_colored(
            text,
            self.canvas.scale,
            TextStyle {
                size: font,
                bold,
                color: [color.r, color.g, color.b, color.a],
                shadow,
                max_width: Some(line.max_width),
            },
        )?;
        let top = line.top + ((line.height - brush.height) / 2.0).round();
        menu::add_sprite(
            self.layers,
            brush.brush.cast()?,
            line.x,
            top,
            brush.width,
            brush.height,
        )?;
        Ok(brush.width)
    }

    /// Width `text` would take, without drawing it.
    pub fn measure(&self, text: &str, font: f32, bold: bool) -> Result<f32> {
        if text.is_empty() {
            return Ok(0.0);
        }
        let brush = self.gfx.text_brush_colored(
            text,
            self.canvas.scale,
            TextStyle {
                size: font,
                bold,
                color: [0, 0, 0, 255],
                shadow: None,
                max_width: None,
            },
        )?;
        Ok(brush.width)
    }

    pub fn fill(&self, color: Rgba, round: Round) -> Result<()> {
        if round.w < 1.0 || round.h < 1.0 {
            return Ok(());
        }
        let compositor = &self.gfx.compositor;
        let geometry = compositor.CreateRoundedRectangleGeometry()?;
        geometry.SetSize(Vector2::new(round.w, round.h))?;
        let radius = round.radius.min(round.w / 2.0).min(round.h / 2.0).max(0.0);
        geometry.SetCornerRadius(Vector2::new(radius, radius))?;
        let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
        shape.SetFillBrush(&compositor.CreateColorBrushWithColor(ui_color(color))?)?;
        let visual = compositor.CreateShapeVisual()?;
        visual.SetOffset(Vector3::new(round.x, round.y, 0.0))?;
        visual.SetSize(Vector2::new(round.w, round.h))?;
        visual.Shapes()?.Append(&shape)?;
        self.layers.InsertAtTop(&visual)?;
        Ok(())
    }

    pub fn stroke(&self, color: Rgba, round: Round, thickness: f32) -> Result<()> {
        if round.w <= thickness || round.h <= thickness {
            return Ok(());
        }
        let compositor = &self.gfx.compositor;
        let geometry = compositor.CreateRoundedRectangleGeometry()?;
        geometry.SetOffset(Vector2::new(thickness / 2.0, thickness / 2.0))?;
        geometry.SetSize(Vector2::new(round.w - thickness, round.h - thickness))?;
        let radius = (round.radius - thickness / 2.0).max(0.0);
        geometry.SetCornerRadius(Vector2::new(radius, radius))?;
        let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
        shape.SetStrokeBrush(&compositor.CreateColorBrushWithColor(ui_color(color))?)?;
        shape.SetStrokeThickness(thickness)?;
        let visual = compositor.CreateShapeVisual()?;
        visual.SetOffset(Vector3::new(round.x, round.y, 0.0))?;
        visual.SetSize(Vector2::new(round.w, round.h))?;
        visual.Shapes()?.Append(&shape)?;
        self.layers.InsertAtTop(&visual)?;
        Ok(())
    }
}

fn draw_specular_sheen(
    layers: &VisualCollection,
    sheen: Rgba,
    width: f32,
    height: f32,
) -> Result<()> {
    let compositor = layers.Compositor()?;
    let brush = compositor.CreateLinearGradientBrush()?;
    brush.SetStartPoint(Vector2::new(0.0, 0.0))?;
    brush.SetEndPoint(Vector2::new(0.0, 1.0))?;
    let stops = brush.ColorStops()?;
    stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(0.0, ui_color(sheen))?)?;
    stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(
        SHEEN_END * 0.3,
        ui_color(Rgba {
            a: sheen.a / 4,
            ..sheen
        }),
    )?)?;
    stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(
        SHEEN_END,
        ui_color(Rgba { a: 0, ..sheen }),
    )?)?;
    menu::add_sprite(layers, brush.cast()?, 0.0, 0.0, width, height)
}

fn draw_diagonal_rim(
    layers: &VisualCollection,
    rim_stops: [u8; 4],
    round: Round,
    thickness: f32,
) -> Result<()> {
    if round.w <= thickness || round.h <= thickness {
        return Ok(());
    }
    let compositor = layers.Compositor()?;
    let brush = compositor.CreateLinearGradientBrush()?;
    brush.SetStartPoint(Vector2::new(0.0, 0.0))?;
    brush.SetEndPoint(Vector2::new(1.0, 1.0))?;
    brush.SetInterpolationSpace(CompositionColorSpace::RgbLinear)?;
    let stops = brush.ColorStops()?;
    for (at, alpha) in [0.0, 0.35, 0.65, 1.0].into_iter().zip(rim_stops) {
        stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(
            at,
            Color {
                A: alpha,
                R: 255,
                G: 255,
                B: 255,
            },
        )?)?;
    }
    let geometry = compositor.CreateRoundedRectangleGeometry()?;
    geometry.SetOffset(Vector2::new(thickness / 2.0, thickness / 2.0))?;
    geometry.SetSize(Vector2::new(round.w - thickness, round.h - thickness))?;
    let radius = (round.radius - thickness / 2.0).max(0.0);
    geometry.SetCornerRadius(Vector2::new(radius, radius))?;
    let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
    shape.SetStrokeBrush(&brush)?;
    shape.SetStrokeThickness(thickness)?;
    let visual = compositor.CreateShapeVisual()?;
    visual.SetOffset(Vector3::new(round.x, round.y, 0.0))?;
    visual.SetSize(Vector2::new(round.w, round.h))?;
    visual.Shapes()?.Append(&shape)?;
    layers.InsertAtTop(&visual)?;
    Ok(())
}

fn ui_color(color: Rgba) -> Color {
    Color {
        A: color.a,
        R: color.r,
        G: color.g,
        B: color.b,
    }
}
