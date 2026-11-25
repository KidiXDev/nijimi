use serde::{Deserialize, Deserializer};

use crate::{
    Adjustment, BlendMode, Brush, BrushDynamics, LayerId, PixelRect, StrokePoint, Transform,
};

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command {
    AddLayer {
        name: Option<String>,
        #[serde(default, deserialize_with = "present")]
        parent: Option<Option<LayerId>>,
        index: Option<usize>,
    },
    AddGroup {
        name: Option<String>,
        #[serde(default, deserialize_with = "present")]
        parent: Option<Option<LayerId>>,
        index: Option<usize>,
    },
    AddAdjustment {
        name: Option<String>,
        #[serde(default, deserialize_with = "present")]
        parent: Option<Option<LayerId>>,
        index: Option<usize>,
        adjustment: Adjustment,
    },
    SetAdjustment {
        id: LayerId,
        adjustment: Adjustment,
    },
    AddMask {
        id: LayerId,
        #[serde(default = "reveal")]
        reveal: bool,
    },
    RemoveMask {
        id: LayerId,
    },
    PaintMaskStroke {
        id: LayerId,
        value: f32,
        brush: Brush,
        points: Vec<StrokePoint>,
        #[serde(default)]
        dynamics: BrushDynamics,
    },
    DeleteLayer {
        id: LayerId,
    },
    MoveLayer {
        id: LayerId,
        parent: Option<LayerId>,
        index: usize,
    },
    RenameLayer {
        id: LayerId,
        name: String,
    },
    SetLayerVisible {
        id: LayerId,
        visible: bool,
    },
    SetLayerOpacity {
        id: LayerId,
        opacity: f32,
    },
    SetLayerBlendMode {
        id: LayerId,
        blend_mode: BlendMode,
    },
    SelectLayer {
        id: LayerId,
    },
    PaintStroke {
        layer: LayerId,
        brush: Brush,
        points: Vec<StrokePoint>,
    },
    EraseStroke {
        layer: LayerId,
        brush: Brush,
        points: Vec<StrokePoint>,
        #[serde(default)]
        dynamics: BrushDynamics,
    },
    PaintWithDynamics {
        layer: LayerId,
        brush: Brush,
        points: Vec<StrokePoint>,
        dynamics: BrushDynamics,
    },
    Crop {
        rect: PixelRect,
    },
    ResizeCanvas {
        width: u32,
        height: u32,
        #[serde(default)]
        x: i32,
        #[serde(default)]
        y: i32,
    },
    ExtendCanvas {
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
    },
    TransformLayer {
        id: LayerId,
        transform: Transform,
    },
    Undo,
    Redo,
}

fn reveal() -> bool {
    true
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Command {
        serde_json::from_str(json).expect("command")
    }

    fn add_layer(parent: Option<Option<LayerId>>) -> Command {
        Command::AddLayer {
            name: None,
            parent,
            index: None,
        }
    }

    #[test]
    fn an_omitted_parent_differs_from_the_root() {
        assert_eq!(parse(r#"{"type":"addLayer"}"#), add_layer(None));
        assert_eq!(
            parse(r#"{"type":"addLayer","parent":null}"#),
            add_layer(Some(None))
        );
        assert_eq!(
            parse(r#"{"type":"addLayer","parent":4}"#),
            add_layer(Some(Some(4)))
        );
    }

    #[test]
    fn pressure_is_optional() {
        let command = parse(
            r#"{"type":"paintStroke","layer":1,"points":[{"x":1,"y":2},{"x":3,"y":4,"pressure":0.5}],
            "brush":{"diameter":9,"hardness":1,"flow":1,"opacity":1,"color":[0,0.5,1]}}"#,
        );
        let Command::PaintStroke { points, .. } = command else {
            panic!("expected a stroke");
        };
        assert_eq!((points[0].pressure, points[1].pressure), (1.0, 0.5));
    }

    #[test]
    fn malformed_commands_are_rejected() {
        for json in [
            "{}",
            r#"{"type":"unknown"}"#,
            r#"{"type":"setLayerOpacity","id":1,"opacity":null}"#,
            r#"{"type":"deleteLayer","id":-1}"#,
            r#"{"type":"setLayerBlendMode","id":1,"blendMode":"dodge"}"#,
        ] {
            assert!(serde_json::from_str::<Command>(json).is_err(), "{json}");
        }
    }
}
