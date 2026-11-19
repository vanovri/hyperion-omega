use std::any::Any;
use std::sync::Arc;

use arrow::array::{Array, Int64Array, Int64Builder};
use arrow::datatypes::DataType;
use datafusion::common::{exec_err, Result as DfResult, ScalarValue};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, Volatility,
};

/// Vectorized Scalar UDF `time_bucket_ns(timestamp_ns, bucket_width_ns) -> Int64`.
/// Floors nanosecond timestamps to aligned fixed-width intervals directly over Arrow `Int64Array` buffers.
#[derive(Debug)]
pub struct TimeBucketNsUdf {
    signature: Signature,
}

impl Default for TimeBucketNsUdf {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeBucketNsUdf {
    pub fn new() -> Self {
        Self {
            signature: Signature::exact(
                vec![DataType::Int64, DataType::Int64],
                Volatility::Immutable,
            ),
        }
    }

    pub fn into_scalar_udf() -> ScalarUDF {
        ScalarUDF::from(Self::new())
    }
}

impl ScalarUDFImpl for TimeBucketNsUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "time_bucket_ns"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> DfResult<DataType> {
        Ok(DataType::Int64)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> DfResult<ColumnarValue> {
        if args.args.len() != 2 {
            return exec_err!("time_bucket_ns expects 2 arguments (timestamp_ns, bucket_width_ns)");
        }

        match (&args.args[0], &args.args[1]) {
            (
                ColumnarValue::Scalar(ScalarValue::Int64(ts_opt)),
                ColumnarValue::Scalar(ScalarValue::Int64(w_opt)),
            ) => match (ts_opt, w_opt) {
                (Some(ts), Some(w)) => {
                    if *w <= 0 {
                        return exec_err!("time_bucket_ns bucket_width_ns must be > 0");
                    }
                    let bucket = ts.div_euclid(*w) * (*w);
                    Ok(ColumnarValue::Scalar(ScalarValue::Int64(Some(bucket))))
                }
                _ => Ok(ColumnarValue::Scalar(ScalarValue::Int64(None))),
            },
            (
                ColumnarValue::Array(ts_arr_ref),
                ColumnarValue::Scalar(ScalarValue::Int64(Some(width))),
            ) => {
                let w = *width;
                if w <= 0 {
                    return exec_err!("time_bucket_ns bucket_width_ns must be > 0");
                }
                let ts_arr = ts_arr_ref
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .ok_or_else(|| {
                        datafusion::error::DataFusionError::Execution(
                            "time_bucket_ns arg 0 must be Int64Array".into(),
                        )
                    })?;

                if ts_arr.null_count() == 0 {
                    // Pure vectorized slice fast-path (zero null bitmap overhead)
                    let out: Vec<i64> = ts_arr
                        .values()
                        .iter()
                        .map(|&ts| ts.div_euclid(w) * w)
                        .collect();
                    Ok(ColumnarValue::Array(Arc::new(Int64Array::from(out))))
                } else {
                    let mut builder = Int64Builder::with_capacity(ts_arr.len());
                    for i in 0..ts_arr.len() {
                        if ts_arr.is_null(i) {
                            builder.append_null();
                        } else {
                            let ts = ts_arr.value(i);
                            builder.append_value(ts.div_euclid(w) * w);
                        }
                    }
                    Ok(ColumnarValue::Array(Arc::new(builder.finish())))
                }
            }
            _ => {
                let arrays = ColumnarValue::values_to_arrays(&args.args)?;
                let ts_arr = arrays[0]
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .ok_or_else(|| {
                        datafusion::error::DataFusionError::Execution(
                            "time_bucket_ns arg 0 must be Int64Array".into(),
                        )
                    })?;
                let w_arr = arrays[1]
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .ok_or_else(|| {
                        datafusion::error::DataFusionError::Execution(
                            "time_bucket_ns arg 1 must be Int64Array".into(),
                        )
                    })?;

                let mut builder = Int64Builder::with_capacity(ts_arr.len());
                for i in 0..ts_arr.len() {
                    if ts_arr.is_null(i) || w_arr.is_null(i) {
                        builder.append_null();
                    } else {
                        let w = w_arr.value(i);
                        if w <= 0 {
                            return exec_err!("time_bucket_ns bucket_width_ns must be > 0");
                        }
                        let ts = ts_arr.value(i);
                        builder.append_value(ts.div_euclid(w) * w);
                    }
                }
                Ok(ColumnarValue::Array(Arc::new(builder.finish())))
            }
        }
    }
}
