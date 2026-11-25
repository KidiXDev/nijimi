use js_sys::JSON;
use nijimi_core::{
    Adjustment, Document, Layer, LayerContent, LayerId, LayerPlace, LayerProperties, LutInfo,
    PixelRect, SelectionMode, TextSettings, deserialize_present,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
export type BlendMode =
  | 'normal'
  | 'multiply'
  | 'screen'
  | 'overlay'
  | 'softLight'
  | 'add'
  | 'darken'
  | 'lighten';

/** Diameter is in document pixels. Every other value is between 0 and 1. */
export interface Brush {
  diameter: number;
  hardness: number;
  flow: number;
  opacity: number;
  color: [red: number, green: number, blue: number];
}

export type BrushPreset = 'pencil' | 'round' | 'airbrush';

export interface BrushDynamics {
  pressureCurve?: 'linear' | 'soft' | 'firm' | 'fixed';
  /** Fraction of the previous sample retained each step, from 0 to 0.95. */
  stabilizer?: number;
}

export interface Point {
  x: number;
  y: number;
}

export interface PixelRect extends Point {
  width: number;
  height: number;
}

export type CropRatio = 'square' | 'classic' | 'photo' | 'wide' | 'portrait';

/** Scale, then rotate clockwise in degrees, then translate in document pixels. */
export interface LayerTransform {
  x?: number;
  y?: number;
  /** Use a negative scale to flip an axis. */
  scaleX?: number;
  scaleY?: number;
  /** Defaults to the center of the document. */
  pivot?: Point;
  rotation?: number;
}

export interface ImportOptions {
  x?: number;
  y?: number;
  name?: string;
  parent?: number | null;
  index?: number;
}

export interface FillOptions extends Point {
  id: number;
  /** Straight sRGB and alpha, each in the range [0, 1]. */
  color: [number, number, number, number];
  tolerance?: number;
}

export type SelectionShape =
  | { type: 'rectangle'; x: number; y: number; width: number; height: number }
  | { type: 'ellipse'; x: number; y: number; width: number; height: number }
  | { type: 'lasso'; points: Point[] };

/** Every mode acts like `replace` while nothing is selected. */
export type SelectionMode = 'replace' | 'add' | 'subtract' | 'intersect';

export interface SelectSimilarOptions extends Point {
  /** Compares this layer alone. Omit it to compare the visible composite. */
  layer?: number;
  tolerance?: number;
  /** Defaults to true, which keeps only pixels connected to the starting point. */
  contiguous?: boolean;
  mode?: SelectionMode;
}

/** The offset points from the painted pixel to the pixel it copies. */
export type RetouchTool =
  | { type: 'clone'; offsetX: number; offsetY: number }
  | { type: 'heal'; offsetX: number; offsetY: number }
  | { type: 'smudge' }
  | { type: 'blur'; radius: number };

export interface TextSettings extends Point {
  text: string;
  /** A CSS font family list. Load the font before using it. */
  fontFamily: string;
  fontSize: number;
  weight?: number;
  italic?: boolean;
  color?: Rgb;
  align?: 'left' | 'center' | 'right';
  /** Multiple of the font size. */
  lineHeight?: number;
  /** The a, b, c and d values of a canvas transform, applied around the position. */
  transform?: [number, number, number, number];
}

export type Rgb = [number, number, number];

export type AdjustmentSettings =
  | { type: 'brightnessContrast'; brightness: number; contrast: number }
  | { type: 'levels'; black: number; white: number; gamma: number; outputBlack?: number; outputWhite?: number }
  | { type: 'curves'; points: Point[] }
  | { type: 'hsl'; hue: number; saturation: number; lightness: number }
  | { type: 'colorBalance'; shadows: Rgb; midtones: Rgb; highlights: Rgb }
  | { type: 'exposure'; stops: number }
  | { type: 'sharpen'; amount: number }
  | { type: 'blur'; radius: number }
  | { type: 'vignette'; amount: number; radius: number; feather: number }
  | { type: 'grain'; amount: number; seed?: number };

export interface LutInfo {
  title: string | null;
  kind: 'oneDimensional' | 'threeDimensional';
  size: number;
  domainMin: Rgb;
  domainMax: Rgb;
}

export type Adjustment = AdjustmentSettings | { type: 'lut'; table: LutInfo & { samples: Rgb[] } };

/** Bins are indexed by sRGB byte value and exclude fully transparent pixels. */
export interface Histogram {
  red: number[];
  green: number[];
  blue: number[];
  luma: number[];
  pixels: number;
}

export type LayerPlacement = Pick<ImportOptions, 'name' | 'parent' | 'index'>;

export type PreviewCommand =
  | { type: 'setAdjustment'; id: number; adjustment: Adjustment }
  | { type: 'setLayerOpacity'; id: number; opacity: number }
  | { type: 'crop'; rect: PixelRect }
  | { type: 'resizeCanvas'; width: number; height: number; x?: number; y?: number }
  | { type: 'extendCanvas'; left: number; top: number; right: number; bottom: number }
  | { type: 'transformLayer'; id: number; transform: LayerTransform };

export interface StrokePoint {
  x: number;
  y: number;
  pressure?: number;
}

/**
 * Layers are ordered from bottom to top, so index 0 is the bottom.
 * A new layer without `parent` and `index` goes above the active layer.
 * `parent: null` is the top level.
 */
export type Command =
  | { type: 'addLayer'; name?: string; parent?: number | null; index?: number }
  | { type: 'addGroup'; name?: string; parent?: number | null; index?: number }
  | ({ type: 'addAdjustment'; adjustment: Adjustment } & LayerPlacement)
  | { type: 'addMask'; id: number; reveal?: boolean }
  | { type: 'removeMask'; id: number }
  | { type: 'paintMaskStroke'; id: number; value: number; brush: Brush; points: StrokePoint[]; dynamics?: BrushDynamics }
  | { type: 'deleteLayer'; id: number }
  | { type: 'moveLayer'; id: number; parent: number | null; index: number }
  | { type: 'renameLayer'; id: number; name: string }
  | { type: 'setLayerVisible'; id: number; visible: boolean }
  | PreviewCommand
  | { type: 'setLayerBlendMode'; id: number; blendMode: BlendMode }
  | { type: 'selectLayer'; id: number }
  | { type: 'paintStroke'; layer: number; brush: Brush; dynamics?: BrushDynamics; points: StrokePoint[] }
  | { type: 'eraseStroke'; layer: number; brush: Brush; dynamics?: BrushDynamics; points: StrokePoint[] }
  | { type: 'retouchStroke'; layer: number; tool: RetouchTool; brush: Brush; dynamics?: BrushDynamics; points: StrokePoint[] }
  | { type: 'select'; shape: SelectionShape; mode?: SelectionMode }
  | { type: 'deselect' }
  | { type: 'invertSelection' }
  | { type: 'featherSelection'; radius: number }
  | { type: 'growSelection'; radius: number }
  | { type: 'shrinkSelection'; radius: number }
  | { type: 'maskFromSelection'; id: number }
  | { type: 'rasterizeText'; id: number }
  | { type: 'undo' }
  | { type: 'redo' };

export type EditorEvent =
  | { type: 'documentReplaced' }
  | { type: 'canvasResized'; width: number; height: number }
  | { type: 'previewChanged' }
  | { type: 'layerAdded'; id: number }
  | { type: 'layerRemoved'; id: number }
  | { type: 'layerMoved'; id: number }
  | { type: 'layerChanged'; id: number }
  | { type: 'pixelsChanged'; id: number }
  | { type: 'maskChanged'; id: number }
  | { type: 'selectionChanged' }
  | { type: 'activeLayerChanged'; id: number | null }
  | { type: 'historyChanged'; canUndo: boolean; canRedo: boolean }
  | { type: 'deviceLost'; message: string }
  | { type: 'gpuError'; message: string };

export interface LayerState {
  id: number;
  name: string;
  visible: boolean;
  opacity: number;
  blendMode: BlendMode;
  kind: 'raster' | 'text' | 'group' | 'adjustment';
  adjustment: AdjustmentSettings | { type: 'lut'; table: LutInfo } | null;
  text: Required<TextSettings> | null;
  mask: { reveal: boolean } | null;
  children: LayerState[];
}

export interface EditorState {
  width: number;
  height: number;
  activeLayer: number | null;
  /** Bounds of the selected pixels, or null when nothing is selected. */
  selection: PixelRect | null;
  canUndo: boolean;
  canRedo: boolean;
  layers: LayerState[];
}
"#;

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct ImportOptions {
    pub x: i32,
    pub y: i32,
    pub name: Option<String>,
    #[serde(deserialize_with = "deserialize_present")]
    pub parent: Option<Option<LayerId>>,
    pub index: Option<usize>,
}

impl ImportOptions {
    pub fn place(&self) -> Option<LayerPlace> {
        LayerPlace::requested(self.parent, self.index)
    }
}

#[derive(Deserialize)]
pub(crate) struct FillOptions {
    pub id: LayerId,
    pub x: u32,
    pub y: u32,
    pub color: [f32; 4],
    #[serde(default)]
    pub tolerance: f32,
}

#[derive(Deserialize)]
pub(crate) struct SelectSimilarOptions {
    pub x: u32,
    pub y: u32,
    pub layer: Option<LayerId>,
    #[serde(default)]
    pub tolerance: f32,
    #[serde(default = "contiguous")]
    pub contiguous: bool,
    #[serde(default)]
    pub mode: SelectionMode,
}

fn contiguous() -> bool {
    true
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorState<'a> {
    width: u32,
    height: u32,
    active_layer: Option<LayerId>,
    selection: Option<PixelRect>,
    can_undo: bool,
    can_redo: bool,
    layers: Vec<LayerState<'a>>,
}

#[derive(Serialize)]
struct LayerState<'a> {
    id: LayerId,
    #[serde(flatten)]
    properties: &'a LayerProperties,
    kind: &'static str,
    adjustment: Option<AdjustmentState<'a>>,
    text: Option<&'a TextSettings>,
    mask: Option<MaskState>,
    children: Vec<LayerState<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum AdjustmentState<'a> {
    Settings(&'a Adjustment),
    Lut {
        #[serde(rename = "type")]
        kind: &'static str,
        table: &'a LutInfo,
    },
}

#[derive(Serialize)]
struct MaskState {
    reveal: bool,
}

impl<'a> EditorState<'a> {
    pub fn new<T: Clone>(document: &'a Document<T>, committed: &Document<T>) -> Self {
        let (width, height) = document.size();
        Self {
            width,
            height,
            active_layer: document.active(),
            selection: document.selection().map(|selection| selection.bounds()),
            can_undo: committed.can_undo(),
            can_redo: committed.can_redo(),
            layers: layer_states(document.layers()),
        }
    }
}

fn layer_states<T>(layers: &[Layer<T>]) -> Vec<LayerState<'_>> {
    layers
        .iter()
        .map(|layer| LayerState {
            id: layer.id,
            properties: &layer.properties,
            kind: match layer.content {
                LayerContent::Raster(_) if layer.text.is_some() => "text",
                LayerContent::Raster(_) => "raster",
                LayerContent::Group(_) => "group",
                LayerContent::Adjustment(_) => "adjustment",
            },
            adjustment: layer.grade().map(|adjustment| match adjustment {
                Adjustment::Lut { table } => AdjustmentState::Lut {
                    kind: "lut",
                    table: &table.info,
                },
                _ => AdjustmentState::Settings(adjustment),
            }),
            text: layer.text.as_ref(),
            mask: layer.mask.as_ref().map(|mask| MaskState {
                reveal: mask.reveal,
            }),
            children: layer_states(layer.children()),
        })
        .collect()
}

pub fn to_js(value: &impl Serialize) -> Result<JsValue, JsValue> {
    let json = serde_json::to_string(value).map_err(JsError::from)?;
    JSON::parse(&json)
}

pub fn from_js<T: DeserializeOwned>(value: &JsValue) -> Result<T, JsValue> {
    let json = JSON::stringify(value)?
        .as_string()
        .ok_or_else(|| JsError::new("Expected an object."))?;
    Ok(serde_json::from_str(&json).map_err(JsError::from)?)
}
