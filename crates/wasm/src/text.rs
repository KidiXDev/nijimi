use nijimi_core::{Point, TextAlign, TextSettings};
use nijimi_render::TextImage;
use wasm_bindgen::prelude::*;
use web_sys::{OffscreenCanvas, OffscreenCanvasRenderingContext2d};

const UNSET_FONT: &str = "10px sans-serif";
const MARGIN: f64 = 2.0;

pub(crate) struct RenderedText {
    size: (u32, u32),
    rgba: Vec<u8>,
    position: (i32, i32),
}

impl RenderedText {
    pub fn new(text: &TextSettings, canvas: (u32, u32)) -> Result<Self, JsValue> {
        let measuring = context((1, 1))?;
        apply_style(&measuring, text)?;
        let Some((left, top, right, bottom)) = covered(text, &measuring, canvas)? else {
            return Ok(Self {
                size: (1, 1),
                rgba: vec![0; 4],
                position: (0, 0),
            });
        };
        let size = (right - left, bottom - top);
        let drawing = context(size)?;
        apply_style(&drawing, text)?;
        let Point { x, y } = text.position;
        let [a, b, c, d] = text.transform.map(f64::from);
        let (x, y) = (f64::from(x), f64::from(y));
        drawing.set_transform(a, b, c, d, x - f64::from(left), y - f64::from(top))?;
        let [red, green, blue] = text.color.map(|channel| (channel * 255.0).round());
        drawing.set_fill_style_str(&format!("rgb({red} {green} {blue})"));
        for (line, baseline) in lines(text, &drawing)? {
            drawing.fill_text(line, 0.0, baseline)?;
        }
        let (width, height) = (f64::from(size.0), f64::from(size.1));
        let image = drawing.get_image_data(0.0, 0.0, width, height)?;
        Ok(Self {
            size,
            rgba: image.data().0,
            position: (left as i32, top as i32),
        })
    }

    pub fn image(&self) -> TextImage<'_> {
        TextImage {
            size: self.size,
            rgba: &self.rgba,
            position: self.position,
        }
    }
}

fn context((width, height): (u32, u32)) -> Result<OffscreenCanvasRenderingContext2d, JsValue> {
    OffscreenCanvas::new(width, height)?
        .get_context("2d")?
        .and_then(|context| context.dyn_into().ok())
        .ok_or_else(|| JsError::new("2D canvas is unavailable.").into())
}

fn apply_style(
    context: &OffscreenCanvasRenderingContext2d,
    text: &TextSettings,
) -> Result<(), JsValue> {
    let style = if text.italic { "italic" } else { "normal" };
    let TextSettings {
        weight,
        font_size,
        font_family,
        ..
    } = text;
    context.set_font(&format!("{style} {weight} {font_size}px {font_family}"));
    if context.font() == UNSET_FONT && *font_size != 10.0 {
        return Err(JsError::new("The font family is not valid CSS.").into());
    }
    context.set_text_align(match text.align {
        TextAlign::Left => "left",
        TextAlign::Center => "center",
        TextAlign::Right => "right",
    });
    Ok(())
}

fn lines<'a>(
    text: &'a TextSettings,
    context: &OffscreenCanvasRenderingContext2d,
) -> Result<Vec<(&'a str, f64)>, JsValue> {
    let ascent = context.measure_text("")?.font_bounding_box_ascent();
    let spacing = f64::from(text.font_size) * f64::from(text.line_height);
    Ok(text
        .text
        .split('\n')
        .enumerate()
        .map(|(index, line)| (line, ascent + index as f64 * spacing))
        .collect())
}

fn covered(
    text: &TextSettings,
    context: &OffscreenCanvasRenderingContext2d,
    canvas: (u32, u32),
) -> Result<Option<(u32, u32, u32, u32)>, JsValue> {
    let Point { x, y } = text.position;
    let [a, b, c, d] = text.transform.map(f64::from);
    let mut xs = (f64::INFINITY, f64::NEG_INFINITY);
    let mut ys = xs;
    for (line, baseline) in lines(text, context)? {
        let metrics = context.measure_text(line)?;
        let left = -metrics.actual_bounding_box_left() - MARGIN;
        let right = metrics.actual_bounding_box_right() + MARGIN;
        let top = baseline - metrics.actual_bounding_box_ascent() - MARGIN;
        let bottom = baseline + metrics.actual_bounding_box_descent() + MARGIN;
        for (u, v) in [(left, top), (right, top), (left, bottom), (right, bottom)] {
            let (px, py) = (f64::from(x) + a * u + c * v, f64::from(y) + b * u + d * v);
            xs = (xs.0.min(px), xs.1.max(px));
            ys = (ys.0.min(py), ys.1.max(py));
        }
    }
    let clip = |(start, end): (f64, f64), limit: u32| {
        (
            (start.floor() as u32).min(limit),
            (end.ceil() as u32).min(limit),
        )
    };
    let ((left, right), (top, bottom)) = (clip(xs, canvas.0), clip(ys, canvas.1));
    Ok((left < right && top < bottom).then_some((left, top, right, bottom)))
}
