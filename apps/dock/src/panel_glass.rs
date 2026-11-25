//! Composition glass for the search and battery panels.
//!
//! The visual tree is built when a panel opens and released when it closes.
//! Nothing here animates, and nothing is retained while the panel is hidden.
//! Colours come from [`crate::panel_theme::GlassTokens`].

use anyhow::Result;
use windows::{
    UI::{
        Color,
        Composition::{Desktop::DesktopWindowTarget, VisualCollection},
    },
    Win32::{Foundation::HWND, System::WinRT::Composition::ICompositorDesktopInterop},
    core::Interface,
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    gfx::Gfx,
    menu,
    panel_theme::{GlassTokens, Rgba},
};

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
        let root = compositor.CreateContainerVisual()?;
        root.SetSize(Vector2::new(canvas.width, canvas.height))?;
        let layers = root.Children()?;
        menu::draw_tinted_background(
            &layers,
            ui_color(canvas.tokens.tint),
            canvas.tokens.glass,
            canvas.width,
            canvas.height,
        )?;
        let surface = Surface {
            gfx,
            layers: &layers,
            canvas,
        };
        draw(&surface)?;
        if canvas.tokens.glass {
            let thickness = canvas.scale.round().max(1.0);
            surface.stroke(
                canvas.tokens.rim,
                Round {
                    x: 0.0,
                    y: 0.0,
                    w: canvas.width,
                    h: canvas.height,
                    radius: 8.0 * canvas.scale,
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
    pub fn text(&self, text: &str, line: Line, font: f32, bold: bool, color: Rgba) -> Result<()> {
        if text.is_empty() || line.max_width < 1.0 {
            return Ok(());
        }
        let brush = self.gfx.text_brush_colored(
            text,
            self.canvas.scale,
            font,
            bold,
            [color.r, color.g, color.b, color.a],
            Some(line.max_width),
        )?;
        let top = line.top + ((line.height - brush.height) / 2.0).round();
        menu::add_sprite(
            self.layers,
            brush.brush.cast()?,
            line.x,
            top,
            brush.width,
            brush.height,
        )
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

fn ui_color(color: Rgba) -> Color {
    Color {
        A: color.a,
        R: color.r,
        G: color.g,
        B: color.b,
    }
}
