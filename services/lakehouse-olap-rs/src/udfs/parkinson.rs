use std::any::Any;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, BinaryArray, Int64Array};
use arrow::datatypes::{DataType, Field, FieldRef};
use datafusion::common::{exec_err, Result as DfResult, ScalarValue};
use datafusion::logical_expr::function::{AccumulatorArgs, StateFieldsArgs};
use datafusion::logical_expr::{
    Accumulator, AggregateUDF, AggregateUDFImpl, Signature, TypeSignature, Volatility,
};

use crate::math_fp::{evaluate_parkinson_from_q64_sum, log2_ratio_q32};

const PARKINSON_STATE_BYTES: usize = 57;

#[derive(Debug)]
pub struct RealizedVolatilityParkinsonUdaf {
    signature: Signature,
}

impl Default for RealizedVolatilityParkinsonUdaf {
    fn default() -> Self {
        Self::new()
    }
}

impl RealizedVolatilityParkinsonUdaf {
    pub fn new() -> Self {
        Self {
            signature: Signature::one_of(
                vec![
                    // 2-arg: realized_volatility_parkinson(high_fp, low_fp)
                    TypeSignature::Exact(vec![DataType::Int64, DataType::Int64]),
                    // 3-arg raw tick bucketed: realized_volatility_parkinson(price_fp, timestamp_ns, bucket_width_ns)
                    TypeSignature::Exact(vec![DataType::Int64, DataType::Int64, DataType::Int64]),
                ],
                Volatility::Immutable,
            ),
        }
    }

    pub fn into_aggregate_udf() -> AggregateUDF {
        AggregateUDF::from(Self::new())
    }
}

impl AggregateUDFImpl for RealizedVolatilityParkinsonUdaf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "realized_volatility_parkinson"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> DfResult<DataType> {
        Ok(DataType::Int64)
    }

    fn state_fields(&self, _args: StateFieldsArgs) -> DfResult<Vec<FieldRef>> {
        Ok(vec![Arc::new(Field::new(
            "parkinson_state_bin",
            DataType::Binary,
            true,
        ))])
    }

    fn accumulator(&self, _acc_args: AccumulatorArgs) -> DfResult<Box<dyn Accumulator>> {
        Ok(Box::new(ParkinsonAccumulator::default()))
    }
}

#[derive(Debug, Default)]
struct ParkinsonAccumulator {
    sum_log2_sq_q64: u128,
    count: u64,
    has_open_bucket: bool,
    open_bucket_id: i64,
    open_high_fp: i64,
    open_low_fp: i64,
    bucket_width_ns: i64,
}

impl ParkinsonAccumulator {
    #[inline]
    fn commit_hl_bar(&mut self, high_fp: i64, low_fp: i64) {
        if let Some(q32) = log2_ratio_q32(high_fp, low_fp) {
            let sq = (q32 as u128) * (q32 as u128);
            self.sum_log2_sq_q64 = self.sum_log2_sq_q64.saturating_add(sq);
            self.count += 1;
        }
    }

    #[inline]
    fn flush_open_bucket(&mut self) {
        if self.has_open_bucket {
            let h = self.open_high_fp;
            let l = self.open_low_fp;
            self.has_open_bucket = false;
            self.commit_hl_bar(h, l);
        }
    }

    fn to_bytes(&self) -> [u8; PARKINSON_STATE_BYTES] {
        let mut out = [0_u8; PARKINSON_STATE_BYTES];
        out[0..16].copy_from_slice(&self.sum_log2_sq_q64.to_le_bytes());
        out[16..24].copy_from_slice(&self.count.to_le_bytes());
        out[24] = u8::from(self.has_open_bucket);
        out[25..33].copy_from_slice(&self.open_bucket_id.to_le_bytes());
        out[33..41].copy_from_slice(&self.open_high_fp.to_le_bytes());
        out[41..49].copy_from_slice(&self.open_low_fp.to_le_bytes());
        out[49..57].copy_from_slice(&self.bucket_width_ns.to_le_bytes());
        out
    }

    fn merge_from_bytes(&mut self, raw: &[u8]) -> DfResult<()> {
        if raw.len() != PARKINSON_STATE_BYTES {
            return exec_err!("Invalid ParkinsonAccumulator state length");
        }
        let other_sum = u128::from_le_bytes(raw[0..16].try_into().unwrap());
        let other_cnt = u64::from_le_bytes(raw[16..24].try_into().unwrap());
        let other_has_open = raw[24] != 0;
        let other_bucket_id = i64::from_le_bytes(raw[25..33].try_into().unwrap());
        let other_high = i64::from_le_bytes(raw[33..41].try_into().unwrap());
        let other_low = i64::from_le_bytes(raw[41..49].try_into().unwrap());
        let other_width = i64::from_le_bytes(raw[49..57].try_into().unwrap());

        self.sum_log2_sq_q64 = self.sum_log2_sq_q64.saturating_add(other_sum);
        self.count += other_cnt;

        if other_has_open {
            if !self.has_open_bucket {
                self.has_open_bucket = true;
                self.open_bucket_id = other_bucket_id;
                self.open_high_fp = other_high;
                self.open_low_fp = other_low;
                self.bucket_width_ns = other_width;
            } else if self.open_bucket_id == other_bucket_id {
                self.open_high_fp = self.open_high_fp.max(other_high);
                self.open_low_fp = self.open_low_fp.min(other_low);
            } else {
                // Non-matching boundary buckets: commit the older one, retain the newer one
                if other_bucket_id > self.open_bucket_id {
                    self.flush_open_bucket();
                    self.has_open_bucket = true;
                    self.open_bucket_id = other_bucket_id;
                    self.open_high_fp = other_high;
                    self.open_low_fp = other_low;
                    self.bucket_width_ns = other_width;
                } else {
                    self.commit_hl_bar(other_high, other_low);
                }
            }
        }
        Ok(())
    }
}

impl Accumulator for ParkinsonAccumulator {
    fn update_batch(&mut self, values: &[ArrayRef]) -> DfResult<()> {
        if values.len() == 2 {
            let highs = values[0]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "realized_volatility_parkinson arg 0 must be Int64Array".into(),
                    )
                })?;
            let lows = values[1]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "realized_volatility_parkinson arg 1 must be Int64Array".into(),
                    )
                })?;

            let n = highs.len();
            if highs.null_count() == 0 && lows.null_count() == 0 {
                let h_s = highs.values();
                let l_s = lows.values();
                for i in 0..n {
                    self.commit_hl_bar(h_s[i], l_s[i]);
                }
            } else {
                for i in 0..n {
                    if !highs.is_null(i) && !lows.is_null(i) {
                        self.commit_hl_bar(highs.value(i), lows.value(i));
                    }
                }
            }
            Ok(())
        } else if values.len() == 3 {
            let prices = values[0]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "realized_volatility_parkinson arg 0 must be Int64Array".into(),
                    )
                })?;
            let timestamps = values[1]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "realized_volatility_parkinson arg 1 must be Int64Array".into(),
                    )
                })?;
            let widths = values[2]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "realized_volatility_parkinson arg 2 must be Int64Array".into(),
                    )
                })?;

            let n = prices.len();
            for i in 0..n {
                if prices.is_null(i) || timestamps.is_null(i) || widths.is_null(i) {
                    continue;
                }
                let p = prices.value(i);
                let ts = timestamps.value(i);
                let w = widths.value(i);
                if w <= 0 || p <= 0 {
                    continue;
                }
                let b_id = ts.div_euclid(w);
                if !self.has_open_bucket {
                    self.has_open_bucket = true;
                    self.open_bucket_id = b_id;
                    self.open_high_fp = p;
                    self.open_low_fp = p;
                    self.bucket_width_ns = w;
                } else if b_id == self.open_bucket_id {
                    if p > self.open_high_fp {
                        self.open_high_fp = p;
                    }
                    if p < self.open_low_fp {
                        self.open_low_fp = p;
                    }
                } else {
                    self.flush_open_bucket();
                    self.has_open_bucket = true;
                    self.open_bucket_id = b_id;
                    self.open_high_fp = p;
                    self.open_low_fp = p;
                    self.bucket_width_ns = w;
                }
            }
            Ok(())
        } else {
            exec_err!("realized_volatility_parkinson expects 2 or 3 arguments")
        }
    }

    fn evaluate(&mut self) -> DfResult<ScalarValue> {
        let mut total_sum = self.sum_log2_sq_q64;
        let mut total_cnt = self.count;
        if self.has_open_bucket {
            if let Some(q32) = log2_ratio_q32(self.open_high_fp, self.open_low_fp) {
                total_sum = total_sum.saturating_add((q32 as u128) * (q32 as u128));
                total_cnt += 1;
            }
        }
        Ok(ScalarValue::Int64(evaluate_parkinson_from_q64_sum(
            total_sum, total_cnt,
        )))
    }

    fn size(&self) -> usize {
        std::mem::size_of_val(self)
    }

    fn state(&mut self) -> DfResult<Vec<ScalarValue>> {
        if self.count == 0 && !self.has_open_bucket {
            Ok(vec![ScalarValue::Binary(None)])
        } else {
            Ok(vec![ScalarValue::Binary(Some(self.to_bytes().to_vec()))])
        }
    }

    fn merge_batch(&mut self, states: &[ArrayRef]) -> DfResult<()> {
        if states.is_empty() {
            return Ok(());
        }
        let bin_arr = states[0]
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or_else(|| {
                datafusion::error::DataFusionError::Execution(
                    "realized_volatility_parkinson state must be BinaryArray".into(),
                )
            })?;
        for i in 0..bin_arr.len() {
            if !bin_arr.is_null(i) {
                self.merge_from_bytes(bin_arr.value(i))?;
            }
        }
        Ok(())
    }
}
