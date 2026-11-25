use serde::Serialize;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Histogram {
    pub red: Vec<u32>,
    pub green: Vec<u32>,
    pub blue: Vec<u32>,
    pub luma: Vec<u32>,
    pub pixels: u32,
}

impl Histogram {
    pub fn from_rgba8(rgba: &[[u8; 4]]) -> Self {
        let mut result = Self {
            red: vec![0; 256],
            green: vec![0; 256],
            blue: vec![0; 256],
            luma: vec![0; 256],
            pixels: 0,
        };
        for &[red, green, blue, alpha] in rgba {
            if alpha > 0 {
                let [r, g, b] = [red, green, blue].map(usize::from);
                let luma = (2126 * r + 7152 * g + 722 * b + 5000) / 10000;
                result.red[r] += 1;
                result.green[g] += 1;
                result.blue[b] += 1;
                result.luma[luma] += 1;
                result.pixels += 1;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_counts_visible_pixels_and_luma() {
        let histogram =
            Histogram::from_rgba8(&[[255, 0, 0, 255], [255, 255, 255, 128], [10, 20, 30, 0]]);
        assert_eq!(histogram.pixels, 2);
        assert_eq!(histogram.red[255], 2);
        assert_eq!(histogram.green[0], 1);
        assert_eq!(histogram.blue[255], 1);
        assert_eq!(histogram.luma[54], 1);
        assert_eq!(histogram.luma[255], 1);
    }
}
