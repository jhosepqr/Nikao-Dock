//! The verse of the day, written on the menu bar itself between the name of
//! the app in front and the island.
//!
//! It runs as a marquee: the whole verse and its reference travel from right
//! to left along that stretch of the bar, appearing out of a soft edge on the
//! right and dissolving into one on the left. When it has gone by, it starts
//! again a moment later.
//!
//! Cost: the verse is one bitmap drawn once. The compositor moves it with a
//! single keyframe animation and keeps the soft edges in place with an
//! expression, so this process does no per-frame work; it wakes once per
//! pass to start the next one.

use super::{IslandGeometry, Look, Span};
use crate::gfx::Gfx;
use anyhow::Result;
use muelle_core::verse::Verse;
use std::time::{Duration, Instant};
use windows::{
    Foundation::TimeSpan,
    UI::{
        Color,
        Composition::{
            CompositionAnimation, CompositionMappingMode, ContainerVisual, SpriteVisual,
        },
    },
    core::{Interface, h},
};
use windows_numerics::{Vector2, Vector3};

/// Below this, in logical pixels, there is no room for a readable stretch.
const MIN_WIDTH: f32 = 150.0;
/// How far the soft edges at each end reach into the stretch.
const EDGE: f32 = 56.0;
/// Travel speed, in logical pixels per second: an easy reading pace.
const SPEED: f32 = 42.0;
/// Pause between one pass and the next, once the last letter has left.
pub(super) const PAUSE: Duration = Duration::from_secs(3);

/// What the verse was laid out for; anything else changing keeps it, and its
/// animation, running.
#[derive(Clone, PartialEq)]
pub(super) struct Key {
    verse: Verse,
    region: (i32, i32),
    bar_height: i32,
    scale: f32,
    light: bool,
}

impl Key {
    pub(super) fn new(look: &Look, verse: Verse, region: Span, bar_height: f32) -> Self {
        Self {
            verse,
            region: (region.left.round() as i32, region.right.round() as i32),
            bar_height: bar_height.round() as i32,
            scale: look.scale,
            light: look.light,
        }
    }
}

pub(super) struct Strip {
    pub(super) key: Key,
    pub(super) visual: ContainerVisual,
    sprite: SpriteVisual,
    glide: CompositionAnimation,
    /// How long one pass takes, from entering on the right to leaving on the left.
    pub(super) pass: Duration,
    started: Option<Instant>,
}

impl Strip {
    /// `None` when `region` is too narrow for the verse. The verse does not
    /// move until `play`.
    pub(super) fn build(gfx: &Gfx, look: &Look, key: Key, region: Span) -> Result<Option<Self>> {
        let c = &gfx.compositor;
        let scale = look.scale;
        let px = |logical: f32| (logical * scale).round();
        let width = region.width().round();
        if width < px(MIN_WIDTH) {
            return Ok(None);
        }
        let bar_height = key.bar_height as f32;
        let Verse { reference, text } = key.verse;
        let ink = IslandGeometry::new(Span::default(), 0.0, look).ink();
        let label = gfx.verse_brush(text, Some(&format!("— {reference}")), scale, ink)?;
        let y = ((bar_height - label.height) / 2.0).round();

        // The stretch of the bar the verse travels along. It clips, so the
        // letters exist only between its ends.
        let strip = c.CreateContainerVisual()?;
        strip.SetSize(Vector2::new(width, bar_height))?;
        strip.SetOffset(Vector3::new(region.left.round(), 0.0, 0.0))?;
        strip.SetClip(&c.CreateInsetClip()?)?;

        let sprite = c.CreateSpriteVisual()?;
        sprite.SetSize(Vector2::new(label.width, label.height))?;
        // Waiting just past the right end until the first pass.
        sprite.SetOffset(Vector3::new(width, y, 0.0))?;

        // The soft edges belong to the stretch, not to the moving text: the
        // gradient is laid out across the stretch and shifted back by however
        // far the text has moved, so its edges stay put on the bar.
        let edge = (px(EDGE) / width).min(0.3);
        let gradient = c.CreateLinearGradientBrush()?;
        gradient.SetMappingMode(CompositionMappingMode::Absolute)?;
        gradient.SetStartPoint(Vector2::new(0.0, 0.0))?;
        gradient.SetEndPoint(Vector2::new(width, 0.0))?;
        let stops = gradient.ColorStops()?;
        for (offset, color) in [
            (0.0, CLEAR),
            (edge, SOLID),
            (1.0 - edge, SOLID),
            (1.0, CLEAR),
        ] {
            stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(offset, color)?)?;
        }
        let follow = c.CreateExpressionAnimationWithExpression(h!("Vector2(-text.Offset.X, 0)"))?;
        follow.SetReferenceParameter(h!("text"), &sprite)?;
        gradient.StartAnimation(h!("Offset"), &follow)?;

        let mask = c.CreateMaskBrush()?;
        mask.SetSource(&label.brush)?;
        mask.SetMask(&gradient)?;
        sprite.SetBrush(&mask)?;
        strip.Children()?.InsertAtTop(&sprite)?;

        // Right to left at a steady pace, from just past the right end until
        // the reference has left on the left.
        let seconds = (width + label.width) / px(SPEED).max(1.0);
        let glide = c.CreateVector3KeyFrameAnimation()?;
        glide.SetDuration(TimeSpan {
            Duration: (f64::from(seconds) * 1e7) as i64,
        })?;
        let steady = c.CreateLinearEasingFunction()?;
        glide.InsertKeyFrameWithEasingFunction(0.0, Vector3::new(width, y, 0.0), &steady)?;
        glide.InsertKeyFrameWithEasingFunction(1.0, Vector3::new(-label.width, y, 0.0), &steady)?;

        Ok(Some(Self {
            key,
            visual: strip,
            sprite,
            glide: glide.cast()?,
            pass: Duration::from_secs_f32(seconds),
            started: None,
        }))
    }

    /// Starts a pass from the right end.
    pub(super) fn play(&mut self) -> Result<()> {
        self.sprite.StartAnimation(h!("Offset"), &self.glide)?;
        self.started = Some(Instant::now());
        Ok(())
    }

    /// Not moving: never started, or the last pass has gone by.
    pub(super) fn idle(&self) -> bool {
        self.started
            .is_none_or(|started| started.elapsed() >= self.pass)
    }
}

const CLEAR: Color = Color {
    A: 0,
    R: 255,
    G: 255,
    B: 255,
};
const SOLID: Color = Color {
    A: 255,
    R: 255,
    G: 255,
    B: 255,
};
