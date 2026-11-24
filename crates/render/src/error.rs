use std::{error::Error, fmt};

use nijimi_core::{DocumentError, ProjectError};

#[derive(Debug)]
pub enum EngineError {
    DeviceLost,
    EmptyImage,
    ImageTooLarge { limit: u32 },
    PixelCount { expected: u64, actual: u64 },
    NoActiveLayer,
    InvalidBrush,
    InvalidStroke,
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
                write!(
                    formatter,
                    "Expected {expected} bytes of RGBA, got {actual}."
                )
            }
            Self::NoActiveLayer => write!(formatter, "No layer is selected."),
            Self::InvalidBrush => write!(formatter, "A brush value is out of range."),
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
