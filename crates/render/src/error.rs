use std::{error::Error, fmt};

use nijimi_core::{CubeError, DocumentError, ProjectError};

#[derive(Debug)]
pub enum EngineError {
    DeviceLost,
    EmptyImage,
    ImageTooLarge { limit: u32 },
    PixelCount { expected: u64, actual: u64 },
    NoActiveLayer,
    InvalidBrush,
    InvalidMaskValue,
    AdjustmentExport,
    Cube(CubeError),
    InvalidStroke,
    InvalidRegion,
    InvalidTransform,
    UnsupportedPreview,
    InvalidFill,
    StaleResult,
    Document(DocumentError),
    Project(ProjectError),
    OutOfGpuMemory,
    Gpu(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceLost => write!(formatter, "The GPU device was lost."),
            Self::EmptyImage => write!(formatter, "The image has no pixels."),
            Self::ImageTooLarge { limit } => {
                write!(formatter, "Images can be at most {limit} px per side.")
            }
            Self::PixelCount { expected, actual } => {
                write!(formatter, "Expected {expected} pixel bytes, got {actual}.")
            }
            Self::NoActiveLayer => write!(formatter, "No layer is selected."),
            Self::InvalidMaskValue => write!(formatter, "Mask paint must be between 0 and 1."),
            Self::AdjustmentExport => write!(
                formatter,
                "An adjustment uses the layers below it. Export the composite or its group."
            ),
            Self::Cube(error) => error.fmt(formatter),
            Self::InvalidBrush => write!(formatter, "A brush value is out of range."),
            Self::InvalidRegion => write!(formatter, "The region must lie inside the canvas."),
            Self::InvalidTransform => write!(formatter, "A transform value is out of range."),
            Self::UnsupportedPreview => write!(formatter, "This command cannot be previewed."),
            Self::InvalidFill => write!(formatter, "A fill or wand value is out of range."),
            Self::StaleResult => write!(
                formatter,
                "The document changed while the edit was running."
            ),
            Self::InvalidStroke => write!(formatter, "A stroke point is out of range."),
            Self::Document(error) => error.fmt(formatter),
            Self::Project(error) => error.fmt(formatter),
            Self::OutOfGpuMemory => write!(formatter, "The GPU is out of memory."),
            Self::Gpu(reason) => write!(formatter, "The GPU reported an error: {reason}"),
        }
    }
}

impl Error for EngineError {}

impl From<DocumentError> for EngineError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

impl From<CubeError> for EngineError {
    fn from(error: CubeError) -> Self {
        Self::Cube(error)
    }
}

impl From<ProjectError> for EngineError {
    fn from(error: ProjectError) -> Self {
        Self::Project(error)
    }
}

impl From<wgpu::Error> for EngineError {
    fn from(error: wgpu::Error) -> Self {
        match error {
            wgpu::Error::OutOfMemory { .. } => Self::OutOfGpuMemory,
            other => Self::Gpu(other.to_string()),
        }
    }
}
