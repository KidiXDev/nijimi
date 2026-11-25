use std::f32::consts::TAU;

use serde::Deserialize;

use crate::{MAX_COORDINATE, PixelRect, Point, TileCoord};

pub const MAX_SELECTION_RADIUS: u32 = 500;

const SUBROWS: u32 = 8;
const HALF: u8 = 128;
const UNREACHED: f32 = 1e20;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SelectionShape {
    Rectangle {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    Ellipse {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    Lasso {
        points: Vec<Point>,
    },
}

impl SelectionShape {
    fn outline(&self) -> Vec<Point> {
        match self {
            &Self::Rectangle {
                x,
                y,
                width,
                height,
            } => vec![
                Point::new(x, y),
                Point::new(x + width, y),
                Point::new(x + width, y + height),
                Point::new(x, y + height),
            ],
            &Self::Ellipse {
                x,
                y,
                width,
                height,
            } => {
                let (rx, ry) = (width / 2.0, height / 2.0);
                let segments = (rx.abs().max(ry.abs()).sqrt() * 16.0).clamp(32.0, 2048.0) as usize;
                (0..segments)
                    .map(|index| {
                        let (sin, cos) = (index as f32 / segments as f32 * TAU).sin_cos();
                        Point::new(x + rx + rx * cos, y + ry + ry * sin)
                    })
                    .collect()
            }
            Self::Lasso { points } => points.clone(),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.outline()
            .iter()
            .all(|point| point.x.abs() <= MAX_COORDINATE && point.y.abs() <= MAX_COORDINATE)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionMode {
    #[default]
    Replace,
    Add,
    Subtract,
    Intersect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    bounds: PixelRect,
    alpha: Vec<u8>,
}

impl Selection {
    pub fn new(bounds: PixelRect, alpha: Vec<u8>) -> Option<Self> {
        assert_eq!(
            alpha.len(),
            bounds.width as usize * bounds.height as usize,
            "one alpha byte per pixel"
        );
        if alpha.is_empty() {
            return None;
        }
        let rows = || alpha.chunks_exact(bounds.width as usize);
        let used = |row: &[u8]| row.iter().any(|value| *value > 0);
        let top = rows().position(used)? as u32;
        let bottom = rows().rposition(used)? as u32 + 1;
        let left = rows().filter_map(|row| row.iter().position(|value| *value > 0));
        let left = left.min()? as u32;
        let right = rows().filter_map(|row| row.iter().rposition(|value| *value > 0));
        let right = right.max()? as u32 + 1;
        let trimmed = PixelRect {
            x: bounds.x + left,
            y: bounds.y + top,
            width: right - left,
            height: bottom - top,
        };
        if trimmed == bounds {
            return Some(Self { bounds, alpha });
        }
        let alpha = rows()
            .skip(top as usize)
            .take(trimmed.height as usize)
            .flat_map(|row| &row[left as usize..right as usize])
            .copied()
            .collect();
        Some(Self {
            bounds: trimmed,
            alpha,
        })
    }

    pub fn from_fn(bounds: PixelRect, mut value: impl FnMut(u32, u32) -> u8) -> Option<Self> {
        let mut alpha = Vec::with_capacity(bounds.width as usize * bounds.height as usize);
        for y in bounds.y..bounds.y + bounds.height {
            alpha.extend((bounds.x..bounds.x + bounds.width).map(|x| value(x, y)));
        }
        Self::new(bounds, alpha)
    }

    pub fn from_shape(shape: &SelectionShape, size: (u32, u32)) -> Option<Self> {
        let outline = shape.outline();
        let extent = |axis: fn(&Point) -> f32, limit: u32| {
            let values = || outline.iter().map(axis);
            let start = (values().fold(f32::INFINITY, f32::min).floor() as u32).min(limit);
            let end = (values().fold(f32::NEG_INFINITY, f32::max).ceil() as u32).min(limit);
            (start, end.saturating_sub(start))
        };
        let (x, width) = extent(|point| point.x, size.0);
        let (y, height) = extent(|point| point.y, size.1);
        let bounds = PixelRect {
            x,
            y,
            width,
            height,
        };
        if width == 0 || height == 0 {
            return None;
        }
        let mut coverage = vec![0.0f32; width as usize * height as usize];
        for crossings in crossings(&outline, y, height).chunk_by(|a, b| a.subrow == b.subrow) {
            let row = (crossings[0].subrow / SUBROWS) as usize * width as usize;
            let row = &mut coverage[row..row + width as usize];
            let mut winding = 0;
            let mut start = 0.0;
            for crossing in crossings {
                if winding == 0 {
                    start = crossing.x;
                }
                winding += crossing.winding;
                if winding == 0 {
                    cover(row, start - x as f32, crossing.x - x as f32);
                }
            }
        }
        let alpha = coverage
            .into_iter()
            .map(|value| (value / SUBROWS as f32 * 255.0).round() as u8)
            .collect();
        Self::new(bounds, alpha)
    }

    pub fn bounds(&self) -> PixelRect {
        self.bounds
    }

    pub fn alpha(&self) -> &[u8] {
        &self.alpha
    }

    pub fn bytes(&self) -> u64 {
        self.alpha.len() as u64
    }

    pub fn value(&self, x: u32, y: u32) -> u8 {
        let PixelRect {
            x: left,
            y: top,
            width,
            height,
        } = self.bounds;
        let (Some(x), Some(y)) = (x.checked_sub(left), y.checked_sub(top)) else {
            return 0;
        };
        if x >= width || y >= height {
            return 0;
        }
        self.alpha[y as usize * width as usize + x as usize]
    }

    pub fn touches(&self, coord: TileCoord) -> bool {
        let (left, top) = coord.origin();
        let tile = PixelRect {
            x: left,
            y: top,
            width: crate::TILE_SIZE,
            height: crate::TILE_SIZE,
        };
        self.bounds.overlaps(tile)
    }

    pub fn to_canvas(&self, (width, height): (u32, u32)) -> Vec<u8> {
        let mut canvas = vec![0; width as usize * height as usize];
        let length = self.bounds.width as usize;
        for (row, values) in self.alpha.chunks_exact(length).enumerate() {
            let start = (self.bounds.y as usize + row) * width as usize + self.bounds.x as usize;
            canvas[start..start + length].copy_from_slice(values);
        }
        canvas
    }

    pub fn translated(&self, (dx, dy): (i32, i32), size: (u32, u32)) -> Option<Self> {
        let shifted = |start: u32, offset: i32, length: u32, limit: u32| {
            let start = i64::from(start) + i64::from(offset);
            let end = (start + i64::from(length)).clamp(0, i64::from(limit));
            let start = start.clamp(0, i64::from(limit));
            (start as u32, (end - start) as u32)
        };
        let (x, width) = shifted(self.bounds.x, dx, self.bounds.width, size.0);
        let (y, height) = shifted(self.bounds.y, dy, self.bounds.height, size.1);
        let bounds = PixelRect {
            x,
            y,
            width,
            height,
        };
        Self::from_fn(bounds, |x, y| {
            self.value(x.wrapping_sub_signed(dx), y.wrapping_sub_signed(dy))
        })
    }

    pub fn combined(&self, next: &Self, mode: SelectionMode) -> Option<Self> {
        let pixels = |bounds, combine: fn(u8, u8) -> u8| {
            Self::from_fn(bounds, |x, y| combine(self.value(x, y), next.value(x, y)))
        };
        match mode {
            SelectionMode::Replace => Some(next.clone()),
            SelectionMode::Add => pixels(self.bounds.union(next.bounds), u8::max),
            SelectionMode::Subtract => pixels(self.bounds, |a, b| a.min(255 - b)),
            SelectionMode::Intersect => pixels(self.bounds, u8::min),
        }
    }

    pub fn inverted(&self, size: (u32, u32)) -> Option<Self> {
        Self::from_fn(PixelRect::full(size), |x, y| 255 - self.value(x, y))
    }

    pub fn feathered(&self, radius: u32, size: (u32, u32)) -> Option<Self> {
        let region = self.bounds.grown(radius, size);
        let (width, height) = (region.width as usize, region.height as usize);
        let mut values: Vec<u16> = self.sampled(region).map(|value| value << 8).collect();
        for pass in 0..3 {
            let radius = ((radius + pass) / 3) as usize;
            if radius > 0 {
                values = blurred_rows_transposed(&values, width, radius);
                values = blurred_rows_transposed(&values, height, radius);
            }
        }
        let alpha = values
            .into_iter()
            .map(|value| ((value + 128) >> 8) as u8)
            .collect();
        Self::new(region, alpha)
    }

    pub fn expanded(&self, radius: u32, size: (u32, u32)) -> Option<Self> {
        let region = self.bounds.grown(radius, size);
        let mut alpha: Vec<u8> = self.sampled(region).map(|value| value as u8).collect();
        expand(&mut alpha, region.size(), radius);
        Self::new(region, alpha)
    }

    pub fn contracted(&self, radius: u32, size: (u32, u32)) -> Option<Self> {
        let region = self.bounds.grown(1, size);
        let mut outside: Vec<u8> = self
            .sampled(region)
            .map(|value| 255 - value as u8)
            .collect();
        expand(&mut outside, region.size(), radius);
        let alpha = outside.into_iter().map(|value| 255 - value).collect();
        Self::new(region, alpha)
    }

    fn sampled(&self, region: PixelRect) -> impl Iterator<Item = u16> {
        (region.y..region.y + region.height).flat_map(move |y| {
            (region.x..region.x + region.width).map(move |x| u16::from(self.value(x, y)))
        })
    }
}

struct Crossing {
    subrow: u32,
    x: f32,
    winding: i32,
}

fn crossings(outline: &[Point], top: u32, height: u32) -> Vec<Crossing> {
    let subrow_of = |y: f32| {
        let subrow = ((y - top as f32) * SUBROWS as f32 - 0.5).ceil();
        subrow.clamp(0.0, (height * SUBROWS) as f32) as u32
    };
    let mut crossings = Vec::new();
    let ends = outline.iter().cycle().skip(1);
    for (&a, &b) in outline.iter().zip(ends) {
        let (upper, lower, winding) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
        for subrow in subrow_of(upper.y)..subrow_of(lower.y) {
            let y = top as f32 + (subrow as f32 + 0.5) / SUBROWS as f32;
            let t = (y - upper.y) / (lower.y - upper.y);
            crossings.push(Crossing {
                subrow,
                x: upper.x + (lower.x - upper.x) * t,
                winding,
            });
        }
    }
    crossings.sort_by(|a, b| a.subrow.cmp(&b.subrow).then(a.x.total_cmp(&b.x)));
    crossings
}

fn cover(row: &mut [f32], start: f32, end: f32) {
    let limit = row.len() as f32;
    let (start, end) = (start.clamp(0.0, limit), end.clamp(0.0, limit));
    if end <= start {
        return;
    }
    let (first, last) = (start as usize, end as usize);
    if first == last {
        row[first] += end - start;
        return;
    }
    row[first] += (first + 1) as f32 - start;
    for value in &mut row[first + 1..last] {
        *value += 1.0;
    }
    if last < row.len() {
        row[last] += end - last as f32;
    }
}

fn blurred_rows_transposed(values: &[u16], width: usize, radius: usize) -> Vec<u16> {
    let height = values.len() / width;
    let window = (2 * radius + 1) as u32;
    let mut result = vec![0; values.len()];
    for (y, row) in values.chunks_exact(width).enumerate() {
        let at = |x: usize| u32::from(row[x.min(width - 1)]);
        let mut sum = at(0) * (radius as u32 + 1) + (1..=radius).map(at).sum::<u32>();
        for x in 0..width {
            result[x * height + y] = ((sum + window / 2) / window) as u16;
            sum = sum + at(x + radius + 1) - at(x.saturating_sub(radius));
        }
    }
    result
}

fn expand(alpha: &mut [u8], (width, height): (u32, u32), radius: u32) {
    let (width, height) = (width as usize, height as usize);
    let mut distances: Vec<f32> = alpha
        .iter()
        .map(|value| if *value >= HALF { 0.0 } else { UNREACHED })
        .collect();
    let mut line = Vec::new();
    for x in 0..width {
        line.clear();
        line.extend((0..height).map(|y| distances[y * width + x]));
        for (y, distance) in squared_distances(&line).into_iter().enumerate() {
            distances[y * width + x] = distance;
        }
    }
    for row in distances.chunks_exact_mut(width) {
        let nearest = squared_distances(row);
        row.copy_from_slice(&nearest);
    }
    for (value, squared) in alpha.iter_mut().zip(distances) {
        let covered = (radius as f32 + 1.0 - squared.sqrt()).clamp(0.0, 1.0);
        *value = (*value).max((covered * 255.0).round() as u8);
    }
}

fn squared_distances(costs: &[f32]) -> Vec<f32> {
    let mut parabolas: Vec<(usize, f32)> = Vec::new();
    for (position, cost) in costs.iter().enumerate() {
        if *cost >= UNREACHED {
            continue;
        }
        let mut start = f32::NEG_INFINITY;
        while let Some(&(other, other_start)) = parabolas.last() {
            let gap = (position - other) as f32;
            let middle = (position + other) as f32 / 2.0;
            start = (cost - costs[other]) / (2.0 * gap) + middle;
            if start > other_start {
                break;
            }
            parabolas.pop();
            start = f32::NEG_INFINITY;
        }
        parabolas.push((position, start));
    }
    if parabolas.is_empty() {
        return costs.to_vec();
    }
    let mut current = 0;
    (0..costs.len())
        .map(|position| {
            while parabolas
                .get(current + 1)
                .is_some_and(|(_, start)| *start < position as f32)
            {
                current += 1;
            }
            let (origin, _) = parabolas[current];
            let offset = position.abs_diff(origin) as f32;
            offset * offset + costs[origin]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (40, 30);

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    fn shape(shape: SelectionShape) -> Selection {
        Selection::from_shape(&shape, SIZE).expect("selection")
    }

    fn rectangle(x: f32, y: f32, width: f32, height: f32) -> Selection {
        shape(SelectionShape::Rectangle {
            x,
            y,
            width,
            height,
        })
    }

    fn lasso(points: &[(f32, f32)]) -> Selection {
        shape(SelectionShape::Lasso {
            points: points.iter().map(|(x, y)| Point::new(*x, *y)).collect(),
        })
    }

    fn total(selection: &Selection) -> f32 {
        selection
            .alpha
            .iter()
            .map(|value| f32::from(*value) / 255.0)
            .sum()
    }

    #[test]
    fn rectangles_are_exact_and_clipped_to_the_canvas() {
        let whole = rectangle(3.0, 4.0, 10.0, 5.0);
        assert_eq!(whole.bounds, rect(3, 4, 10, 5));
        assert!(whole.alpha.iter().all(|value| *value == 255));
        assert_eq!(rectangle(13.0, 9.0, -10.0, -5.0), whole);

        let clipped = rectangle(-5.0, -5.0, 100.0, 100.0);
        assert_eq!(clipped.bounds, PixelRect::full(SIZE));

        let half = rectangle(2.5, 4.0, 1.0, 1.5);
        assert_eq!(half.value(2, 4), 128);
        assert_eq!(half.value(2, 5), 64);
        for outside in [(50.0, 5.0), (5.0, -20.0)] {
            let shape = SelectionShape::Rectangle {
                x: outside.0,
                y: outside.1,
                width: 10.0,
                height: 10.0,
            };
            assert_eq!(Selection::from_shape(&shape, SIZE), None);
        }
    }

    #[test]
    fn ellipses_cover_their_area() {
        let circle = shape(SelectionShape::Ellipse {
            x: 5.0,
            y: 5.0,
            width: 20.0,
            height: 20.0,
        });
        assert!((total(&circle) - 314.16).abs() < 1.5, "{}", total(&circle));
        assert_eq!(circle.value(15, 15), 255);
        assert_eq!(circle.value(5, 5), 0);
    }

    #[test]
    fn a_lasso_that_crosses_itself_has_no_holes() {
        let twice_around = [
            (5.0, 5.0),
            (25.0, 5.0),
            (25.0, 25.0),
            (5.0, 25.0),
            (5.0, 5.0),
            (25.0, 5.0),
            (25.0, 25.0),
            (5.0, 25.0),
        ];
        assert_eq!(lasso(&twice_around), rectangle(5.0, 5.0, 20.0, 20.0));
        let triangle = lasso(&[(0.0, 0.0), (20.0, 0.0), (0.0, 20.0)]);
        assert!((total(&triangle) - 200.0).abs() < 1.0);
    }

    #[test]
    fn selections_combine() {
        let a = rectangle(0.0, 0.0, 10.0, 10.0);
        let b = rectangle(5.0, 0.0, 10.0, 10.0);
        let combined = |mode| a.combined(&b, mode).expect("selection").bounds;
        assert_eq!(combined(SelectionMode::Replace), rect(5, 0, 10, 10));
        assert_eq!(combined(SelectionMode::Add), rect(0, 0, 15, 10));
        assert_eq!(combined(SelectionMode::Subtract), rect(0, 0, 5, 10));
        assert_eq!(combined(SelectionMode::Intersect), rect(5, 0, 5, 10));
        assert_eq!(a.combined(&a, SelectionMode::Subtract), None);
    }

    #[test]
    fn inverting_twice_restores_the_selection() {
        let selection = rectangle(5.5, 5.0, 10.0, 10.0);
        let inverted = selection.inverted(SIZE).expect("selection");
        assert_eq!(inverted.value(6, 6), 0);
        assert_eq!(inverted.value(0, 0), 255);
        assert_eq!(inverted.inverted(SIZE), Some(selection));
        assert_eq!(rectangle(0.0, 0.0, 40.0, 30.0).inverted(SIZE), None);
    }

    #[test]
    fn growing_and_shrinking_move_the_edge_by_the_radius() {
        let selection = rectangle(10.0, 10.0, 10.0, 10.0);
        let grown = selection.expanded(3, SIZE).expect("selection");
        assert_eq!(grown.bounds, rect(7, 7, 16, 16));
        assert_eq!(grown.value(7, 15), 255);
        assert_eq!(grown.value(7, 7), 0);
        let shrunk = selection.contracted(2, SIZE).expect("selection");
        assert_eq!(shrunk, rectangle(12.0, 12.0, 6.0, 6.0));
        assert_eq!(selection.contracted(5, SIZE), None);

        let touching_the_edge = rectangle(0.0, 0.0, 10.0, 30.0);
        assert_eq!(
            touching_the_edge.contracted(2, SIZE),
            Some(rectangle(0.0, 0.0, 8.0, 30.0))
        );
    }

    #[test]
    fn distances_match_a_brute_force_search() {
        let (width, height) = (23, 17);
        let seeds = [(2, 3), (20, 1), (11, 16), (12, 9)];
        let mut alpha = vec![0; width * height];
        for (x, y) in seeds {
            alpha[y * width + x] = 255;
        }
        expand(&mut alpha, (width as u32, height as u32), 4);
        for (index, value) in alpha.into_iter().enumerate() {
            let (x, y) = (index % width, index / width);
            let distance = seeds
                .iter()
                .map(|(sx, sy)| (x.abs_diff(*sx) as f32).hypot(y.abs_diff(*sy) as f32))
                .fold(f32::INFINITY, f32::min);
            let expected = (5.0 - distance).clamp(0.0, 1.0) * 255.0;
            assert!((f32::from(value) - expected).abs() < 1.0, "{x} {y}");
        }
    }

    #[test]
    fn feathering_softens_within_the_radius_and_keeps_canvas_edges() {
        let selection = rectangle(0.0, 10.0, 40.0, 20.0);
        for radius in [1, 2, 5, 9] {
            let soft = selection.feathered(radius, SIZE).expect("selection");
            assert_eq!(soft.bounds.y, 10 - radius);
            assert_eq!(soft.value(20, 10 + radius), 255);
            assert_eq!(soft.value(0, 29), 255);
            let edge: Vec<_> = (10 - radius..=10 + radius)
                .map(|y| soft.value(20, y))
                .collect();
            assert!(edge.windows(2).all(|pair| pair[0] < pair[1]), "{edge:?}");
        }
    }

    #[test]
    fn invalid_shapes_are_rejected() {
        for x in [f32::NAN, f32::INFINITY, MAX_COORDINATE * 2.0] {
            let shape = SelectionShape::Ellipse {
                x,
                y: 0.0,
                width: 5.0,
                height: 5.0,
            };
            assert!(!shape.is_valid());
        }
        let empty = SelectionShape::Lasso { points: Vec::new() };
        assert_eq!(Selection::from_shape(&empty, SIZE), None);
    }
}
