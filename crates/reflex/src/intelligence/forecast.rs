use std::cmp::Ordering;

use super::codec::Decoder;
use super::types::{IntelligenceError, ResourceVector};

pub(super) const MAXIMUM_TYPED_FORECASTS: usize = 9;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub(crate) enum ForecastAxis {
    KernelAcceptance = 1,
    ImmediateImprovement = 2,
    UsefulDescendants = 3,
    CrossGoalLeverage = 4,
    CompressionValue = 5,
    InformationValue = 6,
    Novelty = 7,
    DeadEndRisk = 8,
    VerificationCost = 9,
}

impl ForecastAxis {
    pub(super) const fn higher_is_better(self) -> bool {
        !matches!(self, Self::DeadEndRisk | Self::VerificationCost)
    }

    pub(super) const fn default_priority(self) -> u8 {
        match self {
            Self::ImmediateImprovement => 0,
            Self::UsefulDescendants => 1,
            Self::CrossGoalLeverage => 2,
            Self::CompressionValue => 3,
            Self::InformationValue => 4,
            Self::Novelty => 5,
            Self::KernelAcceptance => 6,
            Self::DeadEndRisk => 7,
            Self::VerificationCost => 8,
        }
    }

    pub(super) fn decode(value: u8) -> Result<Self, ()> {
        match value {
            1 => Ok(Self::KernelAcceptance),
            2 => Ok(Self::ImmediateImprovement),
            3 => Ok(Self::UsefulDescendants),
            4 => Ok(Self::CrossGoalLeverage),
            5 => Ok(Self::CompressionValue),
            6 => Ok(Self::InformationValue),
            7 => Ok(Self::Novelty),
            8 => Ok(Self::DeadEndRisk),
            9 => Ok(Self::VerificationCost),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Forecast {
    axis: ForecastAxis,
    estimate: f32,
    calibration_error: f32,
    uncertainty: f32,
    support: u32,
}

impl Forecast {
    pub(super) const fn storage_placeholder() -> Self {
        Self {
            axis: ForecastAxis::KernelAcceptance,
            estimate: 0.0,
            calibration_error: 0.0,
            uncertainty: 1.0,
            support: 0,
        }
    }

    pub(super) fn calibrated(
        axis: ForecastAxis,
        estimate: f32,
        calibration_error: f32,
        support: u32,
    ) -> Result<Self, IntelligenceError> {
        if !estimate.is_finite()
            || !calibration_error.is_finite()
            || !(0.0..=1.0).contains(&estimate)
            || !(0.0..=1.0).contains(&calibration_error)
        {
            return Err(IntelligenceError::InvalidModel);
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "support is a statistical count whose conversion only controls a bounded uncertainty estimate"
        )]
        let support_scale = support.saturating_add(1) as f32;
        let uncertainty = (1.0 / support_scale.sqrt())
            .max(calibration_error)
            .clamp(0.0, 1.0);
        Self::with_uncertainty(axis, estimate, calibration_error, uncertainty, support)
    }

    pub(super) fn with_uncertainty(
        axis: ForecastAxis,
        estimate: f32,
        calibration_error: f32,
        uncertainty: f32,
        support: u32,
    ) -> Result<Self, IntelligenceError> {
        if !estimate.is_finite()
            || !calibration_error.is_finite()
            || !uncertainty.is_finite()
            || !(0.0..=1.0).contains(&estimate)
            || !(0.0..=1.0).contains(&calibration_error)
            || !(calibration_error..=1.0).contains(&uncertainty)
        {
            return Err(IntelligenceError::InvalidModel);
        }
        Ok(Self {
            axis,
            estimate,
            calibration_error,
            uncertainty,
            support,
        })
    }

    pub(crate) const fn axis(self) -> ForecastAxis {
        self.axis
    }

    pub(super) const fn estimate(self) -> f32 {
        self.estimate
    }

    #[cfg(test)]
    pub(super) const fn calibration_error(self) -> f32 {
        self.calibration_error
    }

    #[cfg(test)]
    pub(super) const fn uncertainty(self) -> f32 {
        self.uncertainty
    }

    pub(super) fn conservative_value(self) -> f32 {
        if self.axis.higher_is_better() {
            self.estimate - self.uncertainty - self.calibration_error * 0.25
        } else {
            -(self.estimate + self.uncertainty + self.calibration_error * 0.25)
        }
    }

    pub(super) fn encode_canonical(self, output: &mut Vec<u8>) {
        output.push(self.axis as u8);
        output.extend_from_slice(&self.estimate.to_bits().to_le_bytes());
        output.extend_from_slice(&self.calibration_error.to_bits().to_le_bytes());
        output.extend_from_slice(&self.uncertainty.to_bits().to_le_bytes());
        output.extend_from_slice(&self.support.to_le_bytes());
    }

    pub(super) fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let axis = ForecastAxis::decode(input.read_u8()?)?;
        let estimate = input.read_f32()?;
        let calibration_error = input.read_f32()?;
        let uncertainty = input.read_f32()?;
        let support = input.read_u32()?;
        Self::with_uncertainty(axis, estimate, calibration_error, uncertainty, support)
            .map_err(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResourceForecast {
    expected: ResourceVector,
    upper: ResourceVector,
    bounds: [ResourceBound; 5],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceBound {
    Known,
    Unknown,
}

impl ResourceForecast {
    pub(crate) const fn exact(resources: ResourceVector) -> Self {
        Self {
            expected: resources,
            upper: resources,
            bounds: [ResourceBound::Known; 5],
        }
    }

    pub(crate) const fn verification(resident_bytes: u64, durable_bytes: u64) -> Self {
        Self {
            expected: ResourceVector::new(0, resident_bytes, durable_bytes, 0, 1),
            upper: ResourceVector::new(0, resident_bytes, durable_bytes, 0, 1),
            bounds: [
                ResourceBound::Unknown,
                ResourceBound::Known,
                ResourceBound::Known,
                ResourceBound::Unknown,
                ResourceBound::Known,
            ],
        }
    }

    pub(super) const fn declared_upper(self) -> ResourceVector {
        self.upper
    }

    pub(super) const fn expected(self) -> ResourceVector {
        self.expected
    }

    pub(super) const fn bound_tags(self) -> [u8; 5] {
        let mut tags = [0_u8; 5];
        let mut index = 0;
        while index < self.bounds.len() {
            tags[index] = match self.bounds[index] {
                ResourceBound::Known => 1,
                ResourceBound::Unknown => 2,
            };
            index += 1;
        }
        tags
    }

    #[cfg(test)]
    pub(crate) const fn cpu_time_is_unknown(self) -> bool {
        matches!(self.bounds[0], ResourceBound::Unknown)
    }

    #[cfg(test)]
    pub(crate) const fn elapsed_time_is_unknown(self) -> bool {
        matches!(self.bounds[3], ResourceBound::Unknown)
    }
}

pub(super) fn compare_forecast_vectors(left: &[Forecast], right: &[Forecast]) -> Ordering {
    let mut left_index = 0;
    let mut right_index = 0;
    let mut left_better = false;
    let mut right_better = false;
    let mut tier_order = Ordering::Equal;
    while left_index < left.len() && right_index < right.len() {
        let left_priority = left[left_index].axis.default_priority();
        let right_priority = right[right_index].axis.default_priority();
        match left_priority.cmp(&right_priority) {
            Ordering::Less => left_index += 1,
            Ordering::Greater => right_index += 1,
            Ordering::Equal => {
                let objective_order = right[right_index]
                    .conservative_value()
                    .total_cmp(&left[left_index].conservative_value());
                left_better |= objective_order == Ordering::Less;
                right_better |= objective_order == Ordering::Greater;
                if tier_order == Ordering::Equal {
                    tier_order = compare_compatible_forecasts(left[left_index], right[right_index]);
                }
                left_index += 1;
                right_index += 1;
            }
        }
    }
    match (left_better, right_better) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) | (true, true) => tier_order,
    }
}

fn compare_compatible_forecasts(left: Forecast, right: Forecast) -> Ordering {
    debug_assert_eq!(left.axis, right.axis);
    right
        .conservative_value()
        .total_cmp(&left.conservative_value())
        .then_with(|| left.calibration_error.total_cmp(&right.calibration_error))
        .then_with(|| left.uncertainty.total_cmp(&right.uncertainty))
        .then_with(|| right.support.cmp(&left.support))
}
