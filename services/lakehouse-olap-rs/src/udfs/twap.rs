use std::any::Any;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BinaryArray, BinaryBuilder, BooleanArray, Int64Array, Int64Builder,
};
use arrow::datatypes::{DataType, Field, FieldRef};
use datafusion::common::{exec_err, Result as DfResult, ScalarValue};
use datafusion::logical_expr::function::{AccumulatorArgs, StateFieldsArgs};
use datafusion::logical_expr::{
    Accumulator, AggregateUDF, AggregateUDFImpl, EmitTo, GroupsAccumulator, Signature, Volatility,
};

use crate::math_fp::{evaluate_twap_segments, TwapSegment};

const SEGMENT_BYTES: usize = 72;

#[derive(Debug)]
pub struct TwapPreciseUdaf {
    signature: Signature,
}

impl Default for TwapPreciseUdaf {
    fn default() -> Self {
        Self::new()
    }
}

impl TwapPreciseUdaf {
    pub fn new() -> Self {
        Self {
            signature: Signature::exact(
                vec![DataType::Int64, DataType::Int64],
                Volatility::Immutable,
            ),
        }
    }

    pub fn into_aggregate_udf() -> AggregateUDF {
        AggregateUDF::from(Self::new())
    }
}

impl AggregateUDFImpl for TwapPreciseUdaf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "twap_precise"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> DfResult<DataType> {
        Ok(DataType::Int64)
    }

    fn state_fields(&self, _args: StateFieldsArgs) -> DfResult<Vec<FieldRef>> {
        Ok(vec![Arc::new(Field::new(
            "twap_segments_bin",
            DataType::Binary,
            true,
        ))])
    }

    fn accumulator(&self, _acc_args: AccumulatorArgs) -> DfResult<Box<dyn Accumulator>> {
        Ok(Box::new(TwapAccumulator::default()))
    }

    fn groups_accumulator_supported(&self, _args: AccumulatorArgs) -> bool {
        true
    }

    fn create_groups_accumulator(
        &self,
        _args: AccumulatorArgs,
    ) -> DfResult<Box<dyn GroupsAccumulator>> {
        Ok(Box::new(TwapGroupsAccumulator::default()))
    }
}

#[derive(Debug, Default, Clone)]
struct TwapAccumulator {
    segments: Vec<TwapSegment>,
}

impl TwapAccumulator {
    #[inline]
    fn append_or_coalesce(&mut self, seg: TwapSegment) {
        if let Some(last) = self.segments.last_mut() {
            if seg.first_ts_ns >= last.last_ts_ns {
                let bridge_dt = i128::from(seg.first_ts_ns) - i128::from(last.last_ts_ns);
                last.weighted_sum += i128::from(last.last_price_fp) * bridge_dt + seg.weighted_sum;
                last.simple_sum += seg.simple_sum;
                last.count += seg.count;
                last.last_ts_ns = seg.last_ts_ns;
                last.last_price_fp = seg.last_price_fp;
                return;
            }
        }
        self.segments.push(seg);
    }

    #[inline]
    fn push_point(&mut self, price_fp: i64, ts_ns: i64) {
        if let Some(last) = self.segments.last_mut() {
            if ts_ns >= last.last_ts_ns {
                let dt = i128::from(ts_ns) - i128::from(last.last_ts_ns);
                last.weighted_sum += i128::from(last.last_price_fp) * dt;
                last.simple_sum += i128::from(price_fp);
                last.count += 1;
                last.last_ts_ns = ts_ns;
                last.last_price_fp = price_fp;
                return;
            }
        }
        self.segments.push(TwapSegment {
            first_ts_ns: ts_ns,
            first_price_fp: price_fp,
            last_ts_ns: ts_ns,
            last_price_fp: price_fp,
            weighted_sum: 0,
            simple_sum: i128::from(price_fp),
            count: 1,
        });
    }

    fn encode_segments(&self) -> Vec<u8> {
        let count = self.segments.len() as u32;
        let mut buf = Vec::with_capacity(4 + self.segments.len() * SEGMENT_BYTES);
        buf.extend_from_slice(&count.to_le_bytes());
        for s in &self.segments {
            buf.extend_from_slice(&s.first_ts_ns.to_le_bytes());
            buf.extend_from_slice(&s.first_price_fp.to_le_bytes());
            buf.extend_from_slice(&s.last_ts_ns.to_le_bytes());
            buf.extend_from_slice(&s.last_price_fp.to_le_bytes());
            buf.extend_from_slice(&s.weighted_sum.to_le_bytes());
            buf.extend_from_slice(&s.simple_sum.to_le_bytes());
            buf.extend_from_slice(&s.count.to_le_bytes());
        }
        buf
    }

    fn decode_and_merge(&mut self, raw: &[u8]) -> DfResult<()> {
        if raw.len() < 4 {
            return Ok(());
        }
        let seg_count = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
        let expected_len = 4 + seg_count * SEGMENT_BYTES;
        if raw.len() != expected_len {
            return exec_err!("Corrupted TwapAccumulator binary state");
        }
        let mut off = 4;
        for _ in 0..seg_count {
            let s = &raw[off..off + SEGMENT_BYTES];
            let first_ts_ns = i64::from_le_bytes(s[0..8].try_into().unwrap());
            let first_price_fp = i64::from_le_bytes(s[8..16].try_into().unwrap());
            let last_ts_ns = i64::from_le_bytes(s[16..24].try_into().unwrap());
            let last_price_fp = i64::from_le_bytes(s[24..32].try_into().unwrap());
            let weighted_sum = i128::from_le_bytes(s[32..48].try_into().unwrap());
            let simple_sum = i128::from_le_bytes(s[48..64].try_into().unwrap());
            let count = u64::from_le_bytes(s[64..72].try_into().unwrap());
            self.append_or_coalesce(TwapSegment {
                first_ts_ns,
                first_price_fp,
                last_ts_ns,
                last_price_fp,
                weighted_sum,
                simple_sum,
                count,
            });
            off += SEGMENT_BYTES;
        }
        Ok(())
    }
}

impl Accumulator for TwapAccumulator {
    fn update_batch(&mut self, values: &[ArrayRef]) -> DfResult<()> {
        if values.len() != 2 {
            return exec_err!("twap_precise requires (price_fp, timestamp_ns)");
        }
        let prices = values[0]
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| {
                datafusion::error::DataFusionError::Execution(
                    "twap_precise arg 0 must be Int64Array".into(),
                )
            })?;
        let timestamps = values[1]
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| {
                datafusion::error::DataFusionError::Execution(
                    "twap_precise arg 1 must be Int64Array".into(),
                )
            })?;

        let n = prices.len();
        if n == 0 {
            return Ok(());
        }

        if prices.null_count() == 0 && timestamps.null_count() == 0 {
            if let Some(seg) =
                TwapSegment::try_from_monotonic_slices(prices.values(), timestamps.values())
            {
                self.append_or_coalesce(seg);
                return Ok(());
            }
        }

        let mut pairs = Vec::with_capacity(n);
        for i in 0..n {
            if !prices.is_null(i) && !timestamps.is_null(i) {
                pairs.push((timestamps.value(i), prices.value(i)));
            }
        }
        if let Some(seg) = TwapSegment::from_pairs(&mut pairs) {
            self.append_or_coalesce(seg);
        }
        Ok(())
    }

    fn evaluate(&mut self) -> DfResult<ScalarValue> {
        let mut cloned = self.segments.clone();
        Ok(ScalarValue::Int64(evaluate_twap_segments(&mut cloned)))
    }

    fn size(&self) -> usize {
        std::mem::size_of_val(self) + self.segments.capacity() * std::mem::size_of::<TwapSegment>()
    }

    fn state(&mut self) -> DfResult<Vec<ScalarValue>> {
        if self.segments.is_empty() {
            Ok(vec![ScalarValue::Binary(None)])
        } else {
            Ok(vec![ScalarValue::Binary(Some(self.encode_segments()))])
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
                    "twap_precise state must be BinaryArray".into(),
                )
            })?;
        for i in 0..bin_arr.len() {
            if !bin_arr.is_null(i) {
                self.decode_and_merge(bin_arr.value(i))?;
            }
        }
        Ok(())
    }
}

/// Native vectorized `GroupsAccumulator` for `twap_precise` bypassing DataFusion's
/// slow `GroupsAccumulatorAdapter` (zero per-batch `take` slicing, direct L1-resident slice scan!).
#[derive(Debug, Default)]
struct TwapGroupsAccumulator {
    groups: Vec<TwapAccumulator>,
}

impl GroupsAccumulator for TwapGroupsAccumulator {
    fn update_batch(
        &mut self,
        values: &[ArrayRef],
        group_indices: &[usize],
        opt_filter: Option<&BooleanArray>,
        total_num_groups: usize,
    ) -> DfResult<()> {
        if self.groups.len() < total_num_groups {
            self.groups
                .resize_with(total_num_groups, TwapAccumulator::default);
        }
        if group_indices.is_empty() {
            return Ok(());
        }

        let prices = values[0]
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| {
                datafusion::error::DataFusionError::Execution(
                    "twap_precise arg 0 must be Int64Array".into(),
                )
            })?;
        let timestamps = values[1]
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| {
                datafusion::error::DataFusionError::Execution(
                    "twap_precise arg 1 must be Int64Array".into(),
                )
            })?;

        let n = group_indices.len();
        let no_nulls = prices.null_count() == 0 && timestamps.null_count() == 0;

        // Ultra-fast single-group monotonic slice path (hits 100% of Hive symbol-partitioned scans!)
        if opt_filter.is_none() && no_nulls {
            let g0 = group_indices[0];
            if group_indices.iter().all(|&g| g == g0) {
                if let Some(seg) =
                    TwapSegment::try_from_monotonic_slices(prices.values(), timestamps.values())
                {
                    self.groups[g0].append_or_coalesce(seg);
                    return Ok(());
                }
            }

            let p_s = prices.values();
            let t_s = timestamps.values();
            for i in 0..n {
                self.groups[group_indices[i]].push_point(p_s[i], t_s[i]);
            }
            return Ok(());
        }

        for (i, &g) in group_indices.iter().enumerate() {
            if let Some(f) = opt_filter {
                if f.is_null(i) || !f.value(i) {
                    continue;
                }
            }
            if prices.is_null(i) || timestamps.is_null(i) {
                continue;
            }
            self.groups[g].push_point(prices.value(i), timestamps.value(i));
        }
        Ok(())
    }

    fn evaluate(&mut self, emit_to: EmitTo) -> DfResult<ArrayRef> {
        let taken: Vec<TwapAccumulator> = match emit_to {
            EmitTo::All => std::mem::take(&mut self.groups),
            EmitTo::First(n) => self.groups.drain(..n).collect(),
        };
        let mut builder = Int64Builder::with_capacity(taken.len());
        for mut acc in taken {
            match evaluate_twap_segments(&mut acc.segments) {
                Some(v) => builder.append_value(v),
                None => builder.append_null(),
            }
        }
        Ok(Arc::new(builder.finish()))
    }

    fn state(&mut self, emit_to: EmitTo) -> DfResult<Vec<ArrayRef>> {
        let taken: Vec<TwapAccumulator> = match emit_to {
            EmitTo::All => std::mem::take(&mut self.groups),
            EmitTo::First(n) => self.groups.drain(..n).collect(),
        };
        let mut builder = BinaryBuilder::with_capacity(taken.len(), taken.len() * (SEGMENT_BYTES + 4));
        for acc in taken {
            if acc.segments.is_empty() {
                builder.append_null();
            } else {
                builder.append_value(acc.encode_segments());
            }
        }
        Ok(vec![Arc::new(builder.finish())])
    }

    fn merge_batch(
        &mut self,
        values: &[ArrayRef],
        group_indices: &[usize],
        opt_filter: Option<&BooleanArray>,
        total_num_groups: usize,
    ) -> DfResult<()> {
        if self.groups.len() < total_num_groups {
            self.groups
                .resize_with(total_num_groups, TwapAccumulator::default);
        }
        let bin_arr = values[0]
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or_else(|| {
                datafusion::error::DataFusionError::Execution(
                    "twap_precise groups state must be BinaryArray".into(),
                )
            })?;
        for (i, &g) in group_indices.iter().enumerate() {
            if let Some(f) = opt_filter {
                if f.is_null(i) || !f.value(i) {
                    continue;
                }
            }
            if !bin_arr.is_null(i) {
                self.groups[g].decode_and_merge(bin_arr.value(i))?;
            }
        }
        Ok(())
    }

    fn size(&self) -> usize {
        std::mem::size_of_val(self)
            + self.groups.capacity() * std::mem::size_of::<TwapAccumulator>()
    }
}
