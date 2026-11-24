use crate::Point;

const SPACING_RATIO: f32 = 0.1;
const MIN_SPACING: f32 = 0.5;
const MIN_DIAMETER: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush {
    pub diameter: f32,
    pub hardness: f32,
    pub flow: f32,
    pub color: [f32; 3],
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            diameter: 64.0,
            hardness: 0.5,
            flow: 1.0,
            color: [0.0; 3],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokePoint {
    pub position: Point,
    pub pressure: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dab {
    pub center: Point,
    pub radius: f32,
    pub hardness: f32,
    pub color: [f32; 3],
    pub alpha: f32,
}

#[derive(Clone, Debug)]
pub struct Stroke {
    brush: Brush,
    last: StrokePoint,
    until_next_dab: f32,
}

impl Stroke {
    pub fn begin(brush: Brush, start: StrokePoint) -> (Self, Dab) {
        let mut stroke = Self {
            brush,
            last: start,
            until_next_dab: 0.0,
        };
        stroke.until_next_dab = stroke.spacing(start.pressure);
        let dab = stroke.dab(start);
        (stroke, dab)
    }

    pub fn extend(&mut self, next: StrokePoint, dabs: &mut Vec<Dab>) {
        let dx = next.position.x - self.last.position.x;
        let dy = next.position.y - self.last.position.y;
        let length = dx.hypot(dy);
        if !length.is_finite() {
            return;
        }

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
        (self.brush.diameter * pressure.clamp(0.0, 1.0)).max(MIN_DIAMETER)
    }

    fn spacing(&self, pressure: f32) -> f32 {
        (self.diameter(pressure) * SPACING_RATIO).max(MIN_SPACING)
    }

    fn dab(&self, point: StrokePoint) -> Dab {
        Dab {
            center: point.position,
            radius: self.diameter(point.pressure) / 2.0,
            hardness: self.brush.hardness,
            color: self.brush.color,
            alpha: self.brush.flow,
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
    fn non_finite_input_is_ignored() {
        assert_eq!(stroke_through(&[at(0.0, 0.0), at(f32::NAN, 0.0)]).len(), 1);
    }
}
