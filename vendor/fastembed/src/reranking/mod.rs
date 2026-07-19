const DEFAULT_MAX_LENGTH: usize = 512;
const DEFAULT_BATCH_SIZE: usize = 256;

mod init;
pub use init::*;

mod r#impl;

#[derive(Debug)]
pub(crate) struct EttinHead {
    dense: Vec<f32>,
    norm_weight: Vec<f32>,
    norm_bias: Vec<f32>,
    output_weight: Vec<f32>,
    output_bias: f32,
    dimension: usize,
}
