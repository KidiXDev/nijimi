use std::{error::Error, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::Point;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Adjustment {
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
    },
    Levels {
        black: f32,
        white: f32,
        gamma: f32,
        #[serde(default)]
        output_black: f32,
        #[serde(default = "one")]
        output_white: f32,
    },
    Curves {
        points: Vec<Point>,
    },
    Hsl {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
    },
    Exposure {
        stops: f32,
    },
    Lut {
        table: Arc<CubeLut>,
    },
    Sharpen {
        amount: f32,
    },
    Blur {
        radius: u32,
    },
    Vignette {
        amount: f32,
        radius: f32,
        feather: f32,
    },
    Grain {
        amount: f32,
        #[serde(default)]
        seed: u32,
    },
}

fn one() -> f32 {
    1.0
}

impl Adjustment {
    pub fn is_valid(&self) -> bool {
        let bipolar = |v: f32| (-1.0..=1.0).contains(&v);
        let unit = |v: f32| (0.0..=1.0).contains(&v);
        match self {
            Self::BrightnessContrast {
                brightness,
                contrast,
            } => bipolar(*brightness) && bipolar(*contrast),
            Self::Levels {
                black,
                white,
                gamma,
                output_black,
                output_white,
            } => {
                [*black, *white, *output_black, *output_white]
                    .into_iter()
                    .all(unit)
                    && white - black >= 0.0001
                    && output_white >= output_black
                    && (0.1..=10.0).contains(gamma)
            }
            Self::Curves { points } => {
                (2..=64).contains(&points.len())
                    && points[0].x == 0.0
                    && points[points.len() - 1].x == 1.0
                    && points.iter().all(|point| unit(point.y))
                    && points.windows(2).all(|p| p[0].x < p[1].x)
            }
            Self::Hsl {
                hue,
                saturation,
                lightness,
            } => (-360.0..=360.0).contains(hue) && bipolar(*saturation) && bipolar(*lightness),
            Self::ColorBalance {
                shadows,
                midtones,
                highlights,
            } => shadows
                .iter()
                .chain(midtones)
                .chain(highlights)
                .copied()
                .all(bipolar),
            Self::Exposure { stops } => (-10.0..=10.0).contains(stops),
            Self::Lut { table } => table.is_valid(),
            Self::Sharpen { amount } => (0.0..=2.0).contains(amount),
            Self::Blur { radius } => *radius <= 32,
            Self::Vignette {
                amount,
                radius,
                feather,
            } => unit(*amount) && unit(*radius) && (0.01..=1.0).contains(feather),
            Self::Grain { amount, .. } => unit(*amount),
        }
    }

    pub fn is_spatial(&self) -> bool {
        matches!(self, Self::Blur { radius: 1.. })
            || matches!(self, Self::Sharpen { amount } if *amount > 0.0)
    }

    pub(crate) fn data_bytes(&self) -> u64 {
        match self {
            Self::Lut { table } => size_of_val(table.samples.as_slice()) as u64,
            Self::Curves { points } => size_of_val(points.as_slice()) as u64,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LutKind {
    OneDimensional,
    ThreeDimensional,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LutInfo {
    pub title: Option<String>,
    pub kind: LutKind,
    pub size: u32,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
}

impl LutInfo {
    fn table_len(&self) -> Option<usize> {
        match self.kind {
            LutKind::OneDimensional if (2..=65_536).contains(&self.size) => {
                Some(self.size as usize)
            }
            LutKind::ThreeDimensional if (2..=65).contains(&self.size) => {
                Some((self.size as usize).pow(3))
            }
            _ => None,
        }
    }

    pub fn sample_count(&self) -> Option<usize> {
        let count = self.table_len()?;
        self.title
            .as_ref()
            .is_none_or(|title| title.len() <= 256)
            .then_some(())?;
        self.domain_min
            .into_iter()
            .zip(self.domain_max)
            .all(|(min, max)| {
                min.is_finite() && max.is_finite() && (max - min).is_finite() && max - min >= 1e-8
            })
            .then_some(count)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CubeLut {
    #[serde(flatten)]
    pub info: LutInfo,
    pub samples: Vec<[f32; 3]>,
}

impl CubeLut {
    pub fn is_valid(&self) -> bool {
        self.info.sample_count() == Some(self.samples.len())
            && self
                .samples
                .iter()
                .flatten()
                .all(|v| v.is_finite() && v.abs() <= 65504.0)
    }

    pub fn parse(text: &str) -> Result<Self, CubeError> {
        let mut info = LutInfo {
            title: None,
            kind: LutKind::ThreeDimensional,
            size: 0,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
        };
        let mut domain_min = false;
        let mut domain_max = false;
        let mut samples = Vec::new();
        for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let invalid = || CubeError::Line(index + 1);
            let line = line.trim();
            if let Some(title) = line.strip_prefix("TITLE") {
                let title = title.trim().strip_prefix('"').ok_or_else(invalid)?;
                let (title, after) = title.split_once('"').ok_or_else(invalid)?;
                if !samples.is_empty()
                    || info.title.is_some()
                    || title.len() > 256
                    || !(after.trim().is_empty() || after.trim_start().starts_with('#'))
                {
                    return Err(invalid());
                }
                info.title = Some(title.into());
                continue;
            }
            let mut words = line
                .split('#')
                .next()
                .unwrap_or_default()
                .split_whitespace();
            let Some(first) = words.next() else { continue };
            match first {
                "LUT_1D_SIZE" | "LUT_3D_SIZE" if info.size == 0 => {
                    info.kind = if first == "LUT_1D_SIZE" {
                        LutKind::OneDimensional
                    } else {
                        LutKind::ThreeDimensional
                    };
                    info.size = words
                        .next()
                        .and_then(|word| word.parse().ok())
                        .ok_or_else(invalid)?;
                    if words.next().is_some() || info.table_len().is_none() {
                        return Err(invalid());
                    }
                }
                "DOMAIN_MIN" if samples.is_empty() && !domain_min => {
                    info.domain_min = triple(words).ok_or_else(invalid)?;
                    domain_min = true;
                }
                "DOMAIN_MAX" if samples.is_empty() && !domain_max => {
                    info.domain_max = triple(words).ok_or_else(invalid)?;
                    domain_max = true;
                }
                "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE"
                    if samples.is_empty() && !domain_min && !domain_max =>
                {
                    let min = words
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(invalid)?;
                    let max = words
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(invalid)?;
                    if words.next().is_some() {
                        return Err(invalid());
                    }
                    info.domain_min = [min; 3];
                    info.domain_max = [max; 3];
                    domain_min = true;
                    domain_max = true;
                }
                _ => {
                    let expected = info.table_len().ok_or_else(invalid)?;
                    if samples.len() == expected {
                        return Err(invalid());
                    }
                    samples.push(triple(std::iter::once(first).chain(words)).ok_or_else(invalid)?);
                }
            }
        }
        let table = Self { info, samples };
        table.is_valid().then_some(table).ok_or(CubeError::Table)
    }
}

fn triple<'a>(mut words: impl Iterator<Item = &'a str>) -> Option<[f32; 3]> {
    let mut result = [0.0; 3];
    for channel in &mut result {
        *channel = words.next()?.parse().ok()?;
    }
    words.next().is_none().then_some(result)
}

#[derive(Debug, PartialEq)]
pub enum CubeError {
    Line(usize),
    Table,
}

impl fmt::Display for CubeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Line(line) => write!(f, "Invalid .cube data on line {line}."),
            Self::Table => write!(
                f,
                "The .cube domain or table values do not match the declared size."
            ),
        }
    }
}
impl Error for CubeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_parses_titles_comments_domains_and_samples() {
        let table = CubeLut::parse(
            "TITLE \"Test #1\" # look\nLUT_1D_SIZE 2\nDOMAIN_MIN -1 0 0\n0 0 0\n1 .25 .5\n",
        )
        .unwrap();
        assert_eq!(table.info.title.as_deref(), Some("Test #1"));
        assert_eq!(table.info.domain_min, [-1.0, 0.0, 0.0]);
        assert_eq!(table.samples[1], [1.0, 0.25, 0.5]);
        assert!(CubeLut::parse("LUT_3D_SIZE 2\n0 0 0\n").is_err());
        assert!(CubeLut::parse("LUT_1D_SIZE 2\n0 0 0\nNaN 0 0\n").is_err());
        assert!(CubeLut::parse("LUT_1D_SIZE 2\n0 0 0\n1 1 1\n1 1 1").is_err());
    }
}
