use std::any::Any;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, BinaryArray, Int64Array};
use arrow::datatypes::{i256, DataType, Field, FieldRef};
use datafusion::common::{exec_err, Result as DfResult, ScalarValue};
use datafusion::logical_expr::function::{AccumulatorArgs, StateFieldsArgs};
use datafusion::logical_expr::{
    Accumulator, AggregateUDF, AggregateUDFImpl, Signature, TypeSignature, Volatility,
};

use crate::math_fp::KylesLambdaState;

// count(8) + sum_v(16) + sum_dp(16) + sum_vv(32) + sum_v_dp(32) + has_price_chain(1) + first_price(8) + first_signed_qty(8) + last_price(8) = 129 bytes
const KYLES_STATE_BYTES: usize = 129;

#[derive(Debug)]
pub struct KylesLambdaSlopeUdaf {
    signature: Signature,
}

impl Default for KylesLambdaSlopeUdaf {
    fn default() -> Self {
        Self::new()
    }
}

impl KylesLambdaSlopeUdaf {
    pub fn new() -> Self {
        Self {
            signature: Signature::one_of(
                vec![
                    // 2-arg: kyles_lambda_slope(delta_price_fp, signed_qty_fp)
                    TypeSignature::Exact(vec![DataType::Int64, DataType::Int64]),
                    // 3-arg tick stream: kyles_lambda_slope(price_fp, qty_fp, side_sign)
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

impl AggregateUDFImpl for KylesLambdaSlopeUdaf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "kyles_lambda_slope"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> DfResult<DataType> {
        Ok(DataType::Int64)
    }

    fn state_fields(&self, _args: StateFieldsArgs) -> DfResult<Vec<FieldRef>> {
        Ok(vec![Arc::new(Field::new(
            "kyles_lambda_state_bin",
            DataType::Binary,
            true,
        ))])
    }

    fn accumulator(&self, _acc_args: AccumulatorArgs) -> DfResult<Box<dyn Accumulator>> {
        Ok(Box::new(KylesLambdaAccumulator::default()))
    }
}

#[derive(Debug, Default)]
struct KylesLambdaAccumulator {
    ols: KylesLambdaState,
    has_price_chain: bool,
    first_price_fp: i64,
    first_signed_qty_fp: i64,
    last_price_fp: i64,
}

impl KylesLambdaAccumulator {
    fn to_bytes(&self) -> [u8; KYLES_STATE_BYTES] {
        let mut b = [0_u8; KYLES_STATE_BYTES];
        b[0..8].copy_from_slice(&self.ols.count.to_le_bytes());
        b[8..24].copy_from_slice(&self.ols.sum_v.to_le_bytes());
        b[24..40].copy_from_slice(&self.ols.sum_dp.to_le_bytes());
        b[40..72].copy_from_slice(&self.ols.sum_vv.to_le_bytes());
        b[72..104].copy_from_slice(&self.ols.sum_v_dp.to_le_bytes());
        b[104] = u8::from(self.has_price_chain);
        b[105..113].copy_from_slice(&self.first_price_fp.to_le_bytes());
        b[113..121].copy_from_slice(&self.first_signed_qty_fp.to_le_bytes());
        b[121..129].copy_from_slice(&self.last_price_fp.to_le_bytes());
        b
    }

    fn merge_from_bytes(&mut self, raw: &[u8]) -> DfResult<()> {
        if raw.len() != KYLES_STATE_BYTES {
            return exec_err!("Invalid KylesLambdaAccumulator binary state");
        }
        let other_ols = KylesLambdaState {
            count: u64::from_le_bytes(raw[0..8].try_into().unwrap()),
            sum_v: i128::from_le_bytes(raw[8..24].try_into().unwrap()),
            sum_dp: i128::from_le_bytes(raw[24..40].try_into().unwrap()),
            sum_vv: i256::from_le_bytes(raw[40..72].try_into().unwrap()),
            sum_v_dp: i256::from_le_bytes(raw[72..104].try_into().unwrap()),
        };
        let other_has_chain = raw[104] != 0;
        let other_first_p = i64::from_le_bytes(raw[105..113].try_into().unwrap());
        let other_first_sq = i64::from_le_bytes(raw[113..121].try_into().unwrap());
        let other_last_p = i64::from_le_bytes(raw[121..129].try_into().unwrap());

        self.ols.merge(&other_ols);
        if other_has_chain {
            if !self.has_price_chain {
                self.has_price_chain = true;
                self.first_price_fp = other_first_p;
                self.first_signed_qty_fp = other_first_sq;
                self.last_price_fp = other_last_p;
            } else {
                // Bridge partition boundary delta
                let bridge_dp = other_first_p.saturating_sub(self.last_price_fp);
                self.ols.update_pair(bridge_dp, other_first_sq);
                self.last_price_fp = other_last_p;
            }
        }
        Ok(())
    }
}

impl Accumulator for KylesLambdaAccumulator {
    fn update_batch(&mut self, values: &[ArrayRef]) -> DfResult<()> {
        if values.len() == 2 {
            let dp_arr = values[0]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "kyles_lambda_slope arg 0 must be Int64Array".into(),
                    )
                })?;
            let sv_arr = values[1]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "kyles_lambda_slope arg 1 must be Int64Array".into(),
                    )
                })?;

            let n = dp_arr.len();
            if dp_arr.null_count() == 0 && sv_arr.null_count() == 0 {
                let dp_s = dp_arr.values();
                let sv_s = sv_arr.values();
                for i in 0..n {
                    self.ols.update_pair(dp_s[i], sv_s[i]);
                }
            } else {
                for i in 0..n {
                    if !dp_arr.is_null(i) && !sv_arr.is_null(i) {
                        self.ols.update_pair(dp_arr.value(i), sv_arr.value(i));
                    }
                }
            }
            Ok(())
        } else if values.len() == 3 {
            let p_arr = values[0]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "kyles_lambda_slope arg 0 must be Int64Array".into(),
                    )
                })?;
            let q_arr = values[1]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "kyles_lambda_slope arg 1 must be Int64Array".into(),
                    )
                })?;
            let s_arr = values[2]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    datafusion::error::DataFusionError::Execution(
                        "kyles_lambda_slope arg 2 must be Int64Array".into(),
                    )
                })?;

            let n = p_arr.len();
            if n == 0 {
                return Ok(());
            }

            if p_arr.null_count() == 0 && q_arr.null_count() == 0 && s_arr.null_count() == 0 {
                let p_s = p_arr.values();
                let q_s = q_arr.values();
                let s_s = s_arr.values();

                let mut start_idx = 0;
                if !self.has_price_chain {
                    self.has_price_chain = true;
                    self.first_price_fp = p_s[0];
                    self.first_signed_qty_fp = q_s[0].saturating_mul(if s_s[0] >= 0 { 1 } else { -1 });
                    self.last_price_fp = p_s[0];
                    start_idx = 1;
                }

                for i in start_idx..n {
                    let dp = p_s[i].saturating_sub(self.last_price_fp);
                    let sv = q_s[i].saturating_mul(if s_s[i] >= 0 { 1 } else { -1 });
                    self.ols.update_pair(dp, sv);
                    self.last_price_fp = p_s[i];
                }
            } else {
                for i in 0..n {
                    if p_arr.is_null(i) || q_arr.is_null(i) || s_arr.is_null(i) {
                        continue;
                    }
                    let p = p_arr.value(i);
                    let sv = q_arr.value(i).saturating_mul(if s_arr.value(i) >= 0 { 1 } else { -1 });
                    if !self.has_price_chain {
                        self.has_price_chain = true;
                        self.first_price_fp = p;
                        self.first_signed_qty_fp = sv;
                        self.last_price_fp = p;
                    } else {
                        let dp = p.saturating_sub(self.last_price_fp);
                        self.ols.update_pair(dp, sv);
                        self.last_price_fp = p;
                    }
                }
            }
            Ok(())
        } else {
            exec_err!("kyles_lambda_slope expects 2 or 3 arguments")
        }
    }

    fn evaluate(&mut self) -> DfResult<ScalarValue> {
        Ok(ScalarValue::Int64(self.ols.evaluate_fp()))
    }

    fn size(&self) -> usize {
        std::mem::size_of_val(self)
    }

    fn state(&mut self) -> DfResult<Vec<ScalarValue>> {
        if self.ols.count == 0 && !self.has_price_chain {
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
                    "kyles_lambda_slope state must be BinaryArray".into(),
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
