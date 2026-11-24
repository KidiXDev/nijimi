use std::{
    error::Error,
    fmt,
    io::{self, Cursor, Read, Write},
};

use serde::{Deserialize, Serialize};
use zip::{ZipArchive, ZipWriter, result::ZipError, write::SimpleFileOptions};

use crate::{
    Document, DocumentError, Layer, LayerContent, LayerId, LayerProperties, MAX_LAYERS, TILE_BYTES,
    TileCoord, Tiles,
};

pub const MAX_PROJECT_TILES: usize = 32_768;

const FORMAT: &str = "nijimi";
const VERSION: u32 = 1;
const MANIFEST: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const FASTEST_COMPRESSION: i64 = 1;
const HALF_FLOAT_ONE: u16 = 0x3C00;
const HALF_FLOAT_NEGATIVE_ZERO: u16 = 0x8000;
const ROUNDING_STEPS: u16 = 4;

#[derive(Debug)]
pub enum ProjectError {
    NotAProject,
    UnsupportedVersion(u32),
    InvalidManifest(String),
    InvalidDocument(DocumentError),
    InvalidSize,
    TooManyTiles,
    DuplicateTile { layer: LayerId, coord: TileCoord },
    InvalidTile { layer: LayerId, coord: TileCoord },
    Archive(ZipError),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAProject => write!(formatter, "The file is not a Nijimi project."),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "Project version {version} is not supported. This engine reads up to version {VERSION}."
            ),
            Self::InvalidManifest(reason) => {
                write!(formatter, "The project manifest is invalid: {reason}")
            }
            Self::InvalidDocument(error) => write!(formatter, "The project is invalid: {error}"),
            Self::InvalidSize => write!(formatter, "The project has an invalid document size."),
            Self::TooManyTiles => write!(
                formatter,
                "The project has more than {MAX_PROJECT_TILES} tiles."
            ),
            Self::DuplicateTile { layer, coord } => write!(
                formatter,
                "Tile {}, {} of layer {layer} appears twice.",
                coord.x, coord.y
            ),
            Self::InvalidTile { layer, coord } => write!(
                formatter,
                "Tile {}, {} of layer {layer} is missing or damaged.",
                coord.x, coord.y
            ),
            Self::Archive(error) => write!(formatter, "The project file is damaged: {error}"),
        }
    }
}

impl Error for ProjectError {}

impl From<ZipError> for ProjectError {
    fn from(error: ZipError) -> Self {
        Self::Archive(error)
    }
}

impl From<io::Error> for ProjectError {
    fn from(error: io::Error) -> Self {
        Self::Archive(error.into())
    }
}

impl From<DocumentError> for ProjectError {
    fn from(error: DocumentError) -> Self {
        Self::InvalidDocument(error)
    }
}

#[derive(Deserialize)]
struct Header {
    format: String,
    version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    format: String,
    version: u32,
    width: u32,
    height: u32,
    active_layer: Option<LayerId>,
    layers: Vec<ManifestLayer>,
}

#[derive(Serialize, Deserialize)]
struct ManifestLayer {
    id: LayerId,
    #[serde(flatten)]
    properties: LayerProperties,
    #[serde(flatten)]
    content: ManifestContent,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ManifestContent {
    Raster { tiles: Vec<TileCoord> },
    Group { layers: Vec<ManifestLayer> },
}

pub struct ProjectWriter {
    archive: ZipWriter<Cursor<Vec<u8>>>,
    tile_count: usize,
}

impl Default for ProjectWriter {
    fn default() -> Self {
        Self {
            archive: ZipWriter::new(Cursor::new(Vec::new())),
            tile_count: 0,
        }
    }
}

impl ProjectWriter {
    pub fn add_tile(
        &mut self,
        layer: LayerId,
        coord: TileCoord,
        pixels: &[u8],
    ) -> Result<(), ProjectError> {
        if self.tile_count == MAX_PROJECT_TILES {
            return Err(ProjectError::TooManyTiles);
        }
        self.tile_count += 1;
        self.write_entry(tile_path(layer, coord), pixels)
    }

    pub fn finish<T>(
        mut self,
        (width, height): (u32, u32),
        layers: &[Layer<T>],
        active_layer: Option<LayerId>,
    ) -> Result<Vec<u8>, ProjectError> {
        let manifest = Manifest {
            format: FORMAT.into(),
            version: VERSION,
            width,
            height,
            active_layer,
            layers: layers.iter().map(manifest_layer).collect(),
        };
        let json = serde_json::to_vec(&manifest)
            .map_err(|error| ProjectError::InvalidManifest(error.to_string()))?;
        self.write_entry(MANIFEST.into(), &json)?;
        Ok(self.archive.finish()?.into_inner())
    }

    fn write_entry(&mut self, path: String, content: &[u8]) -> Result<(), ProjectError> {
        let options = SimpleFileOptions::default().compression_level(Some(FASTEST_COMPRESSION));
        self.archive.start_file(path, options)?;
        self.archive.write_all(content)?;
        Ok(())
    }
}

fn manifest_layer<T>(layer: &Layer<T>) -> ManifestLayer {
    ManifestLayer {
        id: layer.id,
        properties: layer.properties.clone(),
        content: match &layer.content {
            LayerContent::Raster(tiles) => ManifestContent::Raster {
                tiles: tiles.keys().copied().collect(),
            },
            LayerContent::Group(children) => ManifestContent::Group {
                layers: children.iter().map(manifest_layer).collect(),
            },
        },
    }
}

pub struct ProjectReader {
    archive: ZipArchive<Cursor<Vec<u8>>>,
    document: Document<()>,
}

impl ProjectReader {
    pub fn open(file: Vec<u8>) -> Result<Self, ProjectError> {
        let mut archive =
            ZipArchive::new(Cursor::new(file)).map_err(|_| ProjectError::NotAProject)?;
        let json = {
            let entry = archive
                .by_name(MANIFEST)
                .map_err(|_| ProjectError::NotAProject)?;
            let mut json = Vec::new();
            entry.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut json)?;
            json
        };
        if json.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(ProjectError::InvalidManifest("it is too large".into()));
        }

        let header: Header = parse(&json)?;
        if header.format != FORMAT {
            return Err(ProjectError::NotAProject);
        }
        if header.version == 0 || header.version > VERSION {
            return Err(ProjectError::UnsupportedVersion(header.version));
        }
        let manifest: Manifest = parse(&json)?;
        if manifest.width == 0 || manifest.height == 0 {
            return Err(ProjectError::InvalidSize);
        }
        let mut limits = Limits::default();
        let layers = manifest
            .layers
            .into_iter()
            .map(|layer| limits.accept(layer))
            .collect::<Result<_, _>>()?;
        let document = Document::with_layers(
            (manifest.width, manifest.height),
            layers,
            manifest.active_layer,
        )?;
        Ok(Self { archive, document })
    }

    pub fn document(&self) -> &Document<()> {
        &self.document
    }

    pub fn read_tile(&mut self, layer: LayerId, coord: TileCoord) -> Result<Vec<u8>, ProjectError> {
        let invalid = || ProjectError::InvalidTile { layer, coord };
        let entry = self
            .archive
            .by_name(&tile_path(layer, coord))
            .map_err(|_| invalid())?;
        if entry.size() != TILE_BYTES as u64 {
            return Err(invalid());
        }
        let mut pixels = Vec::with_capacity(TILE_BYTES + 1);
        entry
            .take(TILE_BYTES as u64 + 1)
            .read_to_end(&mut pixels)
            .map_err(|_| invalid())?;
        if pixels.len() != TILE_BYTES || !has_valid_channels(&pixels) {
            return Err(invalid());
        }
        Ok(pixels)
    }
}

#[derive(Default)]
struct Limits {
    layers: usize,
    tiles: usize,
}

impl Limits {
    fn accept(&mut self, layer: ManifestLayer) -> Result<Layer<()>, ProjectError> {
        self.layers += 1;
        if self.layers > MAX_LAYERS {
            return Err(DocumentError::TooManyLayers.into());
        }
        let id = layer.id;
        let content = match layer.content {
            ManifestContent::Raster { tiles: coords } => {
                self.tiles += coords.len();
                if self.tiles > MAX_PROJECT_TILES {
                    return Err(ProjectError::TooManyTiles);
                }
                let mut tiles = Tiles::new();
                for coord in coords {
                    if tiles.insert(coord, ()).is_some() {
                        return Err(ProjectError::DuplicateTile { layer: id, coord });
                    }
                }
                LayerContent::Raster(tiles)
            }
            ManifestContent::Group { layers } => LayerContent::Group(
                layers
                    .into_iter()
                    .map(|layer| self.accept(layer))
                    .collect::<Result<_, _>>()?,
            ),
        };
        Ok(Layer {
            id,
            properties: layer.properties,
            content,
        })
    }
}

fn parse<'a, T: Deserialize<'a>>(json: &'a [u8]) -> Result<T, ProjectError> {
    serde_json::from_slice(json).map_err(|error| ProjectError::InvalidManifest(error.to_string()))
}

fn tile_path(layer: LayerId, TileCoord { x, y }: TileCoord) -> String {
    format!("tiles/{layer}/{x}_{y}")
}

fn has_valid_channels(pixels: &[u8]) -> bool {
    pixels
        .as_chunks()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes(*bytes))
        .all(|bits| bits <= HALF_FLOAT_ONE + ROUNDING_STEPS || bits == HALF_FLOAT_NEGATIVE_ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlendMode;

    const SIZE: (u32, u32) = (600, 300);

    fn tile(half: u16) -> Vec<u8> {
        half.to_le_bytes().repeat(TILE_BYTES / 2)
    }

    fn sample_layers() -> Vec<Layer<u16>> {
        let mut base = Layer::raster(3, "Base ✿".into());
        base.properties.opacity = 0.3;
        base.properties.blend_mode = BlendMode::SoftLight;
        base.content = LayerContent::Raster(Tiles::from([
            (TileCoord::new(0, 0), 0x3800),
            (TileCoord::new(2, 1), 0x3C00),
        ]));
        let mut group = Layer::group(7, "Group".into());
        group.properties.visible = false;
        group.content = LayerContent::Group(vec![Layer::raster(9, String::new()), base]);
        vec![Layer::raster(1, "Bottom".into()), group]
    }

    fn save(layers: &[Layer<u16>]) -> Vec<u8> {
        let mut writer = ProjectWriter::default();
        for layer in layers {
            layer.visit(&mut |layer| {
                for (coord, half) in layer.tiles().into_iter().flatten() {
                    writer
                        .add_tile(layer.id, *coord, &tile(*half))
                        .expect("tile");
                }
            });
        }
        writer.finish(SIZE, layers, Some(9)).expect("finish")
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        for (path, content) in entries {
            archive
                .start_file(*path, SimpleFileOptions::default())
                .expect("entry");
            archive.write_all(content).expect("content");
        }
        archive.finish().expect("archive").into_inner()
    }

    fn open_manifest(manifest: &str) -> Result<ProjectReader, ProjectError> {
        ProjectReader::open(archive(&[(MANIFEST, manifest.as_bytes())]))
    }

    fn raster(id: u32, tiles: &str) -> String {
        format!(
            r#"{{"id":{id},"name":"a","visible":true,"opacity":1,"blendMode":"normal","kind":"raster","tiles":{tiles}}}"#
        )
    }

    fn manifest(version: u32, size: (u32, u32), layers: &str) -> String {
        format!(
            r#"{{"format":"nijimi","version":{version},"width":{},"height":{},"activeLayer":null,"layers":[{layers}]}}"#,
            size.0, size.1
        )
    }

    #[test]
    fn a_saved_project_reopens_unchanged() {
        let layers = sample_layers();
        let mut reader = ProjectReader::open(save(&layers)).expect("open");
        assert_eq!(reader.document().size(), SIZE);
        assert_eq!(reader.document().active(), Some(9));

        let mut pixels = Vec::new();
        let structure: Vec<_> = reader.document().layers().to_vec();
        let reopened: Vec<Layer<u16>> = structure
            .iter()
            .map(|layer| {
                layer.map_tiles(&mut |id, coord, ()| {
                    pixels = reader.read_tile(id, coord).expect("tile");
                    u16::from_le_bytes([pixels[0], pixels[1]])
                })
            })
            .collect();
        assert_eq!(reopened, layers);
        assert_eq!(pixels, tile(0x3C00));
    }

    #[test]
    fn version_1_files_stay_readable() {
        let json = r#"{
            "format": "nijimi", "version": 1, "width": 300, "height": 200, "activeLayer": 2,
            "addedByALaterMinorChange": true,
            "layers": [{
                "id": 5, "name": "Group", "visible": true, "opacity": 0.5, "blendMode": "softLight",
                "kind": "group", "layers": [{
                    "id": 2, "name": "Paint", "visible": false, "opacity": 1.0,
                    "blendMode": "normal", "kind": "raster", "tiles": [[1, 0]]
                }]
            }]
        }"#;
        let file = archive(&[(MANIFEST, json.as_bytes()), ("tiles/2/1_0", &tile(0x3555))]);
        let mut reader = ProjectReader::open(file).expect("open");
        assert_eq!(reader.document().size(), (300, 200));
        let group = &reader.document().layers()[0];
        assert_eq!(group.properties.opacity, 0.5);
        assert_eq!(group.properties.blend_mode, BlendMode::SoftLight);
        assert!(!group.children()[0].properties.visible);
        assert_eq!(
            reader.read_tile(2, TileCoord::new(1, 0)).expect("tile"),
            tile(0x3555)
        );
    }

    #[test]
    fn files_that_are_not_projects_are_rejected() {
        let other_format = r#"{"format":"other","version":1}"#;
        for file in [
            Vec::new(),
            b"PNG".to_vec(),
            archive(&[("mimetype", b"image/openraster")]),
            archive(&[(MANIFEST, other_format.as_bytes())]),
        ] {
            assert!(matches!(
                ProjectReader::open(file),
                Err(ProjectError::NotAProject)
            ));
        }
        let mut truncated = save(&sample_layers());
        truncated.truncate(truncated.len() / 2);
        assert!(ProjectReader::open(truncated).is_err());
    }

    #[test]
    fn other_versions_are_rejected() {
        for version in [0, VERSION + 1] {
            assert!(matches!(
                open_manifest(&manifest(version, SIZE, "")),
                Err(ProjectError::UnsupportedVersion(found)) if found == version
            ));
        }
    }

    #[test]
    fn invalid_manifests_are_rejected() {
        let too_many_layers = (1..=MAX_LAYERS as u32 + 1)
            .map(|id| raster(id, "[]"))
            .collect::<Vec<_>>()
            .join(",");
        let too_many_tiles = raster(
            1,
            &format!("[{}]", vec!["[0,0]"; MAX_PROJECT_TILES + 1].join(",")),
        );
        let nested = (0..60).fold(raster(1, "[]"), |inner, depth| {
            let id = depth + 2;
            format!(
                r#"{{"id":{id},"name":"g","visible":true,"opacity":1,"blendMode":"normal","kind":"group","layers":[{inner}]}}"#
            )
        });
        let layers = [
            raster(1, "[[0,0],[0,0]]"),
            raster(1, "[[3,0]]"),
            raster(1, "[[4294967295,4294967295]]"),
            raster(1, "[[-1,0]]"),
            raster(u32::MAX, "[]"),
            format!("{},{}", raster(1, "[]"), raster(1, "[]")),
            raster(1, "[]").replace("\"opacity\":1", "\"opacity\":1.5"),
            raster(1, "[]").replace("\"opacity\":1", "\"opacity\":1e999"),
            raster(1, "[]").replace("normal", "unknown"),
            raster(1, "[]").replace("raster", "unknown"),
            raster(1, "[]").replace(
                "\"name\":\"a\"",
                &format!("\"name\":\"{}\"", "a".repeat(257)),
            ),
            too_many_layers,
            too_many_tiles,
            nested,
        ];
        for layer in layers {
            let result = open_manifest(&manifest(1, SIZE, &layer));
            assert!(result.is_err(), "{}", &layer[..layer.len().min(200)]);
        }
        for size in [(0, 10), (10, 0)] {
            assert!(matches!(
                open_manifest(&manifest(1, size, "")),
                Err(ProjectError::InvalidSize)
            ));
        }
        let unknown_active = manifest(1, SIZE, "").replace("null", "4");
        assert!(open_manifest(&unknown_active).is_err());
        let deep_json = "[".repeat(100_000);
        assert!(open_manifest(&deep_json).is_err());
    }

    #[test]
    fn an_oversized_manifest_is_rejected() {
        let padding = " ".repeat(MAX_MANIFEST_BYTES as usize);
        assert!(open_manifest(&(manifest(1, SIZE, "") + &padding)).is_err());
    }

    #[test]
    fn damaged_tiles_are_rejected() {
        let json = manifest(1, SIZE, &raster(1, "[[0,0]]"));
        let coord = TileCoord::new(0, 0);
        let mut infinite = tile(0);
        infinite[..2].copy_from_slice(&0x7C00u16.to_le_bytes());
        let mut not_a_number = tile(0);
        not_a_number[..2].copy_from_slice(&0x7E00u16.to_le_bytes());
        let mut negative = tile(0);
        negative[..2].copy_from_slice(&0xB800u16.to_le_bytes());
        for content in [
            tile(0)[1..].to_vec(),
            [tile(0), vec![0]].concat(),
            vec![0; TILE_BYTES * 64],
            Vec::new(),
            infinite,
            not_a_number,
            negative,
        ] {
            let file = archive(&[(MANIFEST, json.as_bytes()), ("tiles/1/0_0", &content)]);
            let mut reader = ProjectReader::open(file).expect("open");
            assert!(reader.read_tile(1, coord).is_err());
        }
        let mut missing = open_manifest(&json).expect("open");
        assert!(missing.read_tile(1, coord).is_err());
    }
}
