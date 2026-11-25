use serde::{Deserialize, Serialize};

use crate::{Point, TILE_SIZE, TileCoord};

pub const MAX_BRUSH_DIAMETER: f32 = 4096.0;
pub const MAX_COORDINATE: f32 = 1e6;

const SPACING_RATIO: f32 = 0.1;
const MIN_SPACING: f32 = 0.5;
const MIN_DIAMETER: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Brush {
    pub diameter: f32,
    pub hardness: f32,
    pub flow: f32,
    pub opacity: f32,
    pub color: [f32; 3],
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            diameter: 64.0,
            hardness: 0.5,
            flow: 1.0,
            opacity: 1.0,
            color: [0.0; 3],
        }
    }
}

impl Brush {
    pub fn is_valid(&self) -> bool {
        let [red, green, blue] = self.color;
        (0.0..=MAX_BRUSH_DIAMETER).contains(&self.diameter)
            && [self.hardness, self.flow, self.opacity, red, green, blue]
                .iter()
                .all(|value| (0.0..=1.0).contains(value))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrushDynamics {
    pub pressure_curve: PressureCurve,
    pub stabilizer: f32,
}

impl BrushDynamics {
    pub fn is_valid(self) -> bool {
        (0.0..=0.95).contains(&self.stabilizer)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PressureCurve {
    #[default]
    Linear,
    Soft,
    Firm,
    Fixed,
}

impl PressureCurve {
    fn apply(self, value: f32) -> f32 {
        let value = value.clamp(0.0, 1.0);
        match self {
            Self::Linear => value,
            Self::Soft => value.sqrt(),
            Self::Firm => value * value,
            Self::Fixed => 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrushPreset {
    Pencil,
    Round,
    Airbrush,
}

impl BrushPreset {
    pub fn brush(self) -> Brush {
        let (diameter, hardness, flow) = match self {
            Self::Pencil => (5.0, 1.0, 1.0),
            Self::Round => (64.0, 0.85, 1.0),
            Self::Airbrush => (128.0, 0.0, 0.12),
        };
        Brush {
            diameter,
            hardness,
            flow,
            ..Brush::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct StrokePoint {
    #[serde(flatten)]
    pub position: Point,
    #[serde(default = "full_pressure")]
    pub pressure: f32,
}

fn full_pressure() -> f32 {
    1.0
}

impl StrokePoint {
    pub fn is_valid(&self) -> bool {
        let Point { x, y } = self.position;
        x.abs() <= MAX_COORDINATE && y.abs() <= MAX_COORDINATE && self.pressure.is_finite()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dab {
    pub center: Point,
    pub radius: f32,
    pub hardness: f32,
    pub flow: f32,
}

impl Dab {
    pub fn tiles(&self, (width, height): (u32, u32)) -> impl Iterator<Item = TileCoord> {
        let columns = tile_range(self.center.x, self.radius, width);
        let rows = tile_range(self.center.y, self.radius, height);
        rows.flat_map(move |y| columns.clone().map(move |x| TileCoord { x, y }))
            .filter(|coord| self.overlaps(*coord))
    }

    fn overlaps(&self, coord: TileCoord) -> bool {
        let (left, top) = coord.origin();
        let distance = |center: f32, start: u32| {
            let start = start as f32;
            center - center.clamp(start, start + TILE_SIZE as f32)
        };
        distance(self.center.x, left).hypot(distance(self.center.y, top)) < self.radius
    }
}

fn tile_range(center: f32, radius: f32, length: u32) -> std::ops::Range<u32> {
    let start = (center - radius).floor().max(0.0);
    let end = (center + radius).ceil().min(length as f32);
    if center.is_finite() && start < end {
        start as u32 / TILE_SIZE..(end as u32).div_ceil(TILE_SIZE)
    } else {
        0..0
    }
}

#[derive(Clone, Debug)]
pub struct Stroke {
    brush: Brush,
    dynamics: BrushDynamics,
    last: StrokePoint,
    input: StrokePoint,
    until_next_dab: f32,
}

impl Stroke {
    pub fn begin(brush: Brush, start: StrokePoint) -> (Self, Dab) {
        Self::with_dynamics(brush, BrushDynamics::default(), start)
    }

    pub fn with_dynamics(brush: Brush, dynamics: BrushDynamics, start: StrokePoint) -> (Self, Dab) {
        let start = StrokePoint {
            pressure: start.pressure.clamp(0.0, 1.0),
            ..start
        };
        let mut stroke = Self {
            brush,
            dynamics,
            last: start,
            input: start,
            until_next_dab: 0.0,
        };
        stroke.until_next_dab = stroke.spacing(start.pressure);
        let dab = stroke.dab(start);
        (stroke, dab)
    }

    pub fn extend(&mut self, next: StrokePoint, dabs: &mut Vec<Dab>) {
        if !next.is_valid() {
            return;
        }
        self.input = StrokePoint {
            pressure: next.pressure.clamp(0.0, 1.0),
            ..next
        };
        let strength = self.dynamics.stabilizer;
        let mix = |last: f32, next: f32| next + (last - next) * strength;
        self.line_to(
            StrokePoint {
                position: Point::new(
                    mix(self.last.position.x, next.position.x),
                    mix(self.last.position.y, next.position.y),
                ),
                pressure: mix(self.last.pressure, self.input.pressure),
            },
            dabs,
        );
    }

    pub fn complete(&mut self, dabs: &mut Vec<Dab>) {
        if self.last.position != self.input.position {
            self.line_to(self.input, dabs);
            if self.until_next_dab < self.spacing(self.input.pressure) {
                dabs.push(self.dab(self.input));
            }
        }
    }

    fn line_to(&mut self, next: StrokePoint, dabs: &mut Vec<Dab>) {
        let dx = next.position.x - self.last.position.x;
        let dy = next.position.y - self.last.position.y;
        let length = dx.hypot(dy);

        let mut travelled = self.until_next_dab;
        while travelled <= length {
            let t = travelled / length.max(f32::MIN_POSITIVE);
            let point = StrokePoint {
                position: Point::new(self.last.position.x + dx * t, self.last.position.y + dy * t),
                pressure: self.last.pressure + (next.pressure - self.last.pressure) * t,
            };
            dabs.push(self.dab(point));
            travelled += self.spacing(point.pressure);
        }
        self.until_next_dab = travelled - length;
        self.last = next;
    }

    fn diameter(&self, pressure: f32) -> f32 {
        (self.brush.diameter * self.dynamics.pressure_curve.apply(pressure)).max(MIN_DIAMETER)
    }

    fn spacing(&self, pressure: f32) -> f32 {
        (self.diameter(pressure) * SPACING_RATIO).max(MIN_SPACING)
    }

    fn dab(&self, point: StrokePoint) -> Dab {
        Dab {
            center: point.position,
            radius: self.diameter(point.pressure) / 2.0,
            hardness: self.brush.hardness,
            flow: self.brush.flow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32) -> StrokePoint {
        StrokePoint {
            position: Point::new(x, y),
            pressure: 1.0,
        }
    }

    fn stroke_through(points: &[StrokePoint]) -> Vec<Dab> {
        let brush = Brush {
            diameter: 100.0,
            ..Brush::default()
        };
        let (mut stroke, first) = Stroke::begin(brush, points[0]);
        let mut dabs = vec![first];
        for point in &points[1..] {
            stroke.extend(*point, &mut dabs);
        }
        dabs
    }

    fn tiles_under(x: f32, y: f32, radius: f32) -> Vec<(u32, u32)> {
        let dab = Dab {
            center: Point::new(x, y),
            radius,
            hardness: 1.0,
            flow: 1.0,
        };
        dab.tiles((600, 300)).map(Into::into).collect()
    }

    #[test]
    fn dabs_are_evenly_spaced() {
        let dabs = stroke_through(&[at(0.0, 0.0), at(100.0, 0.0)]);
        assert_eq!(dabs.len(), 11);
        for pair in dabs.windows(2) {
            assert!((pair[1].center.x - pair[0].center.x - 10.0).abs() < 1e-3);
        }
    }

    #[test]
    fn spacing_does_not_depend_on_input_sampling() {
        let coarse = stroke_through(&[at(0.0, 0.0), at(95.0, 0.0)]);
        let samples: Vec<_> = (0..=95).map(|x| at(x as f32, 0.0)).collect();
        let fine = stroke_through(&samples);
        assert_eq!(coarse.len(), fine.len());
        for (a, b) in coarse.iter().zip(&fine) {
            assert!((a.center.x - b.center.x).abs() < 1e-2);
        }
    }

    #[test]
    fn a_stationary_pointer_adds_no_dabs() {
        assert_eq!(stroke_through(&[at(5.0, 5.0), at(5.0, 5.0)]).len(), 1);
    }

    #[test]
    fn zero_pressure_still_advances() {
        let mut dabs = Vec::new();
        let start = StrokePoint {
            pressure: 0.0,
            ..at(0.0, 0.0)
        };
        let (mut stroke, first) = Stroke::begin(Brush::default(), start);
        stroke.extend(
            StrokePoint {
                pressure: 0.0,
                ..at(10.0, 0.0)
            },
            &mut dabs,
        );
        assert_eq!(first.radius, MIN_DIAMETER / 2.0);
        assert_eq!(dabs.len(), 20);
    }

    #[test]
    fn pressure_curves_change_diameter_and_stabilizing_preserves_the_endpoint() {
        let brush = Brush {
            diameter: 100.0,
            ..Brush::default()
        };
        let point = StrokePoint {
            pressure: 0.25,
            ..at(0.5, 0.5)
        };
        for (curve, radius) in [
            (PressureCurve::Linear, 12.5),
            (PressureCurve::Soft, 25.0),
            (PressureCurve::Firm, 3.125),
            (PressureCurve::Fixed, 50.0),
        ] {
            let (_, dab) = Stroke::with_dynamics(
                brush,
                BrushDynamics {
                    pressure_curve: curve,
                    stabilizer: 0.0,
                },
                point,
            );
            assert_eq!(dab.radius, radius);
        }
        let (mut stroke, _) = Stroke::with_dynamics(
            brush,
            BrushDynamics {
                stabilizer: 0.5,
                ..BrushDynamics::default()
            },
            at(0.0, 0.0),
        );
        let mut dabs = Vec::new();
        let end = at(101.0, 0.0);
        stroke.extend(end, &mut dabs);
        assert!(dabs.last().unwrap().center.x < end.position.x);
        stroke.complete(&mut dabs);
        assert_eq!(dabs.last().unwrap().center, end.position);
    }

    #[test]
    fn invalid_points_are_ignored() {
        for x in [f32::NAN, f32::INFINITY, MAX_COORDINATE * 2.0] {
            assert_eq!(stroke_through(&[at(0.0, 0.0), at(x, 0.0)]).len(), 1);
        }
    }

    #[test]
    fn a_dab_covers_every_tile_it_overlaps_inside_the_document() {
        assert_eq!(tiles_under(100.0, 100.0, 20.0), [(0, 0)]);
        assert_eq!(
            tiles_under(256.0, 256.0, 1.0),
            [(0, 0), (1, 0), (0, 1), (1, 1)]
        );
        assert_eq!(tiles_under(100.0, 100.0, 1e6).len(), 6);
        assert_eq!(tiles_under(250.0, 250.0, 7.0), [(0, 0), (1, 0), (0, 1)]);
        assert!(tiles_under(-30.0, 100.0, 20.0).is_empty());
        assert!(tiles_under(630.0, 100.0, 20.0).is_empty());
        assert!(tiles_under(f32::NAN, 100.0, 20.0).is_empty());
    }

    #[test]
    fn only_finite_brushes_in_range_are_valid() {
        assert!(Brush::default().is_valid());
        for diameter in [f32::NAN, f32::INFINITY, -1.0, MAX_BRUSH_DIAMETER * 2.0] {
            let brush = Brush {
                diameter,
                ..Brush::default()
            };
            assert!(!brush.is_valid());
        }
        let brush = Brush {
            opacity: f32::NAN,
            ..Brush::default()
        };
        assert!(!brush.is_valid());
    }
}
