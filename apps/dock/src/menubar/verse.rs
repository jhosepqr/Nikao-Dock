//! The verse of the day, in a glass pill between the name of the app in
//! front and the island.
//!
//! The verse is cut into phrases that fit the pill. Each phrase appears
//! letter by letter from the left, stays long enough to read and then
//! dissolves, also from the left; the last one carries the reference. Then
//! the pill fades away until the next minute, when it plays again.
//!
//! Cost: none in this process while it plays. Every phrase is one bitmap
//! drawn once, masked by a gradient whose edges the compositor moves along
//! keyframe animations. The menu bar's minute timer, which already exists for
//! the clock, starts the next cycle. Between cycles nothing animates.

use super::{IslandGeometry, Look, Span, bubble_surface};
use crate::gfx::Gfx;
use anyhow::Result;
use muelle_core::verse::{self, LEAD_IN, Slot, Verse};
use std::time::{Duration, Instant};
use windows::{
    Foundation::TimeSpan,
    UI::{
        Color,
        Composition::{
            CompositionAnimation, CompositionEasingFunction, CompositionMappingMode,
            CompositionObject, Compositor, ContainerVisual,
        },
    },
    core::{HSTRING, Interface, h},
};
use windows_numerics::{Vector2, Vector3};

/// Space between the text and the ends of the pill, in logical pixels.
const PADDING: f32 = 14.0;
/// Longer lines are hard to read at a glance, even with room for them.
const MAX_TEXT_WIDTH: f32 = 520.0;
/// Below this there is no room for a phrase worth reading.
const MIN_TEXT_WIDTH: f32 = 150.0;
/// The soft edge of the letters appearing and dissolving.
const EDGE: f32 = 34.0;
/// How far each phrase drifts left while it appears and dissolves.
const DRIFT: f32 = 7.0;

/// What the pill was built for; anything else changing keeps it, and its
/// animation, running.
#[derive(Clone, PartialEq)]
pub(super) struct Key {
    verse: Verse,
    region: (i32, i32),
    bar_height: i32,
    scale: f32,
    surface: crate::appearance::Look,
}

impl Key {
    pub(super) fn new(look: &Look, verse: Verse, region: Span, bar_height: f32) -> Self {
        Self {
            verse,
            region: (region.left.round() as i32, region.right.round() as i32),
            bar_height: bar_height.round() as i32,
            scale: look.scale,
            surface: look.surface(),
        }
    }
}

pub(super) struct Strip {
    pub(super) key: Key,
    pub(super) visual: ContainerVisual,
    /// Every animation of a cycle with what it drives, to start them again.
    animations: Vec<(CompositionObject, &'static HSTRING, CompositionAnimation)>,
    cycle: Duration,
    started: Instant,
}

impl Strip {
    /// `None` when `region` is too narrow for the verse.
    pub(super) fn build(gfx: &Gfx, look: &Look, key: Key, region: Span) -> Result<Option<Self>> {
        let c = &gfx.compositor;
        let scale = look.scale;
        let px = |logical: f32| (logical * scale).round();
        let bar_height = key.bar_height as f32;
        let padding = px(PADDING);
        let max_text = (region.width() - 2.0 * padding).min(px(MAX_TEXT_WIDTH));
        if max_text < px(MIN_TEXT_WIDTH) {
            return Ok(None);
        }
        let Verse { reference, text } = key.verse;
        let reference_width =
            gfx.verse_width("·", Some(reference), scale)? - gfx.verse_width("·", None, scale)?;
        let phrases = verse::phrases(text, reference_width, max_text, |phrase| {
            gfx.verse_width(phrase, None, scale)
                .unwrap_or(f32::INFINITY)
        });
        let geometry = IslandGeometry::new(Span::default(), 0.0, look);
        let labels = phrases
            .iter()
            .map(|phrase| {
                gfx.verse_brush(
                    &phrase.text,
                    phrase.with_reference.then_some(reference),
                    scale,
                    geometry.ink(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let widest = labels.iter().map(|label| label.width).fold(0.0, f32::max);
        let width = (widest + 2.0 * padding).min(region.width()).round();
        let left = (region.left + (region.width() - width) / 2.0).round();

        let strip = c.CreateContainerVisual()?;
        strip.SetSize(Vector2::new(width, bar_height))?;
        strip.SetOffset(Vector3::new(left, 0.0, 0.0))?;
        strip.SetOpacity(0.0)?;
        let pill = Span {
            left: 0.0,
            right: width,
        };
        let (content, pill_height) = bubble_surface(
            gfx,
            look,
            &strip.Children()?,
            IslandGeometry::new(pill, 0.0, look),
            bar_height,
        )?;

        let lengths: Vec<usize> = phrases
            .iter()
            .map(|phrase| {
                phrase.text.chars().count()
                    + if phrase.with_reference {
                        reference.chars().count()
                    } else {
                        0
                    }
            })
            .collect();
        let (slots, seconds) = verse::schedule(&lengths);
        let timing = Timing::new(c, seconds)?;
        let mut animations = Vec::new();

        let fade = timing.scalar(&[
            (0.0, 0.0),
            (LEAD_IN, 1.0),
            (slots.last().map_or(LEAD_IN, |slot| slot.end), 1.0),
            (seconds, 0.0),
        ])?;
        animations.push((strip.cast()?, h!("Opacity"), fade));

        let edge = px(EDGE);
        let drift = px(DRIFT);
        for (label, slot) in labels.iter().zip(&slots) {
            let x = ((width - label.width) / 2.0).round();
            let y = ((pill_height - label.height) / 2.0).round();
            let sprite = c.CreateSpriteVisual()?;
            sprite.SetSize(Vector2::new(label.width, label.height))?;
            sprite.SetOffset(Vector3::new(x + drift, y, 0.0))?;
            let wipe = Wipe::new(label.width, edge);
            let gradient = c.CreateLinearGradientBrush()?;
            gradient.SetMappingMode(CompositionMappingMode::Absolute)?;
            gradient.SetStartPoint(Vector2::new(-2.0 * edge, 0.0))?;
            gradient.SetEndPoint(Vector2::new(label.width + 2.0 * edge, 0.0))?;
            let stops = gradient.ColorStops()?;
            // Tail out, tail in, head in, head out: the letters between the
            // two inner stops show, the soft edges around them fade. Each
            // stop is `behind` its edge's position by one soft edge or not.
            for (color, side, behind) in [
                (CLEAR, Edge::Tail, true),
                (SOLID, Edge::Tail, false),
                (SOLID, Edge::Head, true),
                (CLEAR, Edge::Head, false),
            ] {
                let lag = if behind { wipe.edge } else { 0.0 };
                let keys = side
                    .keys(&wipe, slot, seconds)
                    .map(|(time, position)| (time, wipe.at(position - lag)));
                let stop = c.CreateColorGradientStopWithOffsetAndColor(keys[0].1, color)?;
                stops.Append(&stop)?;
                animations.push((stop.cast()?, h!("Offset"), timing.scalar(&keys)?));
            }
            let mask = c.CreateMaskBrush()?;
            mask.SetSource(&label.brush)?;
            mask.SetMask(&gradient)?;
            sprite.SetBrush(&mask)?;
            content.InsertAtTop(&sprite)?;

            let glide = c.CreateVector3KeyFrameAnimation()?;
            glide.SetDuration(timing.duration)?;
            for (time, dx) in [
                (0.0, drift),
                (slot.start, drift),
                (slot.shown, 0.0),
                (slot.fading, 0.0),
                (slot.end, -drift),
                (seconds, -drift),
            ] {
                glide.InsertKeyFrameWithEasingFunction(
                    timing.progress(time),
                    Vector3::new(x + dx, y, 0.0),
                    &timing.gentle,
                )?;
            }
            animations.push((sprite.cast()?, h!("Offset"), glide.cast()?));
        }

        let mut strip = Self {
            key,
            visual: strip,
            animations,
            cycle: Duration::from_secs_f32(seconds),
            started: Instant::now(),
        };
        strip.restart()?;
        Ok(Some(strip))
    }

    /// Plays the verse from the start.
    pub(super) fn restart(&mut self) -> Result<()> {
        for (target, property, animation) in &self.animations {
            target.StartAnimation(property, animation)?;
        }
        self.started = Instant::now();
        Ok(())
    }

    /// The last cycle has played to its end.
    pub(super) fn finished(&self) -> bool {
        self.started.elapsed() >= self.cycle
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

/// Positions along a phrase, in its pixels, as gradient offsets. The
/// gradient runs two edges beyond each end so its soft edges can start and
/// finish fully outside the letters.
struct Wipe {
    width: f32,
    edge: f32,
}

impl Wipe {
    fn new(width: f32, edge: f32) -> Self {
        Self { width, edge }
    }

    fn at(&self, x: f32) -> f32 {
        ((x + 2.0 * self.edge) / (self.width + 4.0 * self.edge)).clamp(0.0, 1.0)
    }
}

/// The two moving edges of a phrase.
#[derive(Clone, Copy)]
enum Edge {
    /// Leads the letters in: everything left of it shows.
    Head,
    /// Follows them out: everything left of it is gone.
    Tail,
}

impl Edge {
    /// Where the edge is (in the phrase's pixels) over the cycle.
    fn keys(self, wipe: &Wipe, slot: &Slot, seconds: f32) -> [(f32, f32); 4] {
        match self {
            // Before: just left of the letters, so none show. After: past the
            // right end, so all do.
            Self::Head => {
                let (before, after) = (0.0, wipe.width + 2.0 * wipe.edge);
                [
                    (0.0, before),
                    (slot.start, before),
                    (slot.shown, after),
                    (seconds, after),
                ]
            }
            Self::Tail => {
                let (before, after) = (-wipe.edge, wipe.width + wipe.edge);
                [
                    (0.0, before),
                    (slot.fading, before),
                    (slot.end, after),
                    (seconds, after),
                ]
            }
        }
    }
}

struct Timing {
    compositor: Compositor,
    seconds: f32,
    duration: TimeSpan,
    /// Letters pass at an even pace.
    steady: CompositionEasingFunction,
    gentle: CompositionEasingFunction,
}

impl Timing {
    fn new(compositor: &Compositor, seconds: f32) -> Result<Self> {
        Ok(Self {
            compositor: compositor.clone(),
            seconds,
            duration: TimeSpan {
                Duration: (f64::from(seconds) * 1e7) as i64,
            },
            steady: compositor.CreateLinearEasingFunction()?.cast()?,
            gentle: compositor
                .CreateCubicBezierEasingFunction(Vector2::new(0.4, 0.0), Vector2::new(0.2, 1.0))?
                .cast()?,
        })
    }

    fn progress(&self, time: f32) -> f32 {
        (time / self.seconds).clamp(0.0, 1.0)
    }

    fn scalar(&self, keys: &[(f32, f32)]) -> Result<CompositionAnimation> {
        let animation = self.compositor.CreateScalarKeyFrameAnimation()?;
        animation.SetDuration(self.duration)?;
        for &(time, value) in keys {
            animation.InsertKeyFrameWithEasingFunction(self.progress(time), value, &self.steady)?;
        }
        Ok(animation.cast()?)
    }
}
