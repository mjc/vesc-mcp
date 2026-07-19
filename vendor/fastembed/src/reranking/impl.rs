#[cfg(feature = "hf-hub")]
use anyhow::Context;
use anyhow::Result;
use ort::{session::Session, value::Value};

#[cfg(feature = "hf-hub")]
use crate::common::load_tokenizer_hf_hub;
use crate::{
    common::{init_session_builder, load_tokenizer},
    models::reranking::reranker_model_list,
    RerankerModel, RerankerModelInfo,
};
use ndarray::{s, Array};
use safetensors::{Dtype, SafeTensors};
use std::path::Path;
use tokenizers::Tokenizer;

#[cfg(feature = "hf-hub")]
use super::RerankInitOptions;
use super::{
    DEFAULT_BATCH_SIZE, EttinHead, OnnxSource, RerankInitOptionsUserDefined, RerankResult,
    TextRerank, UserDefinedRerankingModel,
};

impl TextRerank {
    fn new(tokenizer: Tokenizer, session: Session) -> Self {
        let need_token_type_ids = session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
        Self {
            tokenizer,
            session,
            need_token_type_ids,
            ettin_head: None,
        }
    }

    pub fn get_model_info(model: &RerankerModel) -> RerankerModelInfo {
        TextRerank::list_supported_models()
            .into_iter()
            .find(|m| &m.model == model)
            .expect("Model not found in supported models list. This is a bug - please report it.")
    }

    pub fn list_supported_models() -> Vec<RerankerModelInfo> {
        reranker_model_list()
    }

    #[cfg(feature = "hf-hub")]
    pub fn try_new(options: RerankInitOptions) -> Result<TextRerank> {
        use super::RerankInitOptions;
        use crate::common::pull_from_hf;

        let RerankInitOptions {
            max_length,
            model_name,
            execution_providers,
            cache_dir,
            show_download_progress,
            intra_threads,
        } = options;

        let model_repo = pull_from_hf(model_name.to_string(), cache_dir, show_download_progress)?;

        let model_file_name = TextRerank::get_model_info(&model_name).model_file;
        let model_file_reference = model_repo.get(&model_file_name).context(format!(
            "Failed to retrieve model file: {}",
            model_file_name
        ))?;
        let additional_files = TextRerank::get_model_info(&model_name).additional_files;
        for additional_file in additional_files {
            let _additional_file_reference = model_repo.get(&additional_file).context(format!(
                "Failed to retrieve additional file: {}",
                additional_file
            ))?;
        }

        let session = init_session_builder(execution_providers, intra_threads)?
            .commit_from_file(model_file_reference)?;

        let tokenizer = load_tokenizer_hf_hub(model_repo, max_length)?;
        Ok(Self::new(tokenizer, session))
    }

    /// Create a TextRerank instance from model files provided by the user.
    ///
    /// This can be used for 'bring your own' reranking models
    pub fn try_new_from_user_defined(
        model: UserDefinedRerankingModel,
        options: RerankInitOptionsUserDefined,
    ) -> Result<Self> {
        let RerankInitOptionsUserDefined {
            execution_providers,
            max_length,
            intra_threads,
        } = options;

        let mut session_builder = init_session_builder(execution_providers, intra_threads)?;
        let session = match &model.onnx_source {
            OnnxSource::Memory(bytes) => session_builder.commit_from_memory(bytes)?,
            OnnxSource::File(path) => session_builder.commit_from_file(path)?,
        };

        let tokenizer = load_tokenizer(model.tokenizer_files, max_length)?;
        Ok(Self::new(tokenizer, session))
    }

    /// Creates a user-defined Ettin reranker whose exported ONNX encoder is
    /// followed by the Sentence Transformers pooling/dense head in `root`.
    pub fn try_new_ettin_from_user_defined(
        model: UserDefinedRerankingModel,
        options: RerankInitOptionsUserDefined,
        root: impl AsRef<Path>,
    ) -> Result<Self> {
        let mut reranker = Self::try_new_from_user_defined(model, options)?;
        reranker.ettin_head = Some(EttinHead::load(root.as_ref())?);
        Ok(reranker)
    }

    /// Rerank documents using the reranker model and returns the results sorted by score in descending order.
    ///
    /// Accepts a query and a collection of documents implementing [`AsRef<str>`].
    pub fn rerank<S: AsRef<str> + Send + Sync>(
        &mut self,
        query: S,
        documents: impl AsRef<[S]>,
        return_documents: bool,
        batch_size: Option<usize>,
    ) -> Result<Vec<RerankResult>> {
        let documents = documents.as_ref();
        let batch_size = batch_size.unwrap_or(DEFAULT_BATCH_SIZE);
        anyhow::ensure!(batch_size > 0, "batch_size must be greater than 0");
        let q = query.as_ref();

        let mut scores: Vec<f32> = Vec::with_capacity(documents.len());
        for batch in documents.chunks(batch_size) {
            let inputs = batch.iter().map(|d| (q, d.as_ref())).collect();
            let encodings = self
                .tokenizer
                .encode_batch(inputs, true)
                .map_err(|e| anyhow::Error::msg(e.to_string()).context("Failed to encode batch"))?;

            let encoding_length = encodings
                .first()
                .ok_or_else(|| anyhow::anyhow!("Tokenizer returned empty encodings"))?
                .len();
            let batch_size = batch.len();
            let max_size = encoding_length * batch_size;

            let mut ids_array = Vec::with_capacity(max_size);
            let mut mask_array = Vec::with_capacity(max_size);
            let mut type_ids_array = Vec::with_capacity(max_size);

            encodings.iter().for_each(|encoding| {
                let ids = encoding.get_ids();
                let mask = encoding.get_attention_mask();
                let type_ids = encoding.get_type_ids();

                ids_array.extend(ids.iter().map(|x| *x as i64));
                mask_array.extend(mask.iter().map(|x| *x as i64));
                type_ids_array.extend(type_ids.iter().map(|x| *x as i64));
            });

            let inputs_ids_array = Array::from_shape_vec((batch_size, encoding_length), ids_array)?;
            let attention_mask_array =
                Array::from_shape_vec((batch_size, encoding_length), mask_array)?;
            let token_type_ids_array =
                Array::from_shape_vec((batch_size, encoding_length), type_ids_array)?;

            let mut session_inputs = ort::inputs![
                "input_ids" => Value::from_array(inputs_ids_array)?,
                "attention_mask" => Value::from_array(attention_mask_array)?,
            ];
            if self.need_token_type_ids {
                session_inputs.push((
                    "token_type_ids".into(),
                    Value::from_array(token_type_ids_array)?.into(),
                ));
            }

            let outputs = self.session.run(session_inputs)?;
            let batch_scores: Vec<f32> = if let Some(logits) = outputs.get("logits") {
                logits
                    .try_extract_array::<f32>()
                    .map_err(|error| {
                        anyhow::Error::msg(format!(
                            "Failed to extract logits tensor: {error}"
                        ))
                    })?
                    .slice(s![.., 0])
                    .rows()
                    .into_iter()
                    .flat_map(|row| row.to_vec())
                    .collect()
            } else if let (Some(hidden), Some(head)) =
                (outputs.get("last_hidden_state"), &self.ettin_head)
            {
                hidden
                    .try_extract_array::<f32>()
                    .map_err(|error| {
                        anyhow::Error::msg(format!(
                            "Failed to extract Ettin hidden-state tensor: {error}"
                        ))
                    })?
                    .slice(s![.., 0, ..])
                    .rows()
                    .into_iter()
                    .map(|row| head.score(row.as_slice().expect("CLS row is contiguous")))
                    .collect()
            } else {
                return Err(anyhow::Error::msg(
                    "Output does not contain compatible reranker scores",
                ));
            };
            scores.extend(batch_scores);
        }

        // Return top_n_result of type Vec<RerankResult> ordered by score in descending order, don't use binary heap
        let mut top_n_result: Vec<RerankResult> = scores
            .into_iter()
            .enumerate()
            .map(|(index, score)| RerankResult {
                document: return_documents.then(|| documents[index].as_ref().to_string()),
                score,
                index,
            })
            .collect();
        top_n_result.sort_by(|a, b| a.score.total_cmp(&b.score).reverse());
        Ok(top_n_result)
    }
}

impl EttinHead {
    fn load(root: &Path) -> Result<Self> {
        let dense = tensor(root.join("2_Dense/model.safetensors"), "linear.weight")?;
        let norm_weight = tensor(root.join("3_LayerNorm/model.safetensors"), "norm.weight")?;
        let norm_bias = tensor(root.join("3_LayerNorm/model.safetensors"), "norm.bias")?;
        let output_weight = tensor(root.join("4_Dense/model.safetensors"), "linear.weight")?;
        let output_bias = tensor(root.join("4_Dense/model.safetensors"), "linear.bias")?;
        let dimension = norm_weight.len();
        anyhow::ensure!(
            dimension > 0
                && dense.len() == dimension * dimension
                && norm_bias.len() == dimension
                && output_weight.len() == dimension
                && output_bias.len() == 1,
            "invalid Ettin reranker head dimensions"
        );
        Ok(Self {
            dense,
            norm_weight,
            norm_bias,
            output_weight,
            output_bias: output_bias[0],
            dimension,
        })
    }

    fn score(&self, cls: &[f32]) -> f32 {
        assert_eq!(cls.len(), self.dimension, "Ettin hidden dimension");
        let dimension = f32::from(u16::try_from(self.dimension).expect("Ettin dimension fits u16"));
        let mut hidden = (0..self.dimension)
            .map(|output| {
                gelu(
                    self.dense[output * self.dimension..(output + 1) * self.dimension]
                        .iter()
                        .zip(cls)
                        .map(|(weight, value)| weight * value)
                        .sum(),
                )
            })
            .collect::<Vec<_>>();
        let mean = hidden.iter().sum::<f32>() / dimension;
        let variance = hidden
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f32>()
            / dimension;
        let denominator = (variance + 1e-5).sqrt();
        for (index, value) in hidden.iter_mut().enumerate() {
            *value = (*value - mean) / denominator * self.norm_weight[index]
                + self.norm_bias[index];
        }
        hidden
            .iter()
            .zip(&self.output_weight)
            .map(|(value, weight)| value * weight)
            .sum::<f32>()
            + self.output_bias
    }
}

fn tensor(path: impl AsRef<Path>, name: &str) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path.as_ref())?;
    let tensors = SafeTensors::deserialize(&bytes)?;
    let tensor = tensors.tensor(name)?;
    anyhow::ensure!(tensor.dtype() == Dtype::F32, "Ettin head tensor is not F32");
    Ok(tensor
        .data()
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte chunk")))
        .collect())
}

fn gelu(value: f32) -> f32 {
    0.5 * value * (1.0 + erf(value / std::f32::consts::SQRT_2))
}

fn erf(value: f32) -> f32 {
    let sign = value.signum();
    let x = value.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let polynomial = (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_8) * t
        - 0.284_496_72)
        * t
        + 0.254_829_6)
        * t;
    sign * (1.0 - polynomial * (-x * x).exp())
}

#[cfg(test)]
mod tests {
    use super::{erf, gelu};

    #[test]
    fn ettin_head_activations_match_known_values() {
        assert!(erf(0.0).abs() < 1e-6);
        assert!((erf(1.0) - 0.842_700_8).abs() < 1e-6);
        assert!((gelu(1.0) - 0.841_344_7).abs() < 1e-6);
    }
}
