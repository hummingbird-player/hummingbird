#![allow(dead_code)]

use thiserror::Error;

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum InitializationError {
    #[error("Unknown device provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum SubmissionError {
    #[error("Stream stopped consuming samples (device died?)")]
    WriteTimeout,
    #[error("Unknown stream error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum ListError {
    #[error("Unknown device provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum FindError {
    #[error("Requested device does not exist")]
    DeviceDoesNotExist,
    #[error("Unknown device provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum InfoError {
    #[error("The requested device information is not available until the device is opened")]
    RequiresOpenDevice,
    #[error("The selected device is always the default device and therefore is not consistent")]
    DeviceIsDefaultAlways,
    #[error("Unsupported sample format `{0}` requested")]
    SampleFmt(String),
    #[error("The requested device information is not available")]
    None,
    #[error("Unknown device error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum OpenError {
    #[error(
        "The supplied sample format is from a different device provider than the requested device"
    )]
    InvalidConfigProvider,
    #[error("The supplied sample format is not supported by the device")]
    InvalidSampleFormat,
    #[error("Unknown device error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum CloseError {
    #[error("Unknown stream error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum StateError {
    #[error("Unknown stream error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum ResetError {
    #[error("Unknown stream error: `{0}`")]
    Unknown(String),
}
