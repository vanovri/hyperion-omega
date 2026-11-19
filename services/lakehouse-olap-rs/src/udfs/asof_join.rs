use std::any::Any;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, DictionaryArray, Int64Array, RecordBatch, StringArray,
    UInt32Array, UInt32Builder, UInt64Array,
};
use arrow::compute::{concat_batches, filter_record_batch, lexsort_to_indices, take, take_record_batch, SortColumn, SortOptions};
use arrow::datatypes::{Field, Int8Type, Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::common::{exec_err, DFSchema, DFSchemaRef, Result as DfResult, TableReference};
use datafusion::execution::{SendableRecordBatchStream, SessionState, TaskContext};
use datafusion::logical_expr::{Expr, LogicalPlan, UserDefinedLogicalNode, UserDefinedLogicalNodeCore};
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, ExecutionPlanProperties, PlanProperties,
};
use datafusion::physical_planner::{ExtensionPlanner, PhysicalPlanner};
use futures::{StreamExt, TryStreamExt};
use tokio::sync::OnceCell;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum AsofKeyVal {
    Int(i128),
    Str(String),
}

/// Logical extension node representing a columnar `ASOF [LEFT] JOIN` between
/// a stream of market events (`trades` / `fraud_alerts`) and L2 order book snapshots (`quotes`).
#[derive(Debug, Clone)]
pub struct AsofJoinNode {
    pub left: LogicalPlan,
    pub right: LogicalPlan,
    pub left_key_col: String,
    pub right_key_col: String,
    pub left_ts_col: String,
    pub right_ts_col: String,
    /// When `true`, requires `right_ts < left_ts`; when `false`, `right_ts <= left_ts` (nanosecond ASOF).
    pub strict_lt: bool,
    /// When `true`, preserves unmatched left rows with NULL right columns (`ASOF LEFT JOIN`).
    pub is_left_join: bool,
    pub output_schema: DFSchemaRef,
}

impl AsofJoinNode {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        left: LogicalPlan,
        right: LogicalPlan,
        left_key_col: impl Into<String>,
        right_key_col: impl Into<String>,
        left_ts_col: impl Into<String>,
        right_ts_col: impl Into<String>,
        strict_lt: bool,
        is_left_join: bool,
    ) -> DfResult<Self> {
        let mut qualified_fields: Vec<(Option<TableReference>, Arc<Field>)> = Vec::with_capacity(
            left.schema().fields().len() + right.schema().fields().len(),
        );
        for (_qual, field) in left.schema().iter() {
            qualified_fields.push((None, Arc::clone(field)));
        }
        for (_qual, field) in right.schema().iter() {
            let nullable_field = if field.is_nullable() {
                Arc::clone(field)
            } else {
                Arc::new(field.as_ref().clone().with_nullable(true))
            };
            qualified_fields.push((None, nullable_field));
        }

        let mut metadata = left.schema().metadata().clone();
        for (k, v) in right.schema().metadata() {
            metadata.entry(k.clone()).or_insert_with(|| v.clone());
        }
        let output_schema = Arc::new(DFSchema::new_with_metadata(qualified_fields, metadata)?);

        Ok(Self {
            left,
            right,
            left_key_col: left_key_col.into(),
            right_key_col: right_key_col.into(),
            left_ts_col: left_ts_col.into(),
            right_ts_col: right_ts_col.into(),
            strict_lt,
            is_left_join,
            output_schema,
        })
    }
}

impl PartialEq for AsofJoinNode {
    fn eq(&self, other: &Self) -> bool {
        self.left == other.left
            && self.right == other.right
            && self.left_key_col == other.left_key_col
            && self.right_key_col == other.right_key_col
            && self.left_ts_col == other.left_ts_col
            && self.right_ts_col == other.right_ts_col
            && self.strict_lt == other.strict_lt
            && self.is_left_join == other.is_left_join
    }
}

impl Eq for AsofJoinNode {}

impl PartialOrd for AsofJoinNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match self.left_key_col.partial_cmp(&other.left_key_col) {
            Some(Ordering::Equal) => self.left_ts_col.partial_cmp(&other.left_ts_col),
            ord => ord,
        }
    }
}

impl Hash for AsofJoinNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.left.hash(state);
        self.right.hash(state);
        self.left_key_col.hash(state);
        self.right_key_col.hash(state);
        self.left_ts_col.hash(state);
        self.right_ts_col.hash(state);
        self.strict_lt.hash(state);
        self.is_left_join.hash(state);
    }
}

impl UserDefinedLogicalNodeCore for AsofJoinNode {
    fn name(&self) -> &str {
        "AsofJoin"
    }

    fn inputs(&self) -> Vec<&LogicalPlan> {
        vec![&self.left, &self.right]
    }

    fn schema(&self) -> &DFSchemaRef {
        &self.output_schema
    }

    fn expressions(&self) -> Vec<Expr> {
        Vec::new()
    }

    fn prevent_predicate_push_down_columns(&self) -> HashSet<String> {
        // Prevent generic DataFusion PushDownFilter from duplicating left-only predicates
        // onto the right ASOF child (which has disambiguated column names).
        self.output_schema
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    fn fmt_for_explain(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mode = if self.is_left_join { "Left" } else { "Inner" };
        let cmp = if self.strict_lt { ">" } else { ">=" };
        write!(
            f,
            "AsofJoin: mode={mode}, on=({} = {}), asof=({} {cmp} {})",
            self.left_key_col, self.right_key_col, self.left_ts_col, self.right_ts_col
        )
    }

    fn with_exprs_and_inputs(
        &self,
        _exprs: Vec<Expr>,
        mut inputs: Vec<LogicalPlan>,
    ) -> DfResult<Self> {
        if inputs.len() != 2 {
            return exec_err!("AsofJoinNode requires exactly 2 inputs");
        }
        let right = inputs.pop().unwrap();
        let left = inputs.pop().unwrap();
        Self::try_new(
            left,
            right,
            self.left_key_col.clone(),
            self.right_key_col.clone(),
            self.left_ts_col.clone(),
            self.right_ts_col.clone(),
            self.strict_lt,
            self.is_left_join,
        )
    }
}

/// DataFusion `ExtensionPlanner` lowering `AsofJoinNode` -> `AsofJoinExec`.
#[derive(Debug, Default)]
pub struct AsofJoinPhysicalPlanner;

#[async_trait]
impl ExtensionPlanner for AsofJoinPhysicalPlanner {
    async fn plan_extension(
        &self,
        _planner: &dyn PhysicalPlanner,
        node: &dyn UserDefinedLogicalNode,
        _logical_inputs: &[&LogicalPlan],
        physical_inputs: &[Arc<dyn ExecutionPlan>],
        _session_state: &SessionState,
    ) -> DfResult<Option<Arc<dyn ExecutionPlan>>> {
        let Some(asof_node) = node.as_any().downcast_ref::<AsofJoinNode>() else {
            return Ok(None);
        };
        if physical_inputs.len() != 2 {
            return exec_err!("AsofJoinPhysicalPlanner expected 2 physical inputs");
        }

        let exec = AsofJoinExec::try_new(
            Arc::clone(&physical_inputs[0]),
            Arc::clone(&physical_inputs[1]),
            asof_node.left_key_col.clone(),
            asof_node.right_key_col.clone(),
            asof_node.left_ts_col.clone(),
            asof_node.right_ts_col.clone(),
            asof_node.strict_lt,
            asof_node.is_left_join,
        )?;
        Ok(Some(Arc::new(exec)))
    }
}

#[derive(Debug)]
struct KeySliceMeta {
    global_offset: u32,
    timestamps_ns: Vec<i64>,
}

#[derive(Debug)]
struct IndexedRightSide {
    combined_batch: Option<RecordBatch>,
    by_key: BTreeMap<AsofKeyVal, KeySliceMeta>,
}

/// Columnar physical execution operator `AsofJoinExec` performing nanosecond-exact
/// backward ASOF alignment over contiguous Arrow slices (`&[i64]`) + vectorized `take`.
#[derive(Debug)]
pub struct AsofJoinExec {
    left: Arc<dyn ExecutionPlan>,
    right: Arc<dyn ExecutionPlan>,
    left_key_idx: usize,
    right_key_idx: usize,
    left_ts_idx: usize,
    right_ts_idx: usize,
    left_key_col: String,
    right_key_col: String,
    left_ts_col: String,
    right_ts_col: String,
    strict_lt: bool,
    is_left_join: bool,
    output_schema: SchemaRef,
    properties: PlanProperties,
    right_cache: Arc<OnceCell<Arc<IndexedRightSide>>>,
}

impl AsofJoinExec {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        left: Arc<dyn ExecutionPlan>,
        right: Arc<dyn ExecutionPlan>,
        left_key_col: String,
        right_key_col: String,
        left_ts_col: String,
        right_ts_col: String,
        strict_lt: bool,
        is_left_join: bool,
    ) -> DfResult<Self> {
        let left_schema = left.schema();
        let right_schema = right.schema();

        let left_key_idx = find_col_index(&left_schema, &left_key_col)?;
        let right_key_idx = find_col_index(&right_schema, &right_key_col)?;
        let left_ts_idx = find_col_index(&left_schema, &left_ts_col)?;
        let right_ts_idx = find_col_index(&right_schema, &right_ts_col)?;

        let mut fields: Vec<Field> =
            Vec::with_capacity(left_schema.fields().len() + right_schema.fields().len());
        for f in left_schema.fields() {
            fields.push(f.as_ref().clone());
        }
        for f in right_schema.fields() {
            fields.push(f.as_ref().clone().with_nullable(true));
        }
        let output_schema = Arc::new(Schema::new(fields));

        let properties = PlanProperties::new(
            EquivalenceProperties::new(Arc::clone(&output_schema)),
            left.output_partitioning().clone(),
            EmissionType::Incremental,
            Boundedness::Bounded,
        );

        Ok(Self {
            left,
            right,
            left_key_idx,
            right_key_idx,
            left_ts_idx,
            right_ts_idx,
            left_key_col,
            right_key_col,
            left_ts_col,
            right_ts_col,
            strict_lt,
            is_left_join,
            output_schema,
            properties,
            right_cache: Arc::new(OnceCell::new()),
        })
    }
}

fn find_col_index(schema: &SchemaRef, col_name: &str) -> DfResult<usize> {
    let bare = col_name.rsplit('.').next().unwrap_or(col_name);
    schema
        .index_of(bare)
        .map_err(|_| datafusion::error::DataFusionError::Plan(format!("Column '{col_name}' not found in schema")))
}

#[inline]
fn extract_key_at(arr: &ArrayRef, row: usize) -> Option<AsofKeyVal> {
    if arr.is_null(row) {
        return None;
    }
    if let Some(a) = arr.as_any().downcast_ref::<UInt32Array>() {
        Some(AsofKeyVal::Int(i128::from(a.value(row))))
    } else if let Some(a) = arr.as_any().downcast_ref::<UInt64Array>() {
        Some(AsofKeyVal::Int(i128::from(a.value(row))))
    } else if let Some(a) = arr.as_any().downcast_ref::<Int64Array>() {
        Some(AsofKeyVal::Int(i128::from(a.value(row))))
    } else if let Some(a) = arr.as_any().downcast_ref::<StringArray>() {
        Some(AsofKeyVal::Str(a.value(row).to_string()))
    } else if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<Int8Type>>() {
        let vals = d.values().as_any().downcast_ref::<StringArray>()?;
        let k = d.keys().value(row) as usize;
        Some(AsofKeyVal::Str(vals.value(k).to_string()))
    } else {
        None
    }
}

async fn build_right_index(
    right_plan: Arc<dyn ExecutionPlan>,
    right_key_idx: usize,
    right_ts_idx: usize,
    ctx: Arc<TaskContext>,
) -> DfResult<Arc<IndexedRightSide>> {
    let part_count = right_plan.output_partitioning().partition_count();
    let mut right_batches: Vec<RecordBatch> = Vec::new();
    for p in 0..part_count {
        let stream = right_plan.execute(p, Arc::clone(&ctx))?;
        let collected: Vec<RecordBatch> = stream.try_collect().await?;
        for b in collected {
            if b.num_rows() > 0 {
                right_batches.push(b);
            }
        }
    }

    if right_batches.is_empty() {
        return Ok(Arc::new(IndexedRightSide {
            combined_batch: None,
            by_key: BTreeMap::new(),
        }));
    }

    let right_schema = right_plan.schema();
    let concat = concat_batches(&right_schema, &right_batches)?;

    // Sort globally by (right_key ASC, right_ts ASC) via vectorized Arrow lexsort
    let opts = Some(SortOptions {
        descending: false,
        nulls_first: false,
    });
    let indices = lexsort_to_indices(
        &[
            SortColumn {
                values: Arc::clone(concat.column(right_key_idx)),
                options: opts,
            },
            SortColumn {
                values: Arc::clone(concat.column(right_ts_idx)),
                options: opts,
            },
        ],
        None,
    )?;
    let sorted = take_record_batch(&concat, &indices)?;

    let key_arr = sorted.column(right_key_idx);
    let ts_arr = sorted
        .column(right_ts_idx)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| {
            datafusion::error::DataFusionError::Execution(
                "ASOF right timestamp column must be Int64".into(),
            )
        })?;
    let ts_slice = ts_arr.values();

    let mut by_key: BTreeMap<AsofKeyVal, KeySliceMeta> = BTreeMap::new();
    let total = sorted.num_rows();
    let mut i = 0_usize;
    while i < total {
        let Some(k) = extract_key_at(key_arr, i) else {
            i += 1;
            continue;
        };
        let start = i;
        i += 1;
        while i < total {
            if extract_key_at(key_arr, i).as_ref() != Some(&k) {
                break;
            }
            i += 1;
        }
        by_key.insert(
            k,
            KeySliceMeta {
                global_offset: start as u32,
                timestamps_ns: ts_slice[start..i].to_vec(),
            },
        );
    }

    Ok(Arc::new(IndexedRightSide {
        combined_batch: Some(sorted),
        by_key,
    }))
}

/// Pass 2 (Vectorized Binary Search) ASOF index resolver over sorted `right_ts` slice.
#[inline]
pub fn asof_locate_binary(right_ts: &[i64], l_ts: i64, strict_lt: bool) -> Option<usize> {
    let pos = if strict_lt {
        right_ts.partition_point(|&rts| rts < l_ts)
    } else {
        right_ts.partition_point(|&rts| rts <= l_ts)
    };
    if pos > 0 {
        Some(pos - 1)
    } else {
        None
    }
}

/// Pass 3 (Cache-Line Resident Monotonic + Galloping Cursor) ASOF index resolver.
/// Exploits composite `(instrument_id, timestamp_ns, seq_id)` sort order to stay in L1 cache!
#[inline]
pub fn asof_locate_galloping_cursor(
    right_ts: &[i64],
    l_ts: i64,
    strict_lt: bool,
    cursor: &mut usize,
) -> Option<usize> {
    let len = right_ts.len();
    if len == 0 {
        return None;
    }
    let satisfies = |rts: i64| if strict_lt { rts < l_ts } else { rts <= l_ts };

    let mut idx = (*cursor).min(len - 1);
    if satisfies(right_ts[idx]) {
        let mut steps = 0_usize;
        while idx + 1 < len && satisfies(right_ts[idx + 1]) && steps < 8 {
            idx += 1;
            steps += 1;
        }
        if idx + 1 < len && satisfies(right_ts[idx + 1]) {
            // Galloping exponential bound before narrowed binary search
            let mut step = 16_usize;
            let mut lo = idx + 1;
            let mut hi = (lo + step).min(len);
            while hi < len && satisfies(right_ts[hi]) {
                lo = hi;
                step <<= 1;
                hi = (lo + step).min(len);
            }
            let rel = right_ts[lo..hi].partition_point(|&rts| satisfies(rts));
            idx = lo + rel - 1;
        }
        *cursor = idx;
        Some(idx)
    } else {
        let pos = right_ts[..idx].partition_point(|&rts| satisfies(rts));
        if pos > 0 {
            *cursor = pos - 1;
            Some(pos - 1)
        } else {
            *cursor = 0;
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn align_batch_vectorized(
    left_batch: &RecordBatch,
    right_idx: &IndexedRightSide,
    output_schema: &SchemaRef,
    right_schema: &SchemaRef,
    left_key_idx: usize,
    left_ts_idx: usize,
    strict_lt: bool,
    is_left_join: bool,
) -> DfResult<RecordBatch> {
    let n = left_batch.num_rows();
    if n == 0 {
        return Ok(RecordBatch::new_empty(Arc::clone(output_schema)));
    }

    let Some(ref right_combined) = right_idx.combined_batch else {
        if !is_left_join {
            return Ok(RecordBatch::new_empty(Arc::clone(output_schema)));
        }
        let mut cols: Vec<ArrayRef> = left_batch.columns().to_vec();
        for f in right_schema.fields() {
            cols.push(arrow::array::new_null_array(f.data_type(), n));
        }
        return Ok(RecordBatch::try_new(Arc::clone(output_schema), cols)?);
    };

    let left_ts_arr = left_batch
        .column(left_ts_idx)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| {
            datafusion::error::DataFusionError::Execution(
                "ASOF left timestamp column must be Int64".into(),
            )
        })?;
    let left_ts_slice = left_ts_arr.values();
    let left_key_arr = left_batch.column(left_key_idx);

    let mut take_builder = UInt32Builder::with_capacity(n);
    let mut matched_mask: Vec<bool> = if is_left_join {
        Vec::new()
    } else {
        Vec::with_capacity(n)
    };
    let mut matched_count = 0_usize;

    // Fast path when left key is UInt32 (e.g. `instrument_id`), avoiding per-row enum/String allocs!
    if let Some(u32_keys) = left_key_arr.as_any().downcast_ref::<UInt32Array>() {
        let key_vals = u32_keys.values();
        let mut last_k = u32::MAX;
        let mut cached_meta: Option<&KeySliceMeta> = None;
        let mut cursor = 0_usize;

        for i in 0..n {
            if u32_keys.is_null(i) || left_ts_arr.is_null(i) {
                take_builder.append_null();
                if !is_left_join {
                    matched_mask.push(false);
                }
                continue;
            }
            let kv = key_vals[i];
            if kv != last_k {
                last_k = kv;
                cached_meta = right_idx.by_key.get(&AsofKeyVal::Int(i128::from(kv)));
                cursor = 0;
            }
            let l_ts = left_ts_slice[i];
            if let Some(meta) = cached_meta {
                if let Some(matched_rel) =
                    asof_locate_galloping_cursor(&meta.timestamps_ns, l_ts, strict_lt, &mut cursor)
                {
                    take_builder.append_value(meta.global_offset + matched_rel as u32);
                    matched_count += 1;
                    if !is_left_join {
                        matched_mask.push(true);
                    }
                    continue;
                }
            }
            take_builder.append_null();
            if !is_left_join {
                matched_mask.push(false);
            }
        }
    } else {
        for i in 0..n {
            if left_ts_arr.is_null(i) {
                take_builder.append_null();
                if !is_left_join {
                    matched_mask.push(false);
                }
                continue;
            }
            let l_ts = left_ts_slice[i];
            if let Some(kv) = extract_key_at(left_key_arr, i) {
                if let Some(meta) = right_idx.by_key.get(&kv) {
                    let pos = if strict_lt {
                        meta.timestamps_ns.partition_point(|&rts| rts < l_ts)
                    } else {
                        meta.timestamps_ns.partition_point(|&rts| rts <= l_ts)
                    };
                    if pos > 0 {
                        take_builder.append_value(meta.global_offset + (pos - 1) as u32);
                        matched_count += 1;
                        if !is_left_join {
                            matched_mask.push(true);
                        }
                        continue;
                    }
                }
            }
            take_builder.append_null();
            if !is_left_join {
                matched_mask.push(false);
            }
        }
    }

    if !is_left_join && matched_count == 0 {
        return Ok(RecordBatch::new_empty(Arc::clone(output_schema)));
    }

    let take_indices: UInt32Array = take_builder.finish();
    let mut out_cols: Vec<ArrayRef> =
        Vec::with_capacity(left_batch.num_columns() + right_combined.num_columns());
    for c in left_batch.columns() {
        out_cols.push(Arc::clone(c));
    }
    for rc in right_combined.columns() {
        let aligned = take(rc.as_ref(), &take_indices, None)?;
        out_cols.push(aligned);
    }

    let joined_batch = RecordBatch::try_new(Arc::clone(output_schema), out_cols)?;
    if is_left_join || matched_count == n {
        Ok(joined_batch)
    } else {
        let mask = BooleanArray::from(matched_mask);
        Ok(filter_record_batch(&joined_batch, &mask)?)
    }
}

impl DisplayAs for AsofJoinExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut fmt::Formatter) -> fmt::Result {
        let mode = if self.is_left_join { "Left" } else { "Inner" };
        let cmp = if self.strict_lt { ">" } else { ">=" };
        write!(
            f,
            "AsofJoinExec: mode={mode}, on=({}={}), asof=({} {cmp} {})",
            self.left_key_col, self.right_key_col, self.left_ts_col, self.right_ts_col
        )
    }
}

impl ExecutionPlan for AsofJoinExec {
    fn name(&self) -> &str {
        "AsofJoinExec"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn properties(&self) -> &PlanProperties {
        &self.properties
    }

    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![&self.left, &self.right]
    }

    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        if children.len() != 2 {
            return exec_err!("AsofJoinExec requires 2 children");
        }
        Ok(Arc::new(Self::try_new(
            Arc::clone(&children[0]),
            Arc::clone(&children[1]),
            self.left_key_col.clone(),
            self.right_key_col.clone(),
            self.left_ts_col.clone(),
            self.right_ts_col.clone(),
            self.strict_lt,
            self.is_left_join,
        )?))
    }

    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> DfResult<SendableRecordBatchStream> {
        let left_plan = Arc::clone(&self.left);
        let right_plan = Arc::clone(&self.right);
        let right_cache = Arc::clone(&self.right_cache);
        let output_schema = Arc::clone(&self.output_schema);
        let right_schema = self.right.schema();
        let left_key_idx = self.left_key_idx;
        let right_key_idx = self.right_key_idx;
        let left_ts_idx = self.left_ts_idx;
        let right_ts_idx = self.right_ts_idx;
        let strict_lt = self.strict_lt;
        let is_left_join = self.is_left_join;

        let stream_schema = Arc::clone(&output_schema);
        let fut = async move {
            let right_indexed = right_cache
                .get_or_try_init(|| {
                    build_right_index(
                        Arc::clone(&right_plan),
                        right_key_idx,
                        right_ts_idx,
                        Arc::clone(&context),
                    )
                })
                .await?
                .clone();

            let left_stream = left_plan.execute(partition, context)?;
            let mapped = left_stream.map(move |batch_res| {
                let batch = batch_res?;
                align_batch_vectorized(
                    &batch,
                    &right_indexed,
                    &output_schema,
                    &right_schema,
                    left_key_idx,
                    left_ts_idx,
                    strict_lt,
                    is_left_join,
                )
            });
            Ok::<_, datafusion::error::DataFusionError>(mapped)
        };

        let flat_stream = futures::stream::once(fut).try_flatten();
        Ok(Box::pin(RecordBatchStreamAdapter::new(
            stream_schema,
            flat_stream,
        )))
    }
}
