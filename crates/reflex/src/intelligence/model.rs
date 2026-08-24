use std::collections::BTreeSet;

use crate::learning::{FtrlConversionView, LegacyFtrlHead};
use sha2::{Digest, Sha256};

use super::codec::Decoder;
use super::forecast::{Forecast, ForecastAxis};
use super::types::IntelligenceError;

const MAXIMUM_MODEL_FEATURES: usize = 256;
pub(super) const MAXIMUM_MODEL_HEADS: usize = 32;
const GROUPED_HEAD_COUNT: usize = 4;
const MINIMUM_GROUPED_FEATURES: usize = 24;
const IMPORTED_FTRL_FEATURES: usize = crate::learning::FEATURE_COUNT;
const IMPORTED_FTRL_HEADS: usize = crate::learning::HEAD_COUNT;
pub(super) const IMPORTED_FTRL_AXES: [ForecastAxis; IMPORTED_FTRL_HEADS] = [
    ForecastAxis::ImmediateImprovement,
    ForecastAxis::UsefulDescendants,
    ForecastAxis::CrossGoalLeverage,
    ForecastAxis::CompressionValue,
    ForecastAxis::KernelAcceptance,
    ForecastAxis::VerificationCost,
    ForecastAxis::DeadEndRisk,
];
const IMPORTED_FTRL_OUTPUT_ORDER: [usize; IMPORTED_FTRL_HEADS] = [0, 1, 2, 3, 4, 6, 5];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LinearHead {
    pub(super) axis: ForecastAxis,
    pub(super) bias: f32,
    pub(super) weights: Vec<f32>,
    pub(super) calibration_error: f32,
    pub(super) support: u32,
}

impl LinearHead {
    pub(crate) fn new(
        axis: ForecastAxis,
        bias: f32,
        weights: impl IntoIterator<Item = f32>,
        calibration_error: f32,
        support: u32,
    ) -> Result<Self, IntelligenceError> {
        let weights = weights.into_iter().collect::<Vec<_>>();
        if !bias.is_finite()
            || weights.is_empty()
            || weights.iter().any(|weight| !weight.is_finite())
            || !calibration_error.is_finite()
            || !(0.0..=1.0).contains(&calibration_error)
        {
            return Err(IntelligenceError::InvalidModel);
        }
        Ok(Self {
            axis,
            bias,
            weights,
            calibration_error,
            support,
        })
    }

    fn predict(&self, features: &[f32]) -> Result<Forecast, IntelligenceError> {
        let linear = self
            .weights
            .iter()
            .zip(features)
            .fold(self.bias, |sum, (weight, feature)| {
                weight.mul_add(*feature, sum)
            });
        Forecast::calibrated(
            self.axis,
            sigmoid(linear),
            self.calibration_error,
            self.support,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LinearModel {
    feature_count: u16,
    heads: Vec<LinearHead>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LinearParameters {
    axis: ForecastAxis,
    bias: f32,
    calibration_error: f32,
    support: u32,
}

impl LinearParameters {
    const fn from_head(head: &LinearHead) -> Self {
        Self {
            axis: head.axis,
            bias: head.bias,
            calibration_error: head.calibration_error,
            support: head.support,
        }
    }

    fn forecast(self, linear: f32) -> Result<Forecast, IntelligenceError> {
        Forecast::calibrated(
            self.axis,
            sigmoid(linear),
            self.calibration_error,
            self.support,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupedLinearModel {
    feature_count: u16,
    heads: Vec<LinearParameters>,
    grouped_weights: Vec<[f32; GROUPED_HEAD_COUNT]>,
    tail_weights: Vec<f32>,
}

impl GroupedLinearModel {
    fn new(feature_count: u16, heads: &[LinearHead]) -> Self {
        let feature_count_usize = usize::from(feature_count);
        let group_count = heads.len() / GROUPED_HEAD_COUNT;
        let grouped_head_count = group_count * GROUPED_HEAD_COUNT;
        let mut grouped_weights = Vec::with_capacity(group_count * feature_count_usize);
        for group in 0..group_count {
            let first_head = group * GROUPED_HEAD_COUNT;
            for feature in 0..feature_count_usize {
                grouped_weights.push([
                    heads[first_head].weights[feature],
                    heads[first_head + 1].weights[feature],
                    heads[first_head + 2].weights[feature],
                    heads[first_head + 3].weights[feature],
                ]);
            }
        }
        let tail_head_count = heads.len() - grouped_head_count;
        let mut tail_weights = Vec::with_capacity(tail_head_count * feature_count_usize);
        for head in &heads[grouped_head_count..] {
            tail_weights.extend_from_slice(&head.weights);
        }
        let parameters = heads.iter().map(LinearParameters::from_head).collect();
        Self {
            feature_count,
            heads: parameters,
            grouped_weights,
            tail_weights,
        }
    }
    fn forecast_into(
        &self,
        features: &[f32],
        output: &mut Vec<Forecast>,
    ) -> Result<(), IntelligenceError> {
        let head_count = self.heads.len();
        let feature_count = usize::from(self.feature_count);
        let group_count = head_count / GROUPED_HEAD_COUNT;
        let grouped_head_count = group_count * GROUPED_HEAD_COUNT;
        let mut linear = [0.0_f32; MAXIMUM_MODEL_HEADS];
        for (sum, head) in linear.iter_mut().zip(&self.heads) {
            *sum = head.bias;
        }
        for group in 0..group_count {
            let head_start = group * GROUPED_HEAD_COUNT;
            let weight_start = group * feature_count;
            accumulate_four_heads(
                &mut linear[head_start..head_start + GROUPED_HEAD_COUNT],
                features,
                &self.grouped_weights[weight_start..weight_start + feature_count],
            );
        }
        for (head, sum) in linear
            .iter_mut()
            .enumerate()
            .take(head_count)
            .skip(grouped_head_count)
        {
            let tail_head = head - grouped_head_count;
            let weight_start = tail_head * feature_count;
            *sum = ordered_dot_product(
                *sum,
                &self.tail_weights[weight_start..weight_start + feature_count],
                features,
            );
        }
        for (head, linear) in self.heads.iter().zip(linear) {
            output.push(head.forecast(linear)?);
        }
        Ok(())
    }

    fn weight(&self, head: usize, feature: usize) -> f32 {
        let feature_count = usize::from(self.feature_count);
        let grouped_head_count = (self.heads.len() / GROUPED_HEAD_COUNT) * GROUPED_HEAD_COUNT;
        if head < grouped_head_count {
            self.grouped_weights[(head / GROUPED_HEAD_COUNT) * feature_count + feature]
                [head % GROUPED_HEAD_COUNT]
        } else {
            self.tail_weights[(head - grouped_head_count) * feature_count + feature]
        }
    }
}

fn encode_linear_metadata(
    axis: ForecastAxis,
    bias: f32,
    calibration_error: f32,
    support: u32,
    output: &mut Vec<u8>,
) {
    output.push(axis as u8);
    output.extend_from_slice(&bias.to_bits().to_le_bytes());
    output.extend_from_slice(&calibration_error.to_bits().to_le_bytes());
    output.extend_from_slice(&support.to_le_bytes());
}

#[inline]
fn ordered_dot_product(mut sum: f32, weights: &[f32], features: &[f32]) -> f32 {
    let mut weight_chunks = weights.chunks_exact(GROUPED_HEAD_COUNT);
    let mut feature_chunks = features.chunks_exact(GROUPED_HEAD_COUNT);
    for (weights, features) in weight_chunks.by_ref().zip(feature_chunks.by_ref()) {
        sum = weights[0].mul_add(features[0], sum);
        sum = weights[1].mul_add(features[1], sum);
        sum = weights[2].mul_add(features[2], sum);
        sum = weights[3].mul_add(features[3], sum);
    }
    for (weight, feature) in weight_chunks
        .remainder()
        .iter()
        .zip(feature_chunks.remainder())
    {
        sum = weight.mul_add(*feature, sum);
    }
    sum
}

#[inline]
fn accumulate_four_heads(
    output: &mut [f32],
    features: &[f32],
    weights: &[[f32; GROUPED_HEAD_COUNT]],
) {
    let [sum_0, sum_1, sum_2, sum_3] = output else {
        unreachable!("the grouped inference kernel always receives four accumulators");
    };
    for (weights, feature) in weights.iter().zip(features.iter().copied()) {
        *sum_0 = weights[0].mul_add(feature, *sum_0);
        *sum_1 = weights[1].mul_add(feature, *sum_1);
        *sum_2 = weights[2].mul_add(feature, *sum_2);
        *sum_3 = weights[3].mul_add(feature, *sum_3);
    }
}

fn sigmoid(linear: f32) -> f32 {
    if linear >= 0.0 {
        1.0 / (1.0 + (-linear).exp())
    } else {
        let exponential = linear.exp();
        exponential / (1.0 + exponential)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PriorHead {
    axis: ForecastAxis,
    estimate: f32,
    calibration_error: f32,
    support: u32,
}

impl PriorHead {
    pub(crate) fn new(
        axis: ForecastAxis,
        estimate: f32,
        calibration_error: f32,
        support: u32,
    ) -> Result<Self, IntelligenceError> {
        Forecast::calibrated(axis, estimate, calibration_error, support)?;
        Ok(Self {
            axis,
            estimate,
            calibration_error,
            support,
        })
    }

    fn forecast(self) -> Result<Forecast, IntelligenceError> {
        Forecast::calibrated(
            self.axis,
            self.estimate,
            self.calibration_error,
            self.support,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PriorModel {
    heads: Vec<PriorHead>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImportedFtrlModel {
    weights: [[f32; IMPORTED_FTRL_HEADS]; IMPORTED_FTRL_FEATURES],
    second_moments: [[f32; IMPORTED_FTRL_HEADS]; IMPORTED_FTRL_FEATURES],
    calibration_counts: [u64; IMPORTED_FTRL_HEADS],
    calibration_errors: [f32; IMPORTED_FTRL_HEADS],
}

impl ImportedFtrlModel {
    fn import(conversion: FtrlConversionView<'_>) -> Result<Self, IntelligenceError> {
        let expected_heads = [
            LegacyFtrlHead::ImmediateImprovement,
            LegacyFtrlHead::UsefulDescendants,
            LegacyFtrlHead::CrossGoalLeverage,
            LegacyFtrlHead::CompressionValue,
            LegacyFtrlHead::KernelAcceptance,
            LegacyFtrlHead::VerificationCost,
            LegacyFtrlHead::DeadEndRisk,
        ];
        if FtrlConversionView::semantic_heads() != &expected_heads {
            return Err(IntelligenceError::InvalidModel);
        }
        Self::new(
            conversion.weights(),
            conversion.second_moments(),
            conversion.calibration_counts(),
            conversion.calibration_errors(),
        )
    }

    fn new(
        weights: &[[f32; IMPORTED_FTRL_HEADS]; IMPORTED_FTRL_FEATURES],
        second_moments: &[[f32; IMPORTED_FTRL_HEADS]; IMPORTED_FTRL_FEATURES],
        calibration_counts: &[u64; IMPORTED_FTRL_HEADS],
        calibration_errors: &[f32; IMPORTED_FTRL_HEADS],
    ) -> Result<Self, IntelligenceError> {
        if weights.iter().flatten().any(|weight| !weight.is_finite())
            || second_moments
                .iter()
                .flatten()
                .any(|moment| !moment.is_finite() || *moment < 0.0)
            || calibration_errors
                .iter()
                .any(|error| !error.is_finite() || !(0.0..=1.0).contains(error))
            || calibration_counts
                .iter()
                .zip(calibration_errors)
                .any(|(count, error)| *count == 0 && *error != 0.0)
        {
            return Err(IntelligenceError::InvalidModel);
        }
        Ok(Self {
            weights: *weights,
            second_moments: *second_moments,
            calibration_counts: *calibration_counts,
            calibration_errors: *calibration_errors,
        })
    }

    fn forecast_into(
        &self,
        features: &[f32],
        output: &mut Vec<Forecast>,
    ) -> Result<(), IntelligenceError> {
        let mut linear = [0.0_f32; IMPORTED_FTRL_HEADS];
        let mut support = [0.0_f32; IMPORTED_FTRL_HEADS];
        for (feature_index, feature) in features.iter().copied().enumerate() {
            for head in 0..IMPORTED_FTRL_HEADS {
                linear[head] += self.weights[feature_index][head] * feature;
                support[head] += self.second_moments[feature_index][head] * feature * feature;
            }
        }
        let estimates = imported_sigmoid_heads(linear);
        for head in IMPORTED_FTRL_OUTPUT_ORDER {
            let calibration_error = if self.calibration_counts[head] == 0 {
                1.0
            } else {
                self.calibration_errors[head]
            };
            let epistemic = (1.0 / (1.0 + support[head].max(0.0))).sqrt();
            output.push(Forecast::with_uncertainty(
                IMPORTED_FTRL_AXES[head],
                estimates[head],
                calibration_error,
                epistemic.max(calibration_error).clamp(0.0, 1.0),
                u32::try_from(self.calibration_counts[head]).unwrap_or(u32::MAX),
            )?);
        }
        Ok(())
    }
}

fn imported_sigmoid_heads(linear: [f32; IMPORTED_FTRL_HEADS]) -> [f32; IMPORTED_FTRL_HEADS] {
    let mut estimates = [0.0_f32; IMPORTED_FTRL_HEADS];
    for head in 0..IMPORTED_FTRL_HEADS {
        estimates[head] = (0..head)
            .find(|previous| linear[*previous].to_bits() == linear[head].to_bits())
            .map_or_else(|| sigmoid(linear[head]), |previous| estimates[previous]);
    }
    estimates
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CompactModel {
    Linear(LinearModel),
    GroupedLinear(Box<GroupedLinearModel>),
    Prior(PriorModel),
    ImportedFtrl(Box<ImportedFtrlModel>),
}

impl CompactModel {
    pub(crate) fn import_ftrl(
        conversion: FtrlConversionView<'_>,
    ) -> Result<Self, IntelligenceError> {
        ImportedFtrlModel::import(conversion).map(|model| Self::ImportedFtrl(Box::new(model)))
    }

    pub(crate) fn linear(
        feature_count: usize,
        heads: impl IntoIterator<Item = LinearHead>,
    ) -> Result<Self, IntelligenceError> {
        let feature_count = u16::try_from(feature_count)
            .ok()
            .filter(|count| *count != 0 && usize::from(*count) <= MAXIMUM_MODEL_FEATURES)
            .ok_or(IntelligenceError::InvalidModel)?;
        let mut heads = heads.into_iter().collect::<Vec<_>>();
        let axes = heads.iter().map(|head| head.axis).collect::<BTreeSet<_>>();
        if heads.is_empty()
            || heads.len() > MAXIMUM_MODEL_HEADS
            || axes.len() != heads.len()
            || heads
                .iter()
                .any(|head| head.weights.len() != usize::from(feature_count))
        {
            return Err(IntelligenceError::InvalidModel);
        }
        heads.sort_unstable_by_key(|head| head.axis.default_priority());
        if heads.len() >= GROUPED_HEAD_COUNT
            && usize::from(feature_count) >= MINIMUM_GROUPED_FEATURES
        {
            Ok(Self::GroupedLinear(Box::new(GroupedLinearModel::new(
                feature_count,
                &heads,
            ))))
        } else {
            Ok(Self::Linear(LinearModel {
                feature_count,
                heads,
            }))
        }
    }

    pub(crate) fn prior(
        heads: impl IntoIterator<Item = PriorHead>,
    ) -> Result<Self, IntelligenceError> {
        let mut heads = heads.into_iter().collect::<Vec<_>>();
        let axes = heads.iter().map(|head| head.axis).collect::<BTreeSet<_>>();
        if heads.is_empty() || heads.len() > MAXIMUM_MODEL_HEADS || axes.len() != heads.len() {
            return Err(IntelligenceError::InvalidModel);
        }
        heads.sort_unstable_by_key(|head| head.axis.default_priority());
        Ok(Self::Prior(PriorModel { heads }))
    }

    pub(crate) fn content_identity(&self) -> [u8; 32] {
        let mut encoded = Vec::new();
        self.encode_canonical(&mut encoded);
        Sha256::digest(encoded).into()
    }

    pub(super) fn forecast_into(
        &self,
        features: &[f32],
        output: &mut Vec<Forecast>,
    ) -> Result<(u32, u16), IntelligenceError> {
        let start = u32::try_from(output.len()).map_err(|_| IntelligenceError::CapacityExceeded)?;
        match self {
            Self::Linear(model) => {
                if features.len() != usize::from(model.feature_count) {
                    return Err(IntelligenceError::InvalidFeature);
                }
                for head in &model.heads {
                    output.push(head.predict(features)?);
                }
                Ok((
                    start,
                    u16::try_from(model.heads.len())
                        .map_err(|_| IntelligenceError::CapacityExceeded)?,
                ))
            }
            Self::GroupedLinear(model) => {
                if features.len() != usize::from(model.feature_count) {
                    return Err(IntelligenceError::InvalidFeature);
                }
                model.forecast_into(features, output)?;
                Ok((
                    start,
                    u16::try_from(model.heads.len())
                        .map_err(|_| IntelligenceError::CapacityExceeded)?,
                ))
            }
            Self::Prior(model) => {
                for head in &model.heads {
                    output.push(head.forecast()?);
                }
                Ok((
                    start,
                    u16::try_from(model.heads.len())
                        .map_err(|_| IntelligenceError::CapacityExceeded)?,
                ))
            }
            Self::ImportedFtrl(model) => {
                if features.len() != IMPORTED_FTRL_FEATURES {
                    return Err(IntelligenceError::InvalidFeature);
                }
                model.forecast_into(features, output)?;
                Ok((start, u16::try_from(IMPORTED_FTRL_HEADS).unwrap()))
            }
        }
    }

    pub(super) fn accepts_feature_count(&self, feature_count: usize) -> bool {
        match self {
            Self::Linear(model) => usize::from(model.feature_count) == feature_count,
            Self::GroupedLinear(model) => usize::from(model.feature_count) == feature_count,
            Self::Prior(_) => true,
            Self::ImportedFtrl(_) => feature_count == IMPORTED_FTRL_FEATURES,
        }
    }

    pub(super) fn forecast_count(&self) -> usize {
        match self {
            Self::Linear(model) => model.heads.len(),
            Self::GroupedLinear(model) => model.heads.len(),
            Self::Prior(model) => model.heads.len(),
            Self::ImportedFtrl(_) => IMPORTED_FTRL_HEADS,
        }
    }

    pub(super) fn minimum_support(&self) -> u32 {
        match self {
            Self::Linear(model) => model.heads.iter().map(|head| head.support).min(),
            Self::GroupedLinear(model) => model.heads.iter().map(|head| head.support).min(),
            Self::Prior(model) => model.heads.iter().map(|head| head.support).min(),
            Self::ImportedFtrl(model) => model
                .calibration_counts
                .iter()
                .map(|count| u32::try_from(*count).unwrap_or(u32::MAX))
                .min(),
        }
        .unwrap_or_default()
    }

    pub(super) fn resident_bytes(&self) -> usize {
        match self {
            Self::Linear(model) => std::mem::size_of::<Self>()
                .saturating_add(
                    model
                        .heads
                        .capacity()
                        .saturating_mul(std::mem::size_of::<LinearHead>()),
                )
                .saturating_add(model.heads.iter().fold(0_usize, |bytes, head| {
                    bytes.saturating_add(
                        head.weights
                            .capacity()
                            .saturating_mul(std::mem::size_of::<f32>()),
                    )
                })),
            Self::GroupedLinear(model) => std::mem::size_of::<Self>()
                .saturating_add(std::mem::size_of::<GroupedLinearModel>())
                .saturating_add(
                    model
                        .heads
                        .capacity()
                        .saturating_mul(std::mem::size_of::<LinearParameters>()),
                )
                .saturating_add(
                    model
                        .grouped_weights
                        .capacity()
                        .saturating_mul(std::mem::size_of::<[f32; GROUPED_HEAD_COUNT]>()),
                )
                .saturating_add(
                    model
                        .tail_weights
                        .capacity()
                        .saturating_mul(std::mem::size_of::<f32>()),
                ),
            Self::Prior(model) => std::mem::size_of::<Self>().saturating_add(
                model
                    .heads
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PriorHead>()),
            ),
            Self::ImportedFtrl(_) => {
                std::mem::size_of::<Self>().saturating_add(std::mem::size_of::<ImportedFtrlModel>())
            }
        }
    }

    pub(super) fn encode_canonical(&self, output: &mut Vec<u8>) {
        match self {
            Self::Linear(model) => {
                output.push(1);
                output.extend_from_slice(&model.feature_count.to_le_bytes());
                output.extend_from_slice(&(model.heads.len() as u64).to_le_bytes());
                for head in &model.heads {
                    encode_linear_metadata(
                        head.axis,
                        head.bias,
                        head.calibration_error,
                        head.support,
                        output,
                    );
                    for weight in &head.weights {
                        output.extend_from_slice(&weight.to_bits().to_le_bytes());
                    }
                }
            }
            Self::GroupedLinear(model) => {
                output.push(1);
                output.extend_from_slice(&model.feature_count.to_le_bytes());
                output.extend_from_slice(&(model.heads.len() as u64).to_le_bytes());
                for (head_index, head) in model.heads.iter().enumerate() {
                    encode_linear_metadata(
                        head.axis,
                        head.bias,
                        head.calibration_error,
                        head.support,
                        output,
                    );
                    for feature in 0..usize::from(model.feature_count) {
                        output.extend_from_slice(
                            &model.weight(head_index, feature).to_bits().to_le_bytes(),
                        );
                    }
                }
            }
            Self::Prior(model) => {
                output.push(2);
                output.extend_from_slice(&(model.heads.len() as u64).to_le_bytes());
                for head in &model.heads {
                    output.push(head.axis as u8);
                    output.extend_from_slice(&head.estimate.to_bits().to_le_bytes());
                    output.extend_from_slice(&head.calibration_error.to_bits().to_le_bytes());
                    output.extend_from_slice(&head.support.to_le_bytes());
                }
            }
            Self::ImportedFtrl(model) => {
                output.push(3);
                output.extend_from_slice(
                    &u16::try_from(IMPORTED_FTRL_FEATURES)
                        .expect("the fixed legacy FTRL feature count fits u16")
                        .to_le_bytes(),
                );
                output.push(
                    u8::try_from(IMPORTED_FTRL_HEADS)
                        .expect("the fixed legacy FTRL head count fits u8"),
                );
                output.extend(IMPORTED_FTRL_AXES.iter().map(|axis| *axis as u8));
                for values in [&model.weights, &model.second_moments] {
                    for feature in values {
                        for value in feature {
                            output.extend_from_slice(&value.to_bits().to_le_bytes());
                        }
                    }
                }
                for count in model.calibration_counts {
                    output.extend_from_slice(&count.to_le_bytes());
                }
                for error in model.calibration_errors {
                    output.extend_from_slice(&error.to_bits().to_le_bytes());
                }
            }
        }
    }

    pub(super) fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        match input.read_u8()? {
            1 => {
                let feature_count = usize::from(input.read_u16()?);
                let head_count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
                let bytes_per_head = 13_usize
                    .checked_add(feature_count.checked_mul(4).ok_or(())?)
                    .ok_or(())?;
                if feature_count == 0
                    || feature_count > MAXIMUM_MODEL_FEATURES
                    || head_count == 0
                    || head_count > MAXIMUM_MODEL_HEADS
                    || head_count > input.remaining().checked_div(bytes_per_head).unwrap_or(0)
                {
                    return Err(());
                }
                let mut heads = Vec::with_capacity(head_count);
                for _ in 0..head_count {
                    let axis = ForecastAxis::decode(input.read_u8()?)?;
                    let bias = input.read_f32()?;
                    let calibration_error = input.read_f32()?;
                    let support = input.read_u32()?;
                    let mut weights = Vec::with_capacity(feature_count);
                    for _ in 0..feature_count {
                        weights.push(input.read_f32()?);
                    }
                    heads.push(
                        LinearHead::new(axis, bias, weights, calibration_error, support)
                            .map_err(|_| ())?,
                    );
                }
                Self::linear(feature_count, heads).map_err(|_| ())
            }
            2 => {
                let head_count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
                if head_count == 0
                    || head_count > MAXIMUM_MODEL_HEADS
                    || head_count > input.remaining().saturating_div(13)
                {
                    return Err(());
                }
                let mut heads = Vec::with_capacity(head_count);
                for _ in 0..head_count {
                    heads.push(
                        PriorHead::new(
                            ForecastAxis::decode(input.read_u8()?)?,
                            input.read_f32()?,
                            input.read_f32()?,
                            input.read_u32()?,
                        )
                        .map_err(|_| ())?,
                    );
                }
                Self::prior(heads).map_err(|_| ())
            }
            3 => {
                if usize::from(input.read_u16()?) != IMPORTED_FTRL_FEATURES
                    || usize::from(input.read_u8()?) != IMPORTED_FTRL_HEADS
                {
                    return Err(());
                }
                for expected in IMPORTED_FTRL_AXES {
                    if ForecastAxis::decode(input.read_u8()?)? != expected {
                        return Err(());
                    }
                }
                let mut weights = [[0.0_f32; IMPORTED_FTRL_HEADS]; IMPORTED_FTRL_FEATURES];
                let mut second_moments = [[0.0_f32; IMPORTED_FTRL_HEADS]; IMPORTED_FTRL_FEATURES];
                for values in [&mut weights, &mut second_moments] {
                    for feature in values {
                        for value in feature {
                            *value = input.read_f32()?;
                        }
                    }
                }
                let mut calibration_counts = [0_u64; IMPORTED_FTRL_HEADS];
                for count in &mut calibration_counts {
                    *count = input.read_u64()?;
                }
                let mut calibration_errors = [0.0_f32; IMPORTED_FTRL_HEADS];
                for error in &mut calibration_errors {
                    *error = input.read_f32()?;
                }
                ImportedFtrlModel::new(
                    &weights,
                    &second_moments,
                    &calibration_counts,
                    &calibration_errors,
                )
                .map(|model| Self::ImportedFtrl(Box::new(model)))
                .map_err(|_| ())
            }
            _ => Err(()),
        }
    }
}

#[cfg(test)]
#[path = "model_bench.rs"]
mod performance_tests;
